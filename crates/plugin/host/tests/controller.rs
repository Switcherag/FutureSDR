//! Linked flowgraphs, and replacing them while the stream goes on.

mod common;

use std::thread::sleep;
use std::time::Duration;

use futuresdr::blocks::MessageSink;
use futuresdr::futures::FutureExt;
use futuresdr::runtime::Timer;
use futuresdr::runtime::dev::prelude::*;
use plugin_api::BlockType;
use plugin_api::Plugin;
use plugin_api::add_kernel;
use plugin_host::Controller;
use plugin_host::Description;
use plugin_host::Finished;
use plugin_host::Hold;
use plugin_host::ReuseCpuReader;
use plugin_host::ReuseCpuWriter;

/// The sinks the registry builds: FutureSDR's, on the buffer the plugins
/// are compiled with.
type VectorSink<T> = futuresdr::blocks::VectorSink<T, ReuseCpuReader<T>>;
type NullSink<T> = futuresdr::blocks::NullSink<T, ReuseCpuReader<T>>;

const N: u64 = 200_000;

/// Counts from `start`, `N` items at `rate` items/s, out of port `samples`.
fn source(start: u64, rate: f64) -> Description {
    Description::from_toml(&format!(
        r#"
        connections = "count > pace"
        [blocks.count]
        type = "Counter<f32>"
        start = {start}
        n = {N}
        chunk = 512
        [blocks.pace]
        type = "Throttle<f32>"
        rate = {rate}
        [outputs]
        samples = "pace.output"
        "#
    ))
    .unwrap()
}

/// Collects port `samples` into block `snk`.
fn receiver() -> Description {
    Description::from_toml(
        r#"
        connections = "copy > snk"
        [blocks.copy]
        type = "Copy<f32>"
        [blocks.snk]
        type = "VectorSink<f32>"
        [inputs]
        samples = "copy.input"
        "#,
    )
    .unwrap()
}

fn items(done: &Finished) -> Vec<u64> {
    done.block::<VectorSink<f32>>("snk")
        .unwrap()
        .items()
        .iter()
        .map(|v| *v as u64)
        .collect()
}

fn controller() -> Controller {
    let mut ctrl = Controller::new(common::registry());
    ctrl.link("src.samples", "rx.samples").unwrap();
    ctrl
}

fn assert_increasing(items: &[u64]) {
    if let Some(w) = items.windows(2).find(|w| w[0] >= w[1]) {
        panic!("out of order or repeated: {} then {}", w[0], w[1]);
    }
}

#[test]
fn a_stream_and_its_end_cross_flowgraphs() {
    let mut ctrl = controller();
    // the receiver may start first
    ctrl.spawn("rx", receiver()).unwrap();
    ctrl.spawn("src", source(0, 2e6)).unwrap();
    ctrl.wait("src").unwrap();
    let rx = ctrl.wait("rx").unwrap();
    assert_eq!(items(&rx), (0..N).collect::<Vec<_>>());
    assert!(ctrl.link_stats("src.samples").unwrap().closed);
}

#[test]
fn replacing_with_keep_loses_nothing() {
    let mut ctrl = controller();
    ctrl.spawn("src", source(0, 400_000.0)).unwrap();
    ctrl.spawn("rx", receiver()).unwrap();
    sleep(Duration::from_millis(150));
    let replaced = ctrl.replace("rx", receiver(), Hold::Keep).unwrap();
    let old = replaced.old.wait().unwrap();
    ctrl.wait("src").unwrap();
    let new = ctrl.wait("rx").unwrap();

    let (old, new) = (items(&old), items(&new));
    assert!(
        !old.is_empty() && !new.is_empty(),
        "{} / {}",
        old.len(),
        new.len()
    );
    let all: Vec<u64> = old.iter().chain(&new).copied().collect();
    assert_eq!(all, (0..N).collect::<Vec<_>>());
}

#[test]
fn replacing_with_discard_drops_but_keeps_order() {
    let mut ctrl = controller();
    ctrl.spawn("src", source(0, 400_000.0)).unwrap();
    ctrl.spawn("rx", receiver()).unwrap();
    sleep(Duration::from_millis(150));
    let replaced = ctrl.replace("rx", receiver(), Hold::Discard).unwrap();
    let old = replaced.old.wait().unwrap();
    ctrl.wait("src").unwrap();
    let new = ctrl.wait("rx").unwrap();

    let all: Vec<u64> = items(&old).into_iter().chain(items(&new)).collect();
    assert_increasing(&all);
    assert_eq!(all.last(), Some(&(N - 1)), "the end of the stream arrives");
    assert!(!items(&old).is_empty());
}

