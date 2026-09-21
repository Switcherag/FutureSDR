use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
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
use futuresdr::runtime::DefaultScheduler;
use futuresdr::runtime::Flowgraph;
use futuresdr::runtime::FlowgraphHandle;
use futuresdr::runtime::FlowgraphTask;
use futuresdr::runtime::Pmt;
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
use crate::description::MessagePortDecl;
use crate::description::PortDecl;
use crate::items::ItemType;
use crate::items::with_item_type;
use crate::registry::Registry;
use crate::segments::Split;
use crate::segments::changed_swappable;
use crate::topic::Tap;
use crate::topic::Topic;
use crate::topic::TopicSink;
use crate::topic::TopicSource;

/// What happens, when a flowgraph is replaced, to the input items it has not
/// taken yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// The new flowgraph gets them. Until then the old flowgraph goes on
    /// consuming; the input moves over at one item, so nothing is lost or
    /// delivered twice.
    Keep,
    /// They are dropped: the new flowgraph starts on items that arrive once
    /// it has taken over.
    Discard,
}

/// Where the time of a replacement went.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReplaceTimings {
    /// Building the new flowgraph (plugins, blocks, connections, bridges).
    pub build: Duration,
    /// Starting it.
    pub start: Duration,
    /// Setting the controls it asks for (see [`Controller`]).
    pub controls: Duration,
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

/// A running flowgraph, not linked yet, ready to become a flowgraph of the
/// controller with [`Controller::commit`].
///
/// Its inputs deliver nothing and its outputs wait until it is committed;
/// its blocks can be sent messages to set it up in the meantime. Dropped
/// without being committed, it is stopped.
pub struct Standby {
    name: String,
    managed: Option<Managed>,
    scheduler: DefaultScheduler,
    build: Duration,
    start: Duration,
}

/// The kind and item type of a port, to compare a flowgraph's ports with
/// its replacement's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PortKind {
    Stream(ItemType),
    Message,
}

impl std::fmt::Display for PortKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PortKind::Stream(item) => write!(f, "{item}"),
            PortKind::Message => write!(f, "messages"),
        }
    }
}

impl Standby {
    /// Name it will run under.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Handle to the flowgraph, for messages.
    pub fn handle(&self) -> FlowgraphHandle {
        self.managed().handle.clone()
    }

    /// Its blocks.
    pub fn blocks(&self) -> &Blocks {
        &self.managed().blocks
    }

    /// Time spent building it (plugins, blocks, connections, bridges).
    pub fn build_time(&self) -> Duration {
        self.build
    }

    /// Time spent starting it.
    pub fn start_time(&self) -> Duration {
        self.start
    }

    fn managed(&self) -> &Managed {
        self.managed
            .as_ref()
            .expect("a standby holds its flowgraph")
    }
}

impl Drop for Standby {
    fn drop(&mut self) {
        let Some(managed) = self.managed.take() else {
            return;
        };
        managed.withdraw();
        let Managed { handle, task, .. } = managed;
        self.scheduler
            .spawn(async move {
                let _ = handle.stop().await;
                let _ = task.await;
            })
            .detach();
    }
}

impl std::fmt::Debug for Standby {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Standby")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

struct StreamIn {
    port: PortDecl,
    channel: Arc<dyn Pipe>,
    sub: u64,
    generation: u64,
}

struct StreamOut {
    port: PortDecl,
    channel: Arc<dyn Pipe>,
    generation: u64,
}

struct MessageIn {
    port: MessagePortDecl,
    topic: Arc<Topic>,
    sub: u64,
    generation: u64,
}

struct MessageOut {
    port: MessagePortDecl,
    topic: Arc<Topic>,
    generation: u64,
}

/// A control of a flowgraph: the block and message port that take it.
#[derive(Clone)]
struct Control {
    name: String,
    block: BlockId,
    port: String,
}

/// The ends of a built flowgraph, besides its blocks.
struct Ends {
    inputs: Vec<StreamIn>,
    outputs: Vec<StreamOut>,
    message_inputs: Vec<MessageIn>,
    message_outputs: Vec<MessageOut>,
    controls: Vec<Control>,
    radio: Vec<(String, Pmt)>,
}

impl Ends {
    /// Put the outputs on standby.
    fn standby(&self) {
        for out in &self.outputs {
            out.channel.standby_writer(out.generation);
        }
        for out in &self.message_outputs {
            out.topic.standby_publisher(out.generation);
        }
    }

    /// Drop the outputs on standby.
    fn withdraw(&self) {
        for out in &self.outputs {
            out.channel.withdraw_writer(out.generation);
        }
        for out in &self.message_outputs {
            out.topic.withdraw_publisher(out.generation);
        }
    }

    fn control(&self, name: &str) -> Option<&Control> {
        self.controls.iter().find(|c| c.name == name)
    }

    fn input_names(&self) -> Vec<String> {
        self.inputs.iter().map(|i| i.port.name.clone()).collect()
    }

    /// Kind of port `name`, if the flowgraph has it.
    fn kind(&self, name: &str) -> Option<PortKind> {
        let stream = self.inputs.iter().map(|i| &i.port);
        let stream = stream.chain(self.outputs.iter().map(|o| &o.port));
        if let Some(port) = stream.into_iter().find(|p| p.name == name) {
            return Some(PortKind::Stream(port.item));
        }
        let messages = self.message_inputs.iter().map(|i| &i.port.name);
        messages
            .chain(self.message_outputs.iter().map(|o| &o.port.name))
            .any(|n| n == name)
            .then_some(PortKind::Message)
    }

