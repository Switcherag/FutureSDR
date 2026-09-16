use std::collections::BTreeMap;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use futuresdr::futures::future::Either;
use futuresdr::futures::future::select;
use futuresdr::runtime::BlockId;
use futuresdr::runtime::Flowgraph;
use futuresdr::runtime::FlowgraphHandle;
use futuresdr::runtime::FlowgraphTask;
use futuresdr::runtime::Runtime;
use futuresdr::runtime::RuntimeHandle;
use futuresdr::runtime::TerminatedFlowgraph;
use futuresdr::runtime::Timer;
use futuresdr::runtime::block_on;
use futuresdr::runtime::channel::oneshot;
use futuresdr::runtime::dev::TypedBlockGuard;
use futuresdr::runtime::scheduler::Scheduler;

use crate::bridge::BridgeSink;
use crate::bridge::BridgeSource;
use crate::bridge::Channel;
use crate::bridge::ChannelStats;
use crate::bridge::Pipe;
use crate::bridge::next_generation;
use crate::builder::Blocks;
use crate::builder::build;
use crate::description::Description;
use crate::description::PortDecl;
use crate::items::ItemType;
use crate::items::with_item_type;
use crate::registry::Registry;

/// What happens to the input of a flowgraph while it is being replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// Keep every item. The old flowgraph goes on consuming while the new one
    /// starts; then the input moves over at one item. Nothing is lost or
    /// delivered twice.
    Keep,
    /// Drop what arrives during the replacement. The old flowgraph finishes
    /// what it already took, its input ends once the new one is running, and
    /// the new one starts on items that arrive after that.
    Discard,
}

/// Where the time of a replacement went.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReplaceTimings {
    /// Building the new flowgraph (plugins, blocks, connections, bridges).
    pub build: Duration,
    /// Starting it.
    pub start: Duration,
    /// Moving the inputs over.
    pub switch: Duration,
    /// All of it.
    pub total: Duration,
}

/// A flowgraph that has terminated, with its blocks by name.
pub struct Finished {
    /// The terminated flowgraph.
    pub flowgraph: TerminatedFlowgraph,
    /// Its blocks.
    pub blocks: Blocks,
}

impl Finished {
    /// Typed access to block `name`, whose kernel must be `K`.
    pub fn block<K: 'static>(&self, name: &str) -> Result<TypedBlockGuard<'_, K>> {
        let block = self
            .blocks
            .block_ref::<K>(name)
            .ok_or_else(|| anyhow!("no block '{name}' of type {}", std::any::type_name::<K>()))?;
        Ok(self.flowgraph.block(&block)?)
    }
}

impl std::fmt::Debug for Finished {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Finished")
            .field("blocks", &self.blocks)
            .finish_non_exhaustive()
    }
}

/// A flowgraph that was replaced and is finishing in the background.
///
/// Its outputs keep flowing until it has processed what it took; the
/// replacement's outputs follow.
pub struct Retired {
    name: String,
    blocks: Blocks,
    done: oneshot::Receiver<Result<TerminatedFlowgraph, futuresdr::runtime::Error>>,
}

impl Retired {
    /// Name the flowgraph ran under.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Wait until it has terminated.
    pub async fn wait_async(self) -> Result<Finished> {
        let name = self.name;
        let flowgraph = self
            .done
            .await
            .map_err(|_| anyhow!("flowgraph '{name}' was dropped"))?
            .with_context(|| format!("flowgraph '{name}'"))?;
        Ok(Finished {
            flowgraph,
            blocks: self.blocks,
        })
    }

    /// Blocking form of [`wait_async`](Self::wait_async).
    pub fn wait(self) -> Result<Finished> {
        block_on(self.wait_async())
    }
}

impl std::fmt::Debug for Retired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Retired")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Result of [`Controller::replace`].
#[derive(Debug)]
pub struct Replacement {
    /// The flowgraph that was replaced.
    pub old: Retired,
    /// Where the time went.
    pub timings: ReplaceTimings,
}

struct Managed {
    handle: FlowgraphHandle,
    task: FlowgraphTask,
    blocks: Blocks,
    inputs: Vec<(PortDecl, Arc<dyn Pipe>, u64)>,
    outputs: Vec<(PortDecl, Arc<dyn Pipe>, u64)>,
}

type PortKey = (String, String);