#[test]
fn repeated_replacements_lose_nothing() {
    let mut ctrl = controller();
    ctrl.spawn("src", source(0, 400_000.0)).unwrap();
    ctrl.spawn("rx", receiver()).unwrap();
    let mut retired = Vec::new();
    for _ in 0..8 {
        sleep(Duration::from_millis(40));
        let replaced = ctrl.replace("rx", receiver(), Hold::Keep).unwrap();
        println!("replacement: {:?}", replaced.timings);
        retired.push(replaced.old);
    }
    let mut all = Vec::new();
    for old in retired {
        all.extend(items(&old.wait().unwrap()));
    }
    ctrl.wait("src").unwrap();
    all.extend(items(&ctrl.wait("rx").unwrap()));
    assert_eq!(all, (0..N).collect::<Vec<_>>());
}

#[test]
fn replacing_the_upstream_keeps_the_output_order() {
    let mut ctrl = controller();
    ctrl.spawn("rx", receiver()).unwrap();
    ctrl.spawn("src", source(0, 400_000.0)).unwrap();
    sleep(Duration::from_millis(100));
    // The new source counts on from 10^6; the old one is stopped at once.
    let replaced = ctrl
        .replace("src", source(1_000_000, 2e6), Hold::Keep)
        .unwrap();
    replaced.old.wait().unwrap();
    ctrl.wait("src").unwrap();
    let got = items(&ctrl.wait("rx").unwrap());

    assert_increasing(&got);
    let switch = got
        .iter()
        .position(|v| *v >= 1_000_000)
        .expect("nothing from the new source");
    assert!(switch > 0, "nothing from the old source");
    let new = &got[switch..];
    assert_eq!(
        (new.len(), new.first(), new.last()),
        (N as usize, Some(&1_000_000), Some(&(1_000_000 + N - 1))),
        "the new source's items arrive complete"
    );
}

#[test]
fn mistakes_are_refused_and_leave_things_running() {
    let mut ctrl = controller();
    ctrl.spawn("src", source(0, 400_000.0)).unwrap();
    ctrl.spawn("rx", receiver()).unwrap();

    assert!(ctrl.spawn("rx", receiver()).is_err(), "name taken");
    assert!(
        ctrl.link("src.samples", "rx.other").is_err(),
        "rx is running"
    );

    let unlinked = Description::from_toml(
        r#"
        [blocks.snk]
        type = "NullSink<f32>"
        [inputs]
        other = "snk.input"
        "#,
    )
    .unwrap();
    let err = ctrl.replace("rx", unlinked, Hold::Keep).unwrap_err();
    assert!(format!("{err:#}").contains("no port 'samples'"), "{err:#}");

    let wrong_type = Description::from_toml(
        r#"
        [blocks.snk]
        type = "NullSink<u8>"
        [inputs]
        samples = "snk.input"
        "#,
    )
    .unwrap();
    let err = ctrl.replace("rx", wrong_type, Hold::Keep).unwrap_err();
    assert!(format!("{err:#}").contains("carries f32"), "{err:#}");

    let broken = Description::from_toml(
        r#"
        [blocks.snk]
        type = "VectorSink<f32>"
        capacity = -1
        [inputs]
        samples = "snk.input"
        "#,
    )
    .unwrap();
    assert!(ctrl.replace("rx", broken, Hold::Keep).is_err());

    // Nothing was lost on the way.
    ctrl.wait("src").unwrap();
    assert_eq!(items(&ctrl.wait("rx").unwrap()), (0..N).collect::<Vec<_>>());
}

#[test]
fn a_mismatched_link_is_refused() {
    let mut ctrl = Controller::new(common::registry());
    ctrl.link("src.samples", "rx.samples").unwrap();
    ctrl.spawn("src", source(0, 1e6)).unwrap();
    let bytes = Description::from_toml(
        r#"
        [blocks.snk]
        type = "NullSink<u8>"
        [inputs]
        samples = "snk.input"
        "#,
    )
    .unwrap();
    let err = ctrl.spawn("rx", bytes).unwrap_err();
    assert!(format!("{err:#}").contains("carries f32"), "{err:#}");
    ctrl.stop("src").unwrap();
}