    fn port_names(&self) -> impl Iterator<Item = &String> {
        let stream = self.inputs.iter().map(|i| &i.port.name);
        let stream = stream.chain(self.outputs.iter().map(|o| &o.port.name));
        let messages = self.message_inputs.iter().map(|i| &i.port.name);
        stream
            .chain(messages)
            .chain(self.message_outputs.iter().map(|o| &o.port.name))
    }
}

struct Managed {
    handle: FlowgraphHandle,
    task: FlowgraphTask,
    blocks: Blocks,
    ends: Ends,
}

impl std::ops::Deref for Managed {
    type Target = Ends;
    fn deref(&self) -> &Ends {
        &self.ends
    }
}

type PortKey = (String, String);

/// Controls to set, and to remember.
#[derive(Default)]
struct Plan {
    /// (flowgraph, control, value) to set on running flowgraphs, in order.
    calls: Vec<(String, String, Pmt)>,
    /// (flowgraph, control, value) asked for.
    desires: Vec<(String, String, Pmt)>,
    /// Inputs of running flowgraphs between those set and the one that
    /// asks.
    between: Vec<PortKey>,
}

/// Links parked while controls are set: unparked when dropped, unless
/// [kept](Paused::keep) as they are.
#[derive(Default)]
struct Paused {
    links: Vec<(Arc<dyn Pipe>, u64)>,
    /// Links between, with their readers, to empty before they go on.
    between: Vec<(Arc<dyn Pipe>, u64, u64)>,
}

impl Paused {
    fn park(&mut self, channel: &Arc<dyn Pipe>, sub: u64) {
        channel.park(sub, Hold::Keep);
        self.links.push((channel.clone(), sub));
    }

    /// Unpark them now; the links between drop what they hold.
    fn resume(mut self) {
        for (channel, sub, reader) in self.between.drain(..) {
            channel.restart_reader(sub, reader);
            channel.unpark(sub);
        }
    }