/// Runs flowgraphs built from [`Description`]s, links their ports, and
/// replaces them while the others keep running.
///
/// ```ignore
/// let mut ctrl = Controller::new(registry);
/// ctrl.link("source.samples", "receiver.samples")?;
/// ctrl.spawn("source", Description::from_file("source.toml")?)?;
/// ctrl.spawn("receiver", Description::from_file("rx_a.toml")?)?;
/// // later
/// let done = ctrl.replace("receiver", Description::from_file("rx_b.toml")?, Hold::Keep)?;
/// ```
///
/// Each operation comes in two forms: `spawn_async`, `replace_async`, ...
/// for async code, and `spawn`, `replace`, ... that block the calling
/// thread and must not be called from the runtime's tasks.
///
/// Waiting on the runtime from a blocked thread means that thread has to
/// be woken up, which after an idle period costs more than the replacement
/// itself. For the fastest replacements, drive the controller from a task
/// of its runtime with [`run`](Self::run):
///
/// ```ignore
/// ctrl.run(|mut ctrl| async move {
///     ctrl.replace_async("receiver", rx_b, Hold::Keep).await?;
///     anyhow::Ok(())
/// })?;
/// ```
pub struct Controller {
    runtime: Runtime,
    registry: Registry,
    capacity: usize,
    drain_timeout: Duration,
    flowgraphs: BTreeMap<String, Managed>,
    /// input (flowgraph, port) -> output (flowgraph, port)
    links: HashMap<PortKey, PortKey>,
    /// by output (flowgraph, port)
    channels: HashMap<PortKey, Arc<dyn Pipe>>,
}

fn split_port(what: &str) -> Result<PortKey> {
    let (fg, port) = what
        .split_once('.')
        .ok_or_else(|| anyhow!("'{what}' is not \"flowgraph.port\""))?;
    Ok((fg.to_string(), port.to_string()))
}

fn new_channel(item: ItemType, capacity: usize) -> Arc<dyn Pipe> {
    with_item_type!(item, T => Channel::<T>::new(item, capacity) as Arc<dyn Pipe>)
}

fn add_source(fg: &mut Flowgraph, channel: &Arc<dyn Pipe>, generation: u64) -> Result<BlockId> {
    let item = channel.item();
    with_item_type!(item, T => {
        let channel = channel.clone().as_any().downcast::<Channel<T>>().unwrap();
        Ok(fg.add(BridgeSource::<T>::new(channel, generation))?.id())
    })
}

fn add_sink(fg: &mut Flowgraph, channel: &Arc<dyn Pipe>, generation: u64) -> Result<BlockId> {
    let item = channel.item();
    with_item_type!(item, T => {
        let channel = channel.clone().as_any().downcast::<Channel<T>>().unwrap();
        Ok(fg.add(BridgeSink::<T>::new(channel, generation))?.id())
    })
}

impl Controller {
    /// A controller on a new [`Runtime`].
    pub fn new(registry: Registry) -> Self {
        Self::with_runtime(Runtime::new(), registry)
    }

    /// A controller on `runtime`.
    pub fn with_runtime(runtime: Runtime, registry: Registry) -> Self {
        Self {
            runtime,
            registry,
            capacity: 1 << 20,
            drain_timeout: Duration::from_secs(2),
            flowgraphs: BTreeMap::new(),
            links: HashMap::new(),
            channels: HashMap::new(),
        }
    }

    /// Items a link holds before the oldest are dropped (default 2^20).
    /// Applies to links created afterwards.
    pub fn set_link_capacity(&mut self, items: usize) {
        self.capacity = items.max(1);
    }

    /// How long a replaced flowgraph may take to finish what it took before
    /// it is stopped (default 2 s).
    pub fn set_drain_timeout(&mut self, timeout: Duration) {
        self.drain_timeout = timeout;
    }

    /// The block types available.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The block types available, to load more plugins.
    pub fn registry_mut(&mut self) -> &mut Registry {
        &mut self.registry
    }