#[test]
fn unlinked_ports_can_be_stopped() {
    let mut ctrl = Controller::new(common::registry());
    ctrl.spawn("rx", receiver()).unwrap();
    ctrl.spawn("src", source(0, 1e6)).unwrap();
    sleep(Duration::from_millis(20));
    let rx = ctrl.stop("rx").unwrap();
    assert!(items(&rx).is_empty());
    ctrl.stop("src").unwrap();
    assert_eq!(ctrl.names().count(), 0);
}

#[test]
fn the_controller_runs_as_a_task_of_its_runtime() {
    let (ctrl, all) = controller().run(|mut ctrl| async move {
        let all = async {
            ctrl.spawn_async("src", source(0, 400_000.0)).await?;
            ctrl.spawn_async("rx", receiver()).await?;
            let mut retired = Vec::new();
            for _ in 0..4 {
                Timer::after(Duration::from_millis(40)).await;
                let replaced = ctrl.replace_async("rx", receiver(), Hold::Keep).await?;
                retired.push(replaced.old);
            }
            let mut all = Vec::new();
            for old in retired {
                all.extend(items(&old.wait_async().await?));
            }
            ctrl.wait_async("src").await?;
            all.extend(items(&ctrl.wait_async("rx").await?));
            anyhow::Ok(all)
        }
        .await;
        (ctrl, all)
    });
    assert_eq!(all.unwrap(), (0..N).collect::<Vec<_>>());
    assert_eq!(ctrl.names().count(), 0);
}

/// Pass-through block that fails to start, or takes a while to.
#[derive(Block)]
struct Gate {
    #[input]
    input: ReuseCpuReader<f32>,
    #[output]
    output: ReuseCpuWriter<f32>,
    fail: bool,
}

impl Gate {
    fn new(fail: bool) -> Self {
        Self {
            input: ReuseCpuReader::default(),
            output: ReuseCpuWriter::default(),
            fail,
        }
    }
}

impl Kernel for Gate {
    async fn init(&mut self, _mo: &mut MessageOutputs, _meta: &BlockMeta) -> Result<()> {
        if self.fail {
            anyhow::bail!("this block never starts");
        }
        Timer::after(Duration::from_millis(50)).await;
        Ok(())
    }

    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let o = self.output.slice();
        let (len, m) = (i.len(), i.len().min(o.len()));
        o[..m].copy_from_slice(&i[..m]);
        self.input.consume(m);
        self.output.produce(m);
        if self.input.finished() && m == len {
            io.finished = true;
        }
        Ok(())
    }
}

/// A controller that also knows `Broken<f32>` and `Slow<f32>` gates.
fn controller_with_gates() -> Controller {
    let mut registry = common::registry();
    registry
        .register(Plugin::new(
            "gates",
            vec![
                BlockType {
                    name: "Broken<f32>".into(),
                    description: "Never starts.",
                    add: |fg, _| add_kernel(fg, Gate::new(true)),
                },
                BlockType {
                    name: "Slow<f32>".into(),
                    description: "Takes 50 ms to start.",
                    add: |fg, _| add_kernel(fg, Gate::new(false)),
                },
            ],
        ))
        .unwrap();
    let mut ctrl = Controller::new(registry);
    ctrl.link("src.samples", "rx.samples").unwrap();
    ctrl
}

/// Counts from `start` through gate `gate`, out of port `samples`.
fn gated_source(start: u64, gate: &str) -> Description {
    Description::from_toml(&format!(
        r#"
        connections = "count > gate"
        [blocks.count]
        type = "Counter<f32>"
        start = {start}
        n = {N}
        [blocks.gate]
        type = "{gate}<f32>"
        [outputs]
        samples = "gate.output"
        "#
    ))
    .unwrap()
}

#[test]
fn a_source_that_fails_to_start_leaves_the_stream_open() {
    let mut ctrl = controller_with_gates();
    ctrl.spawn("rx", receiver()).unwrap();
    let err = ctrl.spawn("src", gated_source(0, "Broken")).unwrap_err();
    assert!(format!("{err:#}").contains("never starts"), "{err:#}");
    sleep(Duration::from_millis(50));
    assert!(!ctrl.link_stats("src.samples").unwrap().closed);

    ctrl.spawn("src", source(0, 2e6)).unwrap();
    ctrl.wait("src").unwrap();
    assert_eq!(items(&ctrl.wait("rx").unwrap()), (0..N).collect::<Vec<_>>());
}