    /// Leave them parked; the links between drop what they hold and go on.
    fn keep(mut self) {
        self.links.clear();
        self.resume();
    }
}

impl Drop for Paused {
    fn drop(&mut self) {
        let between = self.between.drain(..).map(|(c, s, _)| (c, s));
        for (channel, sub) in self.links.drain(..).chain(between) {
            channel.unpark(sub);
        }
    }
}

/// Where a demand for a control goes.
enum Target {
    /// This running flowgraph has the control; the inputs of the running
    /// flowgraphs in between, on the way to it.
    Running(String, Vec<PortKey>),
    /// This flowgraph is not running; it may have the control.
    Absent(String),
}

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
/// itself. Drive the controller from a task of its runtime with
/// [`run`](Self::run) instead:
///
/// ```ignore
/// ctrl.run(|mut ctrl| async move {
///     ctrl.replace_async("receiver", rx_b, Hold::Keep).await?;
///     anyhow::Ok(())
/// })?;
/// ```
///
/// Most of a replacement is starting the new flowgraph. To switch in
/// microseconds, start it ahead with [`prepare`](Self::prepare) and switch
/// with [`commit`](Self::commit) when the time comes:
///
/// ```ignore
/// let standby = ctrl.prepare("receiver", rx_b)?;
/// // later
/// let old = ctrl.commit(standby, Hold::Keep)?;
/// ```
///
/// # Links
///
/// An output may feed several inputs, each with a queue of its own. A link
/// can be *parked*: it gets nothing, and the flowgraph it feeds idles.
/// [`select`](Self::select) makes one link of an output the only one that
/// gets items, at once: flowgraphs that are all running take turns without
/// being started or replaced.
///
/// Message outputs (`[message_outputs]`) link to message inputs the same
/// way, and [`tap`](Self::tap) hands their messages to the application.
///
/// # Controls
///
/// A flowgraph asks the flowgraphs feeding it for settings in its `[radio]`
/// section, and a flowgraph offers settings in its `[controls]` section: a
/// demand goes up the links to the nearest flowgraph that offers it. It is
/// applied when the asking flowgraph is committed or its link is selected,
/// before its input switches over, and only if the value changes. A parked
/// link asks for nothing; a demand that no flowgraph offers is ignored.
///
/// While a setting changes, the links concerned get nothing, and the
/// flowgraph that asked starts on items that arrive after, whatever the
/// [`Hold`]: items from before suit neither flowgraph. The links of the
/// flowgraphs in between drop what they hold too; what their blocks hold
/// still follows. A flowgraph that is
/// committed with settings asked of its name (as the replacement of one
/// that had them, or because they were asked before it ran) gets them
/// first, and drops what its outputs made before.
pub struct Controller {
    runtime: Runtime,
    registry: Registry,
    capacity: usize,
    drain_timeout: Duration,
    flowgraphs: BTreeMap<String, Managed>,
    /// input (flowgraph, port) -> output (flowgraph, port)
    links: HashMap<PortKey, PortKey>,
    /// input -> its subscription to the output's channel or topic
    subscriptions: HashMap<PortKey, u64>,
    /// parked inputs
    parked: HashSet<PortKey>,
    /// by output (flowgraph, port)
    channels: HashMap<PortKey, Arc<dyn Pipe>>,
    /// by message output (flowgraph, port)
    topics: HashMap<PortKey, Arc<Topic>>,
    /// flowgraph -> control -> value asked for
    desired: HashMap<String, BTreeMap<String, Pmt>>,
    /// flowgraph -> control -> value set on the running flowgraph
    applied: HashMap<String, BTreeMap<String, Pmt>>,
    /// Ports of flowgraphs with swappable blocks that are on one of their
    /// segments: (flowgraph, port) -> (segment, port).
    aliases: HashMap<PortKey, PortKey>,
    /// Flowgraphs with swappable blocks: the description they run.
    segmented: HashMap<String, Description>,
}

/// The flowgraph running swappable block `block` of flowgraph `name`.
fn segment_name(name: &str, block: &str) -> String {
    format!("{name}/{block}")
}

/// The flowgraph of a part of flowgraph `name`: its main one, or a segment.
fn part_name(name: &str, part: &Option<String>) -> String {
    match part {
        None => name.to_string(),
        Some(block) => segment_name(name, block),
    }
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

fn add_source(
    fg: &mut Flowgraph,
    channel: &Arc<dyn Pipe>,
    sub: u64,
    generation: u64,
) -> Result<BlockId> {
    let item = channel.item();
    with_item_type!(item, T => {
        let channel = channel.clone().as_any().downcast::<Channel<T>>().unwrap();
        Ok(fg.add(BridgeSource::<T>::new(channel, sub, generation))?.id())
    })
}

/// Set `control` of a flowgraph to `value`, through `handle`.
async fn call_control(handle: &FlowgraphHandle, control: &Control, value: &Pmt) -> Result<()> {
    match handle
        .call(control.block, control.port.as_str(), value.clone())
        .await?
    {
        Pmt::InvalidValue => bail!("{value:?} is not a valid value"),
        _ => Ok(()),
    }
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
            subscriptions: HashMap::new(),
            parked: HashSet::new(),
            channels: HashMap::new(),
            topics: HashMap::new(),
            desired: HashMap::new(),
            applied: HashMap::new(),
            aliases: HashMap::new(),
            segmented: HashMap::new(),
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

    /// Where port `what` (`"flowgraph.port"`) is: on the flowgraph named, or
    /// on the segment of a swappable block that has it.
    fn port(&self, what: &str) -> Result<PortKey> {
        let key = split_port(what)?;
        Ok(self.aliases.get(&key).cloned().unwrap_or(key))
    }

    /// Move what is attached to port `from` to port `to`: links, their
    /// subscriptions, channels and taps made before the port moved to a
    /// segment.
    fn rekey(&mut self, from: &PortKey, to: &PortKey) {
        if let Some(v) = self.links.remove(from) {
            self.links.insert(to.clone(), v);
        }
        for v in self.links.values_mut().filter(|v| *v == from) {
            *v = to.clone();
        }
        if let Some(v) = self.subscriptions.remove(from) {
            self.subscriptions.insert(to.clone(), v);
        }
        if self.parked.remove(from) {
            self.parked.insert(to.clone());
        }
        if let Some(v) = self.channels.remove(from) {
            self.channels.insert(to.clone(), v);
        }
        if let Some(v) = self.topics.remove(from) {
            self.topics.insert(to.clone(), v);
        }
    }

    /// Connect output `from` of one flowgraph to input `to` of another, both
    /// written `"flowgraph.port"`, for streams or messages. The input's
    /// flowgraph must not be running; each input has one link, an output may
    /// have several.
    pub fn link(&mut self, from: &str, to: &str) -> Result<()> {
        let from = self.port(from)?;
        let to = self.port(to)?;
        if self.flowgraphs.contains_key(&to.0) {
            bail!("'{}' is running; link its inputs before spawning it", to.0);
        }
        if let Some(old) = self.links.get(&to) {
            bail!("{}.{} is already linked to {}.{}", to.0, to.1, old.0, old.1);
        }
        self.links.insert(to.clone(), from.clone());
        if let Some(channel) = self.channels.get(&from).cloned() {
            self.subscription(&to, |parked| channel.subscribe(parked));
        } else if let Some(topic) = self.topics.get(&from).cloned() {
            self.subscription(&to, |parked| topic.subscribe(parked, false));
        }
        Ok(())
    }

    /// Remove the link of input `to`, whose flowgraph must not be running.
    pub fn unlink(&mut self, to: &str) -> Result<()> {
        let to = self.port(to)?;
        if self.flowgraphs.contains_key(&to.0) {
            bail!(
                "'{}' is running; unlink its inputs once it is stopped",
                to.0
            );
        }
        let from = self
            .links
            .remove(&to)
            .ok_or_else(|| anyhow!("{}.{} is not linked", to.0, to.1))?;
        self.parked.remove(&to);
        if let Some(sub) = self.subscriptions.remove(&to) {
            if let Some(channel) = self.channels.get(&from) {
                channel.unsubscribe(sub);
            } else if let Some(topic) = self.topics.get(&from) {
                topic.unsubscribe(sub);
            }
        }
        Ok(())
    }

    /// The subscription of input `to`, made by `subscribe` (with whether the
    /// link is parked) if there is none yet.
    fn subscription(&mut self, to: &PortKey, subscribe: impl FnOnce(bool) -> u64) -> u64 {
        if let Some(sub) = self.subscriptions.get(to) {
            return *sub;
        }
        let sub = subscribe(self.parked.contains(to));
        self.subscriptions.insert(to.clone(), sub);
        sub
    }

    /// Links from output `from`, which has a channel or topic now.
    fn subscribe_links(&mut self, from: &PortKey, subscribe: &dyn Fn(bool) -> u64) {
        let inputs: Vec<PortKey> = self
            .links
            .iter()
            .filter(|(_, f)| *f == from)
            .map(|(t, _)| t.clone())
            .collect();
        for to in inputs {
            self.subscription(&to, subscribe);
        }
    }

    /// Stop giving items to input `to` (`"flowgraph.port"`): what is queued
    /// stays with [`Hold::Keep`] and is dropped with [`Hold::Discard`]. The
    /// flowgraph need not be running; a link parked before it starts gets
    /// nothing from the start.
    pub fn park(&mut self, to: &str, hold: Hold) -> Result<()> {
        let (to, from) = self.linked(to)?;
        self.parked.insert(to.clone());
        if let Some(sub) = self.subscriptions.get(&to) {
            if let Some(channel) = self.channels.get(&from) {
                channel.park(*sub, hold);
            } else if let Some(topic) = self.topics.get(&from) {
                topic.park(*sub, hold);
            }
        }
        Ok(())
    }

    /// Give items to input `to` again, from now on. Asks for no controls;
    /// see [`select_async`](Self::select_async).
    pub fn unpark(&mut self, to: &str) -> Result<()> {
        let (to, from) = self.linked(to)?;
        self.parked.remove(&to);
        if let Some(sub) = self.subscriptions.get(&to) {
            if let Some(channel) = self.channels.get(&from) {
                channel.unpark(*sub);
            } else if let Some(topic) = self.topics.get(&from) {
                topic.unpark(*sub);
            }
        }
        Ok(())
    }

    /// Make input `to` the only link of its output that gets items, from
    /// the next item on; `hold` decides what the other links keep. The
    /// controls the flowgraph of `to` asks for are set first; if that
    /// changes any, the output's links get nothing meanwhile, and `to`
    /// starts on items that arrive after.
    ///
    /// If this fails, or the future is dropped before it completes, no link
    /// changes.
    pub async fn select_async(&mut self, to: &str, hold: Hold) -> Result<()> {
        let (to, from) = self.linked(to)?;
        let mut plan = Plan::default();
        if let Some(radio) = self
            .flowgraphs
            .get(&to.0)
            .filter(|m| m.inputs.iter().any(|i| i.port.name == to.1))
            .map(|m| m.radio.clone())
        {
            for (control, value) in &radio {
                self.plan(
                    &to.0,
                    std::slice::from_ref(&to.1),
                    true,
                    control,
                    value,
                    &mut plan,
                );
            }
        }
        let retune = !plan.calls.is_empty();
        let mut paused = Paused::default();
        self.pause_between(&plan, &mut paused);
        if retune && let Some(channel) = self.channels.get(&from) {
            for (t, f) in &self.links {
                if *f == from && !self.parked.contains(t) {
                    paused.park(channel, self.subscriptions[t]);
                }
            }
        }
        self.execute(&to.0, plan).await?;
        paused.keep();

        for (t, f) in &self.links {
            if *f == from {
                if *t == to {
                    self.parked.remove(t);
                } else {
                    self.parked.insert(t.clone());
                }
            }
        }
        if let Some(sub) = self.subscriptions.get(&to) {
            if let Some(channel) = self.channels.get(&from) {
                channel.select(*sub, hold, retune);
            } else if let Some(topic) = self.topics.get(&from) {
                topic.select(*sub, hold);
            }
        }
        Ok(())
    }

    /// Blocking form of [`select_async`](Self::select_async).
    pub fn select(&mut self, to: &str, hold: Hold) -> Result<()> {
        block_on(self.select_async(to, hold))
    }

    /// Input `to` and the output it is linked to.
    fn linked(&self, to: &str) -> Result<(PortKey, PortKey)> {
        let to = self.port(to)?;
        let from = self
            .links
            .get(&to)
            .cloned()
            .ok_or_else(|| anyhow!("{}.{} is not linked", to.0, to.1))?;
        Ok((to, from))
    }

    /// The messages message output `from` (`"flowgraph.port"`) posts from
    /// now on, whichever flowgraph runs under that name.
    pub fn tap(&mut self, from: &str) -> Result<Tap> {
        let key = self.port(from)?;
        let topic = self.output_topic(&key)?;
        Ok(Tap::new(topic, from.to_string()))
    }

    /// Values of the controls set on flowgraph `name`.
    pub fn controls(&self, name: &str) -> BTreeMap<String, Pmt> {
        self.applied.get(name).cloned().unwrap_or_default()
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

    /// State of the link of stream or message input `port`
    /// (`"flowgraph.port"`), or of all the links of stream output `port`.
    pub fn link_stats(&self, port: &str) -> Option<ChannelStats> {
        let key = self.port(port).ok()?;
        if let Some(channel) = self.channels.get(&key) {
            return channel.stats(None);
        }
        let from = self.links.get(&key)?;
        let sub = *self.subscriptions.get(&key)?;
        if let Some(channel) = self.channels.get(from) {
            return channel.stats(Some(sub));
        }
        let (queued, dropped, parked) = self.topics.get(from)?.stats(sub)?;
        Some(ChannelStats {
            queued,
            dropped,
            closed: false,
            parked,
        })
    }

    /// The channel behind output `(fg, port)`, created on first use.
    fn output_channel(&mut self, key: &PortKey, item: ItemType) -> Result<Arc<dyn Pipe>> {
        let (fg, port) = key;
        if self.topics.contains_key(key) {
            bail!("{fg}.{port} is a message output");
        }
        if let Some(channel) = self.channels.get(key) {
            if channel.item() != item {
                bail!(
                    "{fg}.{port} carries {}, a linked input expects {item}",
                    channel.item()
                );
            }
            return Ok(channel.clone());
        }
        let channel = new_channel(item, self.capacity);
        self.channels.insert(key.clone(), channel.clone());
        let c = channel.clone();
        self.subscribe_links(key, &move |parked| c.subscribe(parked));
        Ok(channel)
    }

    /// The topic behind message output `key`, created on first use.
    fn output_topic(&mut self, key: &PortKey) -> Result<Arc<Topic>> {
        if self.channels.contains_key(key) {
            bail!("{}.{} is a stream output", key.0, key.1);
        }
        if let Some(topic) = self.topics.get(key) {
            return Ok(topic.clone());
        }
        let topic = Topic::new();
        self.topics.insert(key.clone(), topic.clone());
        let t = topic.clone();
        self.subscribe_links(key, &move |parked| t.subscribe(parked, false));
        Ok(topic)
    }

    /// Build `desc` with bridges on its ports. Bridges are not active yet.
    fn assemble(&mut self, name: &str, desc: &Description) -> Result<(Flowgraph, Blocks, Ends)> {
        self.registry.load_all(&desc.plugins)?;
        let built = build(&self.registry, desc)?;
        let mut fg = built.flowgraph;
        let blocks = built.blocks;
        let key = |port: &str| (name.to_string(), port.to_string());
        let block = |name: &str| blocks.id(name).unwrap();

        let mut inputs = Vec::new();
        for port in &desc.inputs {
            let to = key(&port.name);
            let (channel, sub) = match self.links.get(&to).cloned() {
                Some(from) => {
                    let channel = self.output_channel(&from, port.item)?;
                    let sub = self.subscription(&to, |parked| channel.subscribe(parked));
                    (channel, sub)
                }
                // Unlinked: an input that never delivers anything.
                None => {
                    let channel = new_channel(port.item, 1);
                    let sub = channel.subscribe(false);
                    (channel, sub)
                }
            };
            let generation = next_generation();
            let bridge = add_source(&mut fg, &channel, sub, generation)?;
            fg.stream_dyn(bridge, "output", block(&port.block), port.port.as_str())
                .with_context(|| {
                    format!("input '{}' -> {}.{}", port.name, port.block, port.port)
                })?;
            inputs.push(StreamIn {
                port: port.clone(),
                channel,
                sub,
                generation,
            });
        }
        let mut outputs = Vec::new();
        for port in &desc.outputs {
            let channel = self.output_channel(&key(&port.name), port.item)?;
            let generation = next_generation();
            let bridge = add_sink(&mut fg, &channel, generation)?;
            fg.stream_dyn(block(&port.block), port.port.as_str(), bridge, "input")
                .with_context(|| {
                    format!("output '{}' <- {}.{}", port.name, port.block, port.port)
                })?;
            outputs.push(StreamOut {
                port: port.clone(),
                channel,
                generation,
            });
        }
        let mut message_inputs = Vec::new();
        for port in &desc.message_inputs {
            let to = key(&port.name);
            let (topic, sub) = match self.links.get(&to).cloned() {
                Some(from) => {
                    let topic = self.output_topic(&from)?;
                    let sub = self.subscription(&to, |parked| topic.subscribe(parked, false));
                    (topic, sub)
                }
                None => {
                    let topic = Topic::new();
                    let sub = topic.subscribe(false, false);
                    (topic, sub)
                }
            };
            let generation = next_generation();
            let bridge = fg
                .add(TopicSource::new(topic.clone(), sub, generation))?
                .id();
            fg.message(bridge, "out", block(&port.block), port.port.as_str())
                .with_context(|| {
                    format!(
                        "message input '{}' -> {}.{}",
                        port.name, port.block, port.port
                    )
                })?;
            message_inputs.push(MessageIn {
                port: port.clone(),
                topic,
                sub,
                generation,
            });
        }
        let mut message_outputs = Vec::new();
        let origin: Arc<str> = Arc::from(desc.name.as_deref().unwrap_or(name));
        for port in &desc.message_outputs {
            let topic = self.output_topic(&key(&port.name))?;
            let generation = next_generation();
            let bridge = fg
                .add(TopicSink::new(topic.clone(), generation, origin.clone()))?
                .id();
            fg.message(block(&port.block), port.port.as_str(), bridge, "in")
                .with_context(|| {
                    format!(
                        "message output '{}' <- {}.{}",
                        port.name, port.block, port.port
                    )
                })?;
            message_outputs.push(MessageOut {
                port: port.clone(),
                topic,
                generation,
            });
        }
        let controls = desc
            .controls
            .iter()
            .map(|c| Control {
                name: c.name.clone(),
                block: block(&c.block),
                port: c.port.clone(),
            })
            .collect();
        let ends = Ends {
            inputs,
            outputs,
            message_inputs,
            message_outputs,
            controls,
            radio: desc.radio.clone(),
        };
        Ok((fg, blocks, ends))
    }

    /// Build and start `desc` on standby, to become flowgraph `name` when
    /// [committed](Self::commit). If `name` is running, `desc` must be able
    /// to replace it (see [`replace_async`](Self::replace_async)).
    ///
    /// Starting a flowgraph is the slow part of a replacement: with a
    /// standby prepared in advance, the replacement itself takes
    /// microseconds.
    pub async fn prepare_async(&mut self, name: &str, desc: Description) -> Result<Standby> {
        let t0 = Instant::now();
        let what = if self.flowgraphs.contains_key(name) {
            self.check_ports(name, |port| {
                let stream = desc.port(port).map(|p| PortKind::Stream(p.item));
                stream.or_else(|| desc.message_port(port).map(|_| PortKind::Message))
            })?;
            format!("the replacement of '{name}'")
        } else {
            format!("flowgraph '{name}'")
        };
        let (fg, blocks, ends) = self
            .assemble(name, &desc)
            .with_context(|| format!("building {what}"))?;
        let built = Instant::now();
        let managed = start(self.runtime.handle(), fg, blocks, ends)
            .await
            .with_context(|| format!("starting {what}"))?;
        Ok(Standby {
            name: name.to_string(),
            managed: Some(managed),
            scheduler: self.runtime.scheduler().clone(),
            build: built - t0,
            start: built.elapsed(),
        })
    }

    /// Blocking form of [`prepare_async`](Self::prepare_async).
    pub fn prepare(&mut self, name: &str, desc: Description) -> Result<Standby> {
        block_on(self.prepare_async(name, desc))
    }

    /// Link `standby` in place of the flowgraph running under its name, or
    /// as a new flowgraph if none is: set the controls it offers that were
    /// asked for, and the controls it asks for (see [`Controller`]), then
    /// switch. With no control to set, this takes microseconds.
    ///
    /// Returns the flowgraph it replaced, which finishes in the background.
    /// `hold` decides what happens to the input items the replaced
    /// flowgraph has not taken yet.
    ///
    /// If this fails, or the future is dropped before it completes, the
    /// standby is stopped and nothing is switched.
    pub async fn commit_async(&mut self, standby: Standby, hold: Hold) -> Result<Option<Retired>> {
        Ok(self.commit_timed(standby, hold).await?.0)
    }

    /// Blocking form of [`commit_async`](Self::commit_async).
    pub fn commit(&mut self, standby: Standby, hold: Hold) -> Result<Option<Retired>> {
        block_on(self.commit_async(standby, hold))
    }

    /// [`commit_async`](Self::commit_async), and the time spent on
    /// controls.
    async fn commit_timed(
        &mut self,
        mut standby: Standby,
        hold: Hold,
    ) -> Result<(Option<Retired>, Duration)> {
        let t0 = Instant::now();
        let name = standby.name.clone();
        let new = standby.managed();
        self.check_standby(&name, new)?;

        // What was asked of the flowgraph it replaces, or before it ran:
        // set on it if it offers it, else passed up. Then what it asks for.
        let inputs = new.input_names();
        let handle = new.handle.clone();
        let mut own = Vec::new();
        let mut plan = Plan::default();
        for (control, value) in self.desired.get(&name).cloned().unwrap_or_default() {
            match new.control(&control) {
                Some(c) => own.push((c.clone(), value)),
                None => self.plan(&name, &inputs, false, &control, &value, &mut plan),
            }
        }
        for (control, value) in &new.radio {
            self.plan(&name, &inputs, false, control, value, &mut plan);
        }

        // While the flowgraphs feeding it change their settings, its links
        // get nothing: what arrives meanwhile suits neither flowgraph.
        let retune = !plan.calls.is_empty();
        let mut paused = Paused::default();
        self.pause_between(&plan, &mut paused);
        if retune {
            for input in &new.inputs {
                let to = (name.clone(), input.port.name.clone());
                if self.links.contains_key(&to) && !self.parked.contains(&to) {
                    paused.park(&input.channel, input.sub);
                }
            }
        }
        let mut applied = BTreeMap::new();
        for (control, value) in own {
            call_control(&handle, &control, &value)
                .await
                .with_context(|| format!("setting {} of '{name}'", control.name))?;
            applied.insert(control.name, value);
        }
        self.execute(&name, plan).await?;
        let controls = t0.elapsed();

        let new = standby
            .managed
            .take()
            .expect("a standby holds its flowgraph");
        for out in &new.outputs {
            if !applied.is_empty() {
                // It held what it made before its settings changed.
                out.channel.drop_held(out.generation);
            }
            out.channel.commit_writer(out.generation);
        }
        for out in &new.message_outputs {
            out.topic.commit_publisher(out.generation);
        }
        let old = self.flowgraphs.remove(&name);
        let discard = hold == Hold::Discard && old.is_some();
        for input in &new.inputs {
            if discard || retune {
                input.channel.restart_reader(input.sub, input.generation);
            } else {
                input.channel.set_reader(input.sub, input.generation);
            }
        }
        paused.resume();
        for input in &new.message_inputs {
            if discard {
                input.topic.restart_reader(input.sub, input.generation);
            } else {
                input.topic.set_reader(input.sub, input.generation);
            }
        }
        let retired = old.map(|old| self.retire(&name, old));
        self.applied.insert(name.clone(), applied);
        self.flowgraphs.insert(name, new);
        Ok((retired, controls))
    }

    /// Where a demand for `control` from flowgraph `fg`, through its inputs
    /// `inputs`, goes: up the links that are not parked (`first` also
    /// follows the parked ones among `inputs`), to the nearest flowgraphs
    /// that have the control, or that are not running.
    fn targets(&self, fg: &str, inputs: &[String], first: bool, control: &str) -> Vec<Target> {
        let mut targets = Vec::new();
        let mut seen = HashSet::from([fg.to_string()]);
        // (flowgraph, its inputs, whether it asks, inputs followed to it)
        let mut stack = vec![(fg.to_string(), inputs.to_vec(), true, Vec::new())];
        while let Some((fg, inputs, asks, path)) = stack.pop() {
            for port in inputs {
                let to = (fg.clone(), port);
                if self.parked.contains(&to) && !(asks && first) {
                    continue;
                }
                let Some((provider, _)) = self.links.get(&to) else {
                    continue;
                };
                if !seen.insert(provider.clone()) {
                    continue;
                }
                let mut path = path.clone();
                if !asks {
                    path.push(to.clone());
                }
                match self.flowgraphs.get(provider) {
                    Some(m) if m.control(control).is_some() => {
                        targets.push(Target::Running(provider.clone(), path))
                    }
                    Some(m) => stack.push((provider.clone(), m.input_names(), false, path)),
                    None => targets.push(Target::Absent(provider.clone())),
                }
            }
        }
        targets
    }

    /// Add to `plan` what asking for `control = value` upstream of inputs
    /// `inputs` of `fg` takes (see [`targets`](Self::targets)): setting it
    /// on the running flowgraphs where it changes, and remembering it.
    fn plan(
        &self,
        fg: &str,
        inputs: &[String],
        first: bool,
        control: &str,
        value: &Pmt,
        plan: &mut Plan,
    ) {
        for target in self.targets(fg, inputs, first, control) {
            let provider = match target {
                Target::Running(provider, between) => {
                    let planned = plan
                        .calls
                        .iter()
                        .rev()
                        .find(|(p, c, _)| *p == provider && c == control)
                        .map(|(_, _, v)| v);
                    let current = planned
                        .or_else(|| self.applied.get(&provider).and_then(|a| a.get(control)));
                    if current != Some(value) {
                        plan.calls
                            .push((provider.clone(), control.to_string(), value.clone()));
                        plan.between.extend(between);
                    }
                    provider
                }
                Target::Absent(provider) => provider,
            };
            plan.desires
                .push((provider, control.to_string(), value.clone()));
        }
    }

    /// Park, until `paused` resumes, the links between of `plan` that are
    /// not parked.
    fn pause_between(&self, plan: &Plan, paused: &mut Paused) {
        for to in &plan.between {
            if self.parked.contains(to) {
                continue;
            }
            let (Some(sub), Some(m)) = (self.subscriptions.get(to), self.flowgraphs.get(&to.0))
            else {
                continue;
            };
            let Some(input) = m.inputs.iter().find(|i| i.port.name == to.1) else {
                continue;
            };
            if paused
                .between
                .iter()
                .any(|(c, s, _)| Arc::ptr_eq(c, &input.channel) && *s == *sub)
            {
                continue;
            }
            input.channel.park(*sub, Hold::Keep);
            paused
                .between
                .push((input.channel.clone(), *sub, input.generation));
        }
    }

    /// Carry out `plan`, made for flowgraph `fg`.
    async fn execute(&mut self, fg: &str, plan: Plan) -> Result<()> {
        for (provider, control, value) in plan.calls {
            let m = &self.flowgraphs[&provider];
            let c = m
                .control(&control)
                .expect("a target has the control")
                .clone();
            let handle = m.handle.clone();
            call_control(&handle, &c, &value)
                .await
                .with_context(|| format!("setting {control} of '{provider}' for '{fg}'"))?;
            self.applied
                .entry(provider)
                .or_default()
                .insert(control, value);
        }
        for (provider, control, value) in plan.desires {
            self.desired
                .entry(provider)
                .or_default()
                .insert(control, value);
        }
        Ok(())
    }

    /// Build and start `desc` as flowgraph `name`.
    ///
    /// Each block `desc` lists as `swappable` runs as a flowgraph of its own,
    /// `name/block`, linked to the flowgraph of the others, `name`; the
    /// ports of `desc` keep their names (`name.port`) wherever their blocks
    /// run. [`replace`](Self::replace) with a description that differs in
    /// these blocks only replaces them.
    pub async fn spawn_async(&mut self, name: &str, desc: Description) -> Result<()> {
        if self.flowgraphs.contains_key(name) || self.segmented.contains_key(name) {
            bail!("a flowgraph '{name}' is already running");
        }
        if !desc.swappable.is_empty() {
            return self.spawn_segmented(name, desc).await;
        }
        let standby = self.prepare_async(name, desc).await?;
        self.commit_async(standby, Hold::Keep).await?;
        Ok(())
    }

    async fn spawn_segmented(&mut self, name: &str, desc: Description) -> Result<()> {
        let split = Split::new(&desc)?;
        for (port, block) in &split.moved {
            let from = (name.to_string(), port.clone());
            let to = (segment_name(name, block), port.clone());
            self.rekey(&from, &to);
            self.aliases.insert(from, to);
        }
        for (from, output, to, input) in &split.links {
            self.link(
                &format!("{}.{output}", part_name(name, from)),
                &format!("{}.{input}", part_name(name, to)),
            )?;
        }
        // Downstream first does not matter: links queue until read.
        let mut started: Vec<String> = Vec::new();
        let parts = split
            .segments
            .into_iter()
            .map(|(block, d)| (segment_name(name, &block), d))
            .chain([(name.to_string(), split.main)]);
        for (part, d) in parts {
            let result = async {
                let standby = self.prepare_async(&part, d).await?;
                self.commit_async(standby, Hold::Keep).await
            }
            .await;
            if let Err(e) = result {
                for done in started {
                    let _ = self.stop_async(&done).await;
                }
                return Err(e.context(format!("starting '{part}' of '{name}'")));
            }
            started.push(part);
        }
        self.segmented.insert(name.to_string(), desc);
        Ok(())
    }

    /// Blocking form of [`spawn_async`](Self::spawn_async).
    pub fn spawn(&mut self, name: &str, desc: Description) -> Result<()> {
        block_on(self.spawn_async(name, desc))
    }

    /// Replace the running flowgraph `name` with `desc`, without stopping
    /// the flowgraphs linked to it: [`prepare_async`](Self::prepare_async)
    /// then [`commit`](Self::commit).
    ///
    /// The new flowgraph is started before the old one lets go of its links
    /// (make before break). Ports are matched by name: every input and output
    /// of the old flowgraph that is linked must exist in `desc` with the same
    /// item type. `hold` decides what happens to input items the old
    /// flowgraph has not taken when the new one takes over.
    ///
    /// If this fails, or the future is dropped before it completes, the old
    /// flowgraph keeps its links.
    pub async fn replace_async(
        &mut self,
        name: &str,
        desc: Description,
        hold: Hold,
    ) -> Result<Replacement> {
        if self.segmented.contains_key(name) {
            return self.replace_swappable(name, desc, hold).await;
        }
        let t0 = Instant::now();
        if !self.flowgraphs.contains_key(name) {
            bail!("no flowgraph '{name}' is running");
        }
        let standby = self.prepare_async(name, desc).await?;
        let (build, start) = (standby.build, standby.start);
        let committing = Instant::now();
        let (old, controls) = self.commit_timed(standby, hold).await?;
        let switched = Instant::now();
        Ok(Replacement {
            old: old.expect("the replaced flowgraph was running"),
            timings: ReplaceTimings {
                build,
                start,
                controls,
                switch: (switched - committing).saturating_sub(controls),
                total: switched - t0,
            },
        })
    }

    /// Replace the swappable blocks of `name` that `desc` changes, each
    /// flowgraph of one replaced like a flowgraph; the rest keeps running.
    /// Returns the last replacement.
    async fn replace_swappable(
        &mut self,
        name: &str,
        desc: Description,
        hold: Hold,
    ) -> Result<Replacement> {
        let old = &self.segmented[name];
        let changed = changed_swappable(old, &desc).ok_or_else(|| {
            anyhow!(
                "'{name}' runs with swappable blocks: a replacement may change only those \
                 (stop it and spawn the new description to change the rest)"
            )
        })?;
        if changed.is_empty() {
            bail!("the description replacing '{name}' changes none of its blocks");
        }
        let split = Split::new(&desc)?;
        let mut last = None;
        for block in &changed {
            let (_, segment) = split
                .segments
                .iter()
                .find(|(b, _)| b == block)
                .expect("a changed swappable block has a segment");
            let part = segment_name(name, block);
            last = Some(Box::pin(self.replace_async(&part, segment.clone(), hold)).await?);
            // What runs now, for the next comparison.
            let running = self.segmented.get_mut(name).unwrap();
            let at = running
                .blocks
                .iter()
                .position(|b| &b.name == block)
                .unwrap();
            running.blocks[at] = desc.blocks[at].clone();
        }
        Ok(last.expect("at least one block changed"))
    }

    /// Blocking form of [`replace_async`](Self::replace_async).
    pub fn replace(&mut self, name: &str, desc: Description, hold: Hold) -> Result<Replacement> {
        block_on(self.replace_async(name, desc, hold))
    }

    /// Check that a flowgraph with ports `kind_of` can replace flowgraph
    /// `name`: it has every port that is linked or tapped, of the same kind.
    fn check_ports(&self, name: &str, kind_of: impl Fn(&str) -> Option<PortKind>) -> Result<()> {
        let old = self
            .flowgraphs
            .get(name)
            .ok_or_else(|| anyhow!("no flowgraph '{name}' is running"))?;
        for port in old.port_names() {
            let key = (name.to_string(), port.clone());
            let used = self.links.contains_key(&key)
                || self.links.values().any(|v| *v == key)
                || self.topics.contains_key(&key);
            if !used {
                continue;
            }
            let kind = old.kind(port).expect("a port of the flowgraph");
            match kind_of(port) {
                Some(k) if k == kind => {}
                Some(k) => {
                    bail!("port '{port}' carries {kind} in '{name}', {k} in the replacement")
                }
                None => bail!("the replacement of '{name}' has no port '{port}'"),
            }
        }
        Ok(())
    }

    /// Check that standby `new` can become flowgraph `name` now: links may
    /// have changed since it was prepared.
    fn check_standby(&self, name: &str, new: &Managed) -> Result<()> {
        let current = |port: &str| {
            let to = (name.to_string(), port.to_string());
            let from = self.links.get(&to)?;
            Some((from, self.subscriptions.get(&to).copied()))
        };
        let changed = |port: &str| {
            anyhow!("input '{port}' of '{name}' was linked after the standby was prepared")
        };
        for input in &new.inputs {
            if let Some((from, sub)) = current(&input.port.name) {
                match self.channels.get(from) {
                    Some(c) if Arc::ptr_eq(c, &input.channel) && sub == Some(input.sub) => {}
                    _ => return Err(changed(&input.port.name)),
                }
            }
        }
        for input in &new.message_inputs {
            if let Some((from, sub)) = current(&input.port.name) {
                match self.topics.get(from) {
                    Some(t) if Arc::ptr_eq(t, &input.topic) && sub == Some(input.sub) => {}
                    _ => return Err(changed(&input.port.name)),
                }
            }
        }
        if self.flowgraphs.contains_key(name) {
            self.check_ports(name, |port| new.kind(port))?;
        }
        Ok(())
    }

    /// Let a replaced flowgraph finish in the background: it drains once
    /// its inputs have ended, and is stopped if that takes longer than the
    /// drain timeout (or at once if it has no inputs to end).
    fn retire(&self, name: &str, old: Managed) -> Retired {
        let (tx, done) = oneshot::channel();
        let timeout = if old.inputs.is_empty() && old.message_inputs.is_empty() {
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
        if let Some(desc) = self.segmented.remove(name) {
            let finished = self.stop_one(name).await;
            for s in &desc.swappable {
                // It may have been stopped by its own name.
                let _ = self.stop_one(&segment_name(name, &s.block)).await;
            }
            self.unsegment(name, &desc);
            return finished;
        }
        self.stop_one(name).await
    }

    /// Undo what running `desc` as flowgraph `name` in segments set up, once
    /// its flowgraphs are gone: the links between them go, and its ports,
    /// with what is linked or tapped there, are the flowgraph's again.
    fn unsegment(&mut self, name: &str, desc: &Description) {
        let Ok(split) = Split::new(desc) else {
            return;
        };
        for (from, output, to, input) in &split.links {
            let _ = self.unlink(&format!("{}.{input}", part_name(name, to)));
            // Its stream ended with the run: a reader of the next one would
            // take that end for its own.
            let out = (part_name(name, from), output.clone());
            self.channels.remove(&out);
            self.topics.remove(&out);
        }
        for (port, block) in &split.moved {
            let from = (segment_name(name, block), port.clone());
            let to = (name.to_string(), port.clone());
            self.aliases.remove(&to);
            self.rekey(&from, &to);
        }
    }

    async fn stop_one(&mut self, name: &str) -> Result<Finished> {
        let managed = self
            .flowgraphs
            .remove(name)
            .ok_or_else(|| anyhow!("no flowgraph '{name}' is running"))?;
        self.applied.remove(name);
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
        if let Some(desc) = self.segmented.remove(name) {
            let finished = self.wait_one(name).await;
            for s in &desc.swappable {
                let _ = self.wait_one(&segment_name(name, &s.block)).await;
            }
            self.unsegment(name, &desc);
            return finished;
        }
        self.wait_one(name).await
    }

    async fn wait_one(&mut self, name: &str) -> Result<Finished> {
        let managed = self
            .flowgraphs
            .remove(name)
            .ok_or_else(|| anyhow!("no flowgraph '{name}' is running"))?;
        self.applied.remove(name);
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

impl Drop for Controller {
    /// Taps end once they have taken what is queued.
    fn drop(&mut self) {
        for topic in self.topics.values() {
            topic.close();
        }
    }
}

/// Start a built flowgraph on standby: its outputs wait until they are
/// committed, and are withdrawn if starting fails or is abandoned.
async fn start(
    runtime: RuntimeHandle,
    fg: Flowgraph,
    blocks: Blocks,
    ends: Ends,
) -> Result<Managed> {
    let guard = StandbyEnds(Some(ends));
    guard.0.as_ref().unwrap().standby();
    let (task, handle) = runtime.start(fg).await?.split();
    Ok(Managed {
        handle,
        task,
        blocks,
        ends: guard.keep(),
    })
}

/// Outputs on standby, withdrawn when dropped unless kept.
struct StandbyEnds(Option<Ends>);

impl StandbyEnds {
    fn keep(mut self) -> Ends {
        self.0.take().unwrap()
    }
}

impl Drop for StandbyEnds {
    fn drop(&mut self) {
        if let Some(ends) = &self.0 {
            ends.withdraw();
        }
    }
}