    /// The runtime the flowgraphs run on.
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// Run `f` with this controller as a task of its runtime, and block
    /// until it completes. Return the controller from `f` to keep using it.
    pub fn run<F, Fut, T>(self, f: F) -> T
    where
        F: FnOnce(Self) -> Fut,
        Fut: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let scheduler = self.runtime.scheduler().clone();
        block_on(scheduler.spawn(f(self)))
    }

    /// Connect output `from` of one flowgraph to input `to` of another, both
    /// written `"flowgraph.port"`. The input's flowgraph must not be running
    /// yet; each input has at most one link.
    pub fn link(&mut self, from: &str, to: &str) -> Result<()> {
        let from = split_port(from)?;
        let to = split_port(to)?;
        if self.flowgraphs.contains_key(&to.0) {
            bail!("'{}' is running; link its inputs before spawning it", to.0);
        }
        if let Some(old) = self.links.get(&to) {
            bail!("{}.{} is already linked to {}.{}", to.0, to.1, old.0, old.1);
        }
        if let Some((other, _)) = self.links.iter().find(|(_, f)| **f == from) {
            bail!(
                "{}.{} already feeds {}.{}; one output feeds one input \
                 (duplicate the stream inside the flowgraph)",
                from.0,
                from.1,
                other.0,
                other.1
            );
        }
        self.links.insert(to, from);
        Ok(())
    }

    /// Names of the running flowgraphs.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.flowgraphs.keys().map(String::as_str)
    }

    /// Handle to the running flowgraph `name`, for messages.
    pub fn handle(&self, name: &str) -> Option<FlowgraphHandle> {
        self.flowgraphs.get(name).map(|m| m.handle.clone())
    }

    /// Blocks of the running flowgraph `name`.
    pub fn blocks(&self, name: &str) -> Option<&Blocks> {
        self.flowgraphs.get(name).map(|m| &m.blocks)
    }

    /// State of the link leaving output `from` (`"flowgraph.port"`).
    pub fn link_stats(&self, from: &str) -> Option<ChannelStats> {
        self.channels
            .get(&split_port(from).ok()?)
            .map(|c| c.stats())
    }

    /// The channel behind output `(fg, port)`, created on first use.
    fn output_channel(&mut self, fg: &str, port: &str, item: ItemType) -> Result<Arc<dyn Pipe>> {
        let key = (fg.to_string(), port.to_string());
        if let Some(channel) = self.channels.get(&key) {
            if channel.item() != item {
                bail!(
                    "{fg}.{port} carries {}, a linked input expects {item}",
                    channel.item()
                );
            }
            return Ok(channel.clone());
        }
        let channel = new_channel(item, self.capacity);
        self.channels.insert(key, channel.clone());
        Ok(channel)
    }

    /// Build `desc` with bridges on its ports. Bridges are not active yet.
    fn assemble(&mut self, name: &str, desc: &Description) -> Result<(Flowgraph, Assembled)> {
        self.registry.load_all(&desc.plugins)?;
        let built = build(&self.registry, desc)?;
        let mut fg = built.flowgraph;
        let mut inputs = Vec::new();
        for port in &desc.inputs {
            let channel = match self
                .links
                .get(&(name.to_string(), port.name.clone()))
                .cloned()
            {
                Some((from_fg, from_port)) => {
                    self.output_channel(&from_fg, &from_port, port.item)?
                }
                // Unlinked: an input that never delivers anything.
                None => new_channel(port.item, 1),
            };
            let generation = next_generation();
            let bridge = add_source(&mut fg, &channel, generation)?;
            let block = built.blocks.id(&port.block).unwrap();
            fg.stream_dyn(bridge, "output", block, port.port.as_str())
                .with_context(|| {
                    format!("input '{}' -> {}.{}", port.name, port.block, port.port)
                })?;
            inputs.push((port.clone(), channel, generation));
        }
        let mut outputs = Vec::new();
        for port in &desc.outputs {
            let channel = self.output_channel(name, &port.name, port.item)?;
            let generation = next_generation();
            let bridge = add_sink(&mut fg, &channel, generation)?;
            let block = built.blocks.id(&port.block).unwrap();
            fg.stream_dyn(block, port.port.as_str(), bridge, "input")
                .with_context(|| {
                    format!("output '{}' <- {}.{}", port.name, port.block, port.port)
                })?;
            outputs.push((port.clone(), channel, generation));
        }
        Ok((
            fg,
            Assembled {
                blocks: built.blocks,
                inputs,
                outputs,
            },
        ))
    }

    /// Build and start `desc` as flowgraph `name`.
    pub async fn spawn_async(&mut self, name: &str, desc: Description) -> Result<()> {
        if self.flowgraphs.contains_key(name) {
            bail!("a flowgraph '{name}' is already running");
        }
        let (fg, parts) = self
            .assemble(name, &desc)
            .with_context(|| format!("building flowgraph '{name}'"))?;
        let managed = start(self.runtime.handle(), fg, parts)
            .await
            .with_context(|| format!("starting flowgraph '{name}'"))?;
        for (_, channel, generation) in &managed.inputs {
            channel.set_reader(*generation);
        }
        self.flowgraphs.insert(name.to_string(), managed);
        Ok(())
    }

    /// Blocking form of [`spawn_async`](Self::spawn_async).
    pub fn spawn(&mut self, name: &str, desc: Description) -> Result<()> {
        block_on(self.spawn_async(name, desc))
    }

    /// Replace the running flowgraph `name` with `desc`, without stopping
    /// the flowgraphs linked to it.
    ///
    /// The new flowgraph is started before the old one lets go of its links
    /// (make before break). Ports are matched by name: every input and output
    /// of the old flowgraph that is linked must exist in `desc` with the same
    /// item type. `hold` decides what happens to input items in between.
    ///
    /// If this fails, or the future is dropped before it completes, the old
    /// flowgraph keeps its links.
    pub async fn replace_async(
        &mut self,
        name: &str,
        desc: Description,
        hold: Hold,
    ) -> Result<Replacement> {
        let t0 = Instant::now();
        self.check_ports(name, &desc)?;
        let (fg, parts) = self
            .assemble(name, &desc)
            .with_context(|| format!("building the replacement of '{name}'"))?;
        let built = Instant::now();

        let paused = PausedInputs::pause(&self.flowgraphs[name].inputs, hold);
        let new = start(self.runtime.handle(), fg, parts)
            .await
            .with_context(|| format!("starting the replacement of '{name}'"))?;
        let started = Instant::now();

        paused.hand_over();
        for (_, channel, generation) in &new.inputs {
            match hold {
                Hold::Keep => channel.set_reader(*generation),
                Hold::Discard => channel.restart_reader(*generation),
            }
        }
        let switched = Instant::now();

        let old = self.flowgraphs.insert(name.to_string(), new).unwrap();
        Ok(Replacement {
            old: self.retire(name, old),
            timings: ReplaceTimings {
                build: built - t0,
                start: started - built,
                switch: switched - started,
                total: switched - t0,
            },
        })
    }

    /// Blocking form of [`replace_async`](Self::replace_async).
    pub fn replace(&mut self, name: &str, desc: Description, hold: Hold) -> Result<Replacement> {
        block_on(self.replace_async(name, desc, hold))
    }

    /// Check that `desc` can replace flowgraph `name`: it has every linked
    /// port, with the same item type.
    fn check_ports(&self, name: &str, desc: &Description) -> Result<()> {
        let old = self
            .flowgraphs
            .get(name)
            .ok_or_else(|| anyhow!("no flowgraph '{name}' is running"))?;
        for (port, _, _) in old.inputs.iter().chain(&old.outputs) {
            let linked = self
                .links
                .contains_key(&(name.to_string(), port.name.clone()))
                || self.links.values().any(|v| v.0 == name && v.1 == port.name);
            if !linked {
                continue;
            }
            match desc.port(&port.name) {
                Some(new) if new.item == port.item => {}
                Some(new) => bail!(
                    "port '{}' carries {} in '{name}', {} in the replacement",
                    port.name,
                    port.item,
                    new.item
                ),
                None => bail!("the replacement of '{name}' has no port '{}'", port.name),
            }
        }
        Ok(())
    }

    /// Let a replaced flowgraph finish in the background: it drains once
    /// its inputs have ended, and is stopped if that takes longer than the
    /// drain timeout (or at once if it has no inputs to end).
    fn retire(&self, name: &str, old: Managed) -> Retired {
        let (tx, done) = oneshot::channel();
        let timeout = if old.inputs.is_empty() {
            Duration::ZERO
        } else {
            self.drain_timeout
        };
        let Managed {
            handle,
            task,
            blocks,
            ..
        } = old;
        self.runtime.spawn_background(async move {
            let result = match select(task, Timer::after(timeout)).await {
                Either::Left((result, _)) => result,
                Either::Right((_, task)) => {
                    let _ = handle.stop().await;
                    task.await
                }
            };
            let _ = tx.send(result);
        });
        Retired {
            name: name.to_string(),
            blocks,
            done,
        }
    }

    /// Stop flowgraph `name` and wait until it has terminated. Streams it
    /// writes to end.
    pub async fn stop_async(&mut self, name: &str) -> Result<Finished> {
        let managed = self
            .flowgraphs
            .remove(name)
            .ok_or_else(|| anyhow!("no flowgraph '{name}' is running"))?;
        let Managed {
            handle,
            task,
            blocks,
            ..
        } = managed;
        let _ = handle.stop().await;
        let flowgraph = task
            .await
            .with_context(|| format!("stopping flowgraph '{name}'"))?;
        Ok(Finished { flowgraph, blocks })
    }

    /// Blocking form of [`stop_async`](Self::stop_async).
    pub fn stop(&mut self, name: &str) -> Result<Finished> {
        block_on(self.stop_async(name))
    }

    /// Wait until flowgraph `name` has finished by itself: its sources
    /// ended, including the flowgraphs feeding its inputs.
    pub async fn wait_async(&mut self, name: &str) -> Result<Finished> {
        let managed = self
            .flowgraphs
            .remove(name)
            .ok_or_else(|| anyhow!("no flowgraph '{name}' is running"))?;
        let flowgraph = managed
            .task
            .await
            .with_context(|| format!("flowgraph '{name}'"))?;
        Ok(Finished {
            flowgraph,
            blocks: managed.blocks,
        })
    }

    /// Blocking form of [`wait_async`](Self::wait_async).
    pub fn wait(&mut self, name: &str) -> Result<Finished> {
        block_on(self.wait_async(name))
    }
}