#[test]
fn a_failed_or_abandoned_replacement_leaves_the_old_flowgraph_linked() {
    let mut ctrl = controller_with_gates();
    ctrl.spawn("src", source(0, 400_000.0)).unwrap();
    ctrl.spawn("rx", receiver()).unwrap();
    sleep(Duration::from_millis(50));

    let gated_receiver = |gate: &str| {
        Description::from_toml(&format!(
            r#"
            connections = "gate > snk"
            [blocks.gate]
            type = "{gate}<f32>"
            [blocks.snk]
            type = "VectorSink<f32>"
            [inputs]
            samples = "gate.input"
            "#
        ))
        .unwrap()
    };
    let err = ctrl
        .replace("rx", gated_receiver("Broken"), Hold::Discard)
        .unwrap_err();
    assert!(format!("{err:#}").contains("never starts"), "{err:#}");
    // Polled once, then dropped while the new flowgraphs are starting.
    let abandoned = ctrl
        .replace_async("rx", gated_receiver("Slow"), Hold::Discard)
        .now_or_never();
    assert!(abandoned.is_none());
    let abandoned = ctrl
        .replace_async("src", gated_source(1_000_000, "Slow"), Hold::Keep)
        .now_or_never();
    assert!(abandoned.is_none());
    sleep(Duration::from_millis(100));

    ctrl.wait("src").unwrap();
    let got = items(&ctrl.wait("rx").unwrap());
    assert_increasing(&got);
    assert_eq!(
        got.last(),
        Some(&(N - 1)),
        "the old source still feeds the old receiver, to the end"
    );
    assert!(
        got.len() as u64 > N / 2,
        "only a moment's worth is dropped: {} items",
        got.len()
    );
}

#[test]
fn a_standby_source_writes_nothing_until_committed() {
    let mut ctrl = controller();
    ctrl.spawn("rx", receiver()).unwrap();
    let standby = ctrl.prepare("src", source(0, 2e6)).unwrap();
    assert_eq!(ctrl.names().collect::<Vec<_>>(), ["rx"]);
    sleep(Duration::from_millis(50));
    let stats = ctrl.link_stats("src.samples").unwrap();
    assert_eq!((stats.queued, stats.closed), (0, false));

    assert!(ctrl.commit(standby, Hold::Keep).unwrap().is_none());
    ctrl.wait("src").unwrap();
    assert_eq!(items(&ctrl.wait("rx").unwrap()), (0..N).collect::<Vec<_>>());
}

#[test]
fn committing_prepared_standbys_loses_nothing() {
    let mut ctrl = controller();
    ctrl.spawn("src", source(0, 400_000.0)).unwrap();
    ctrl.spawn("rx", receiver()).unwrap();
    let mut standby = ctrl.prepare("rx", receiver()).unwrap();
    let mut retired = Vec::new();
    for _ in 0..8 {
        sleep(Duration::from_millis(40));
        let t = std::time::Instant::now();
        retired.push(ctrl.commit(standby, Hold::Keep).unwrap().unwrap());
        println!("commit: {:?}", t.elapsed());
        standby = ctrl.prepare("rx", receiver()).unwrap();
    }
    drop(standby);
    let mut all = Vec::new();
    for old in retired {
        all.extend(items(&old.wait().unwrap()));
    }
    ctrl.wait("src").unwrap();
    all.extend(items(&ctrl.wait("rx").unwrap()));
    assert_eq!(all, (0..N).collect::<Vec<_>>());
}

#[test]
fn a_dropped_standby_changes_nothing() {
    let mut ctrl = controller();
    ctrl.spawn("src", source(0, 400_000.0)).unwrap();
    ctrl.spawn("rx", receiver()).unwrap();
    for _ in 0..3 {
        drop(ctrl.prepare("rx", receiver()).unwrap());
        drop(ctrl.prepare("src", source(1_000_000, 2e6)).unwrap());
    }
    ctrl.wait("src").unwrap();
    assert_eq!(items(&ctrl.wait("rx").unwrap()), (0..N).collect::<Vec<_>>());
}

#[test]
fn a_standby_whose_input_was_linked_since_is_refused() {
    let mut ctrl = Controller::new(common::registry());
    let standby = ctrl.prepare("rx", receiver()).unwrap();
    ctrl.link("src.samples", "rx.samples").unwrap();
    let err = ctrl.commit(standby, Hold::Keep).unwrap_err();
    assert!(format!("{err:#}").contains("linked after"), "{err:#}");
    assert_eq!(ctrl.names().count(), 0);

    // Prepared after linking, it works.
    let standby = ctrl.prepare("rx", receiver()).unwrap();
    ctrl.commit(standby, Hold::Keep).unwrap();
    ctrl.spawn("src", source(0, 2e6)).unwrap();
    ctrl.wait("src").unwrap();
    assert_eq!(items(&ctrl.wait("rx").unwrap()), (0..N).collect::<Vec<_>>());
}

/// Counts from `start`, `n` f64 items at `rate` items/s, out of `samples`.
fn f64_source(start: u64, n: u64, rate: f64) -> Description {
    Description::from_toml(&format!(
        r#"
        connections = "count > pace"
        [blocks.count]
        type = "Counter<f64>"
        start = {start}
        n = {n}
        chunk = 256
        [blocks.pace]
        type = "Throttle<f64>"
        rate = {rate}
        [outputs]
        samples = "pace.output"
        "#
    ))
    .unwrap()
}

fn f64_receiver() -> Description {
    Description::from_toml(
        r#"
        connections = "copy > snk"
        [blocks.copy]
        type = "Copy<f64>"
        [blocks.snk]
        type = "VectorSink<f64>"
        [inputs]
        samples = "copy.input"
        "#,
    )
    .unwrap()
}

fn f64_items(done: &Finished) -> Vec<u64> {
    done.block::<VectorSink<f64>>("snk")
        .unwrap()
        .items()
        .iter()
        .map(|v| *v as u64)
        .collect()
}

const SOURCE_SPAN: u64 = 10_000_000;

/// Random replacements of both ends, on demand or through standbys, some
/// standbys dropped, from a task or from this thread. With `Hold::Keep`, the
/// receivers together get, in commit order, a gapless run of every source's
/// items, all of them for the last source.
#[test]
fn random_replacements_keep_every_item() {
    common::test_rng::for_each_seed(3, |rng| {
        let mut ctrl = controller();
        let (mut next_source, last_items) = (0u64, 20_000u64);
        // Long sources: they are replaced before they finish.
        ctrl.spawn("src", f64_source(0, 100 * SOURCE_SPAN / 1000, 300_000.0))
            .unwrap();
        ctrl.spawn("rx", f64_receiver()).unwrap();

        let steps: Vec<usize> = (0..rng.range(4, 10)).map(|_| rng.below(6)).collect();
        let pauses: Vec<u64> = steps.iter().map(|_| rng.range(0, 25) as u64).collect();
        let in_task = rng.chance(50);
        let scenario = move |mut ctrl: Controller| async move {
            let mut retired = Vec::new();
            for (step, pause) in steps.iter().zip(pauses) {
                Timer::after(Duration::from_millis(pause)).await;
                match step {
                    0 => retired.push(
                        ctrl.replace_async("rx", f64_receiver(), Hold::Keep)
                            .await?
                            .old,
                    ),
                    1 => {
                        let standby = ctrl.prepare_async("rx", f64_receiver()).await?;
                        Timer::after(Duration::from_millis(pause)).await;
                        retired.extend(ctrl.commit_async(standby, Hold::Keep).await?);
                    }
                    2 | 3 => {
                        next_source += 1;
                        let desc = f64_source(next_source * SOURCE_SPAN, 100_000_000, 300_000.0);
                        if *step == 2 {
                            ctrl.replace_async("src", desc, Hold::Keep).await?;
                        } else {
                            let standby = ctrl.prepare_async("src", desc).await?;
                            ctrl.commit_async(standby, Hold::Keep).await?;
                        }
                    }
                    4 => drop(ctrl.prepare_async("rx", f64_receiver()).await?),
                    _ => drop(
                        ctrl.prepare_async("src", f64_source(99 * SOURCE_SPAN, 10, 1.0))
                            .await?,
                    ),
                }
            }
            // A short last source ends the stream.
            next_source += 1;
            let last = f64_source(next_source * SOURCE_SPAN, last_items, 2e6);
            ctrl.replace_async("src", last, Hold::Keep).await?;
            ctrl.wait_async("src").await?;
            let mut all = Vec::new();
            for old in retired {
                all.extend(f64_items(&old.wait_async().await?));
            }
            all.extend(f64_items(&ctrl.wait_async("rx").await?));
            anyhow::Ok((all, next_source))
        };
        let (all, last_source) = if in_task {
            ctrl.run(|ctrl| async move { scenario(ctrl).await })
        } else {
            futuresdr::runtime::block_on(scenario(ctrl))
        }
        .unwrap();

        let mut expected_source = 0;
        let mut next = 0;
        for item in &all {
            let (source, offset) = (item / SOURCE_SPAN, item % SOURCE_SPAN);
            if source != expected_source {
                assert!(
                    source > expected_source,
                    "source {source} after {expected_source}"
                );
                assert!(source < 99, "an item of a dropped standby");
                expected_source = source;
                next = 0;
            }
            assert_eq!(
                offset, next,
                "source {source}: item {offset} instead of {next}"
            );
            next += 1;
        }
        assert_eq!(expected_source, last_source, "the last source is heard");
        assert_eq!(next, last_items, "the last source is complete");
    });
}