struct Assembled {
    blocks: Blocks,
    inputs: Vec<(PortDecl, Arc<dyn Pipe>, u64)>,
    outputs: Vec<(PortDecl, Arc<dyn Pipe>, u64)>,
}

/// Start an assembled flowgraph. Its output bridges are queued as writers
/// first, so what they write before the caller finishes the switch is not
/// taken for a stale writer's, and committed once it runs.
async fn start(runtime: RuntimeHandle, fg: Flowgraph, parts: Assembled) -> Result<Managed> {
    let writers = QueuedWriters::queue(&parts.outputs);
    let (task, handle) = runtime.start(fg).await?.split();
    writers.commit();
    Ok(Managed {
        handle,
        task,
        blocks: parts.blocks,
        inputs: parts.inputs,
        outputs: parts.outputs,
    })
}

/// Output bridges queued as writers of their channels. Withdrawn when
/// dropped, unless committed.
struct QueuedWriters(Vec<(Arc<dyn Pipe>, u64)>);

impl QueuedWriters {
    fn queue(outputs: &[(PortDecl, Arc<dyn Pipe>, u64)]) -> Self {
        Self(
            outputs
                .iter()
                .map(|(_, channel, generation)| {
                    channel.queue_writer(*generation);
                    (channel.clone(), *generation)
                })
                .collect(),
        )
    }