#[test]
fn a_standby_takes_messages_before_it_goes_live() {
    let mut ctrl = Controller::new(common::registry());
    let desc = Description::from_toml(
        r#"
        [blocks.sink]
        type = "MessageSink"
        "#,
    )
    .unwrap();
    let standby = ctrl.prepare("ctl", desc).unwrap();
    let handle = standby.handle();
    let sink = standby.blocks().id("sink").unwrap();
    for i in 0..3 {
        futuresdr::runtime::block_on(handle.call(sink, "in", Pmt::U32(i))).unwrap();
    }
    ctrl.commit(standby, Hold::Keep).unwrap();
    let done = ctrl.stop("ctl").unwrap();
    assert_eq!(done.block::<MessageSink>("sink").unwrap().received(), 3);
}

#[test]
fn dropping_the_controller_stops_everything() {
    let mut ctrl = controller();
    ctrl.spawn("src", source(0, 1e6)).unwrap();
    ctrl.spawn("rx", receiver()).unwrap();
    let standby = ctrl.prepare("rx", receiver()).unwrap();
    sleep(Duration::from_millis(20));
    // Only the drops are timed (building the plugin can take long).
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(ctrl);
        drop(standby);
        tx.send(()).unwrap();
    });
    rx.recv_timeout(Duration::from_secs(10))
        .expect("dropping the controller hangs");
}

#[test]
fn accessors_and_mistaken_names() {
    let mut ctrl = controller();
    ctrl.set_link_capacity(0);
    assert!(ctrl.registry().get("Copy<f32>").is_some());
    assert!(ctrl.registry_mut().type_names().count() > 70);
    let _ = ctrl.runtime();
    for bad in ["nodot", ""] {
        assert!(ctrl.link(bad, "rx2.samples").is_err());
        assert!(ctrl.link("src2.samples", bad).is_err());
        assert!(ctrl.link_stats(bad).is_none());
    }
    let err = ctrl.link("src.samples", "rx.samples").unwrap_err();
    assert!(err.to_string().contains("already linked"), "{err}");
    assert!(ctrl.link_stats("src.samples").is_none(), "no channel yet");
    for name in ["nobody", ""] {
        assert!(ctrl.stop(name).is_err());
        assert!(ctrl.wait(name).is_err());
        assert!(ctrl.replace(name, receiver(), Hold::Keep).is_err());
        assert!(ctrl.handle(name).is_none() && ctrl.blocks(name).is_none());
    }

    ctrl.spawn("rx", receiver()).unwrap();
    assert_eq!(ctrl.names().collect::<Vec<_>>(), ["rx"]);
    assert!(ctrl.handle("rx").is_some());
    assert!(ctrl.blocks("rx").unwrap().id("snk").is_some());
    let standby = ctrl.prepare("rx", receiver()).unwrap();
    assert_eq!(standby.name(), "rx");
    assert!(standby.build_time() + standby.start_time() > Duration::ZERO);
    assert!(format!("{standby:?}").contains("rx"));
    let old = ctrl.commit(standby, Hold::Keep).unwrap().unwrap();
    assert_eq!(old.name(), "rx");
    assert!(format!("{old:?}").contains("rx"));
    let done = ctrl.stop("rx").unwrap();
    assert!(format!("{done:?}").starts_with("Finished"));
    assert!(done.block::<VectorSink<f32>>("nope").is_err());
    assert!(
        done.block::<NullSink<f32>>("snk").is_err(),
        "another kernel"
    );
    assert!(items(&old.wait().unwrap()).is_empty());
}

#[test]
fn a_replaced_flowgraph_that_does_not_finish_is_stopped_after_the_drain_timeout() {
    let mut ctrl = controller();
    ctrl.set_drain_timeout(Duration::from_millis(100));
    // The message sink never finishes by itself.
    let stubborn = Description::from_toml(
        r#"
        connections = "copy > snk"
        [blocks.copy]
        type = "Copy<f32>"
        [blocks.snk]
        type = "VectorSink<f32>"
        [blocks.ctl]
        type = "MessageSink"
        [inputs]
        samples = "copy.input"
        "#,
    )
    .unwrap();
    ctrl.spawn("src", source(0, 400_000.0)).unwrap();
    ctrl.spawn("rx", stubborn).unwrap();
    sleep(Duration::from_millis(50));
    let t = std::time::Instant::now();
    let old = ctrl.replace("rx", receiver(), Hold::Keep).unwrap().old;
    let done = old.wait().unwrap();
    let waited = t.elapsed();
    assert!(
        waited >= Duration::from_millis(100) && waited < Duration::from_secs(2),
        "{waited:?}"
    );
    assert!(!items(&done).is_empty());
    ctrl.stop("src").unwrap();
    ctrl.stop("rx").unwrap();
}

#[test]
fn a_full_link_drops_the_oldest_items() {
    let mut ctrl = controller();
    ctrl.set_link_capacity(1000);
    // Nobody reads yet.
    ctrl.spawn("src", source(0, 2e6)).unwrap();
    ctrl.wait("src").unwrap();
    let stats = ctrl.link_stats("src.samples").unwrap();
    assert_eq!(
        (stats.queued, stats.dropped, stats.closed),
        (1000, N - 1000, true)
    );
    ctrl.spawn("rx", receiver()).unwrap();
    assert_eq!(
        items(&ctrl.wait("rx").unwrap()),
        (N - 1000..N).collect::<Vec<_>>(),
        "the newest items are kept"
    );
}

/// What a replacement does with the items the old flowgraph left on its
/// input: a receiver that stopped consuming leaves the stream queued.
#[test]
fn hold_decides_what_happens_to_what_the_old_flowgraph_left() {
    // `Head` with nothing to forward never consumes: once its input buffer
    // is full, the bridge takes nothing more.
    let stalled = || {
        Description::from_toml(
            r#"
            connections = "head > snk"
            [blocks.head]
            type = "Head<f32>"
            n_items = 0
            [blocks.snk]
            type = "NullSink<f32>"
            [inputs]
            samples = "head.input"
            "#,
        )
        .unwrap()
    };
    for hold in [Hold::Keep, Hold::Discard] {
        let mut ctrl = controller();
        ctrl.set_drain_timeout(Duration::from_millis(20));
        ctrl.spawn("rx", stalled()).unwrap();
        ctrl.spawn("src", source(0, 2e6)).unwrap();
        ctrl.wait("src").unwrap();
        // Wait until the stalled receiver has filled its buffer.
        let queued = || ctrl.link_stats("src.samples").unwrap().queued;
        let since = std::time::Instant::now();
        while queued() == N as usize {
            assert!(since.elapsed() < Duration::from_secs(10), "nothing taken");
            sleep(Duration::from_millis(1));
        }
        let left = queued();
        sleep(Duration::from_millis(20));
        assert_eq!(queued(), left, "the stalled receiver takes nothing more");

        let replaced = ctrl.replace("rx", receiver(), hold).unwrap();
        replaced.old.wait().unwrap();
        let got = items(&ctrl.wait("rx").unwrap());
        match hold {
            Hold::Keep => assert_eq!(got, (N - left as u64..N).collect::<Vec<_>>()),
            Hold::Discard => assert!(got.is_empty(), "{} items", got.len()),
        }
    }
}