    fn commit(mut self) {
        for (channel, generation) in self.0.drain(..) {
            channel.commit_writer(generation);
        }
    }
}

impl Drop for QueuedWriters {
    fn drop(&mut self) {
        for (channel, generation) in self.0.drain(..) {
            channel.unqueue_writer(generation);
        }
    }
}

/// With [`Hold::Discard`], the inputs of a flowgraph being replaced: they
/// drop what arrives, while the old flowgraph keeps reading what is already
/// queued. They take items again when dropped, unless handed over to the
/// replacement.
///
/// The old flowgraph keeps its reader turn until the replacement takes it:
/// a bridge that loses its turn ends its stream for good.
struct PausedInputs(Vec<Arc<dyn Pipe>>);

impl PausedInputs {
    fn pause(inputs: &[(PortDecl, Arc<dyn Pipe>, u64)], hold: Hold) -> Self {
        if hold == Hold::Keep {
            return Self(Vec::new());
        }
        Self(
            inputs
                .iter()
                .map(|(_, channel, _)| {
                    channel.set_accept(false);
                    channel.clone()
                })
                .collect(),
        )
    }

    fn hand_over(mut self) {
        self.0.clear();
    }
}

impl Drop for PausedInputs {
    fn drop(&mut self) {
        for channel in self.0.drain(..) {
            channel.set_accept(true);
        }
    }
}
