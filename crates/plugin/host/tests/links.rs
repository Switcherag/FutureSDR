//! One output feeding several inputs, parked and selected links, messages
//! between flowgraphs and taps, and the controls flowgraphs ask for.

mod common;

use std::collections::BTreeMap;
use std::thread::sleep;
use std::time::Duration;
use std::time::Instant;

use futuresdr::blocks::VectorSink;
use futuresdr::runtime::Timer;
use futuresdr::runtime::dev::prelude::*;
use plugin_api::BlockType;
use plugin_api::Plugin;
use plugin_api::add_kernel;
use plugin_host::Controller;
use plugin_host::Description;
use plugin_host::Finished;
use plugin_host::Hold;
use plugin_host::Tap;

const N: u64 = 200_000;

/// Posts `U64(i)` for `i` in `start..start + n`, one every `every_us`
/// microseconds.
#[derive(Block)]
#[message_outputs(out)]
struct Numbers {
    next: u64,
    end: u64,
    every: Duration,
}

impl Kernel for Numbers {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        if self.next == self.end {
            io.finished = true;
            return Ok(());
        }
        mo.post("out", Pmt::U64(self.next)).await?;
        self.next += 1;
        if !self.every.is_zero() {
            Timer::after(self.every).await;
        }
        io.call_again = true;
        Ok(())
    }
}

/// Keeps the numbers posted to `in`; ends when the sender does.
#[derive(Block)]
#[message_inputs(r#in)]
struct Collect {
    got: Vec<u64>,
}

impl Collect {
    async fn r#in(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        match p {
            Pmt::U64(v) => self.got.push(v),
            Pmt::Finished => io.finished = true,
            _ => {}
        }
        Ok(Pmt::Ok)
    }
}

impl Kernel for Collect {}

/// Emits its frequency, 64 items a millisecond; message input `freq` sets
/// it in 10 ms (negative values are refused).
#[derive(Block)]
#[message_inputs(freq)]
struct Tuner {
    #[output]
    output: DefaultCpuWriter<f32>,
    freq: f32,
    calls: usize,
    timer: Option<Timer>,
}

impl Tuner {
    async fn freq(
        &mut self,
        _io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        self.calls += 1;
        Timer::after(Duration::from_millis(10)).await;
        match p {
            Pmt::F64(f) if f >= 0.0 => {
                self.freq = f as f32;
                Ok(Pmt::Ok)
            }
            _ => Ok(Pmt::InvalidValue),
        }
    }
}

impl Kernel for Tuner {
    type BlockOn = Timer;

    fn block_on(&mut self) -> Option<std::pin::Pin<&mut Timer>> {
        self.timer.as_mut().map(std::pin::Pin::new)
    }

    async fn work(
        &mut self,
        _io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let o = self.output.slice();
        let m = o.len().min(64);
        o[..m].fill(self.freq);
        self.output.produce(m);
        self.timer = Some(Timer::after(Duration::from_millis(1)));
        Ok(())
    }
}

fn controller() -> Controller {
    let mut registry = common::registry();
    registry
        .register(Plugin::new(
            "links-test",
            vec![
                BlockType {
                    name: "Numbers".into(),
                    description: "Numbered messages.",
                    add: |fg, s| {
                        add_kernel(
                            fg,
                            Numbers {
                                next: s.get("start")?,
                                end: s.get::<u64>("start")? + s.get::<u64>("n")?,
                                every: Duration::from_micros(s.get_or("every_us", 0)?),
                            },
                        )
                    },
                },
                BlockType {
                    name: "Collect".into(),
                    description: "Keeps numbers.",
                    add: |fg, _| add_kernel(fg, Collect { got: Vec::new() }),
                },
                BlockType {
                    name: "Tuner<f32>".into(),
                    description: "Items become the frequency.",
                    add: |fg, s| {
                        add_kernel(
                            fg,
                            Tuner {
                                output: DefaultCpuWriter::default(),
                                freq: s.get_or("freq", 0.0)?,
                                calls: 0,
                                timer: None,
                            },
                        )
                    },
                },
            ],
        ))
        .unwrap();
    Controller::new(registry)
}

fn desc(text: &str) -> Description {
    Description::from_toml(text).unwrap()
}

/// Counts from 0, `N` items at `rate` items/s, out of port `samples`.
fn source(rate: f64) -> Description {
    desc(&format!(
        r#"
        connections = "count > pace"
        [blocks.count]
        type = "Counter<f32>"
        n = {N}
        chunk = 512
        [blocks.pace]
        type = "Throttle<f32>"
        rate = {rate}
        [outputs]
        samples = "pace.output"
        "#
    ))
}

/// Collects port `samples` into block `snk`, asking for `radio`.
fn receiver(radio: &str) -> Description {
    desc(&format!(
        r#"
        connections = "copy > snk"
        [blocks.copy]
        type = "Copy<f32>"
        [blocks.snk]
        type = "VectorSink<f32>"
        [inputs]
        samples = "copy.input"
        [radio]
        {radio}
        "#
    ))
}

fn items(done: &Finished) -> Vec<f32> {
    done.block::<VectorSink<f32>>("snk")
        .unwrap()
        .items()
        .clone()
}

fn numbers(done: &Finished) -> Vec<u64> {
    items(done).iter().map(|v| *v as u64).collect()
}

fn assert_increasing(items: &[u64]) {
    if let Some(w) = items.windows(2).find(|w| w[0] >= w[1]) {
        panic!("out of order or repeated: {} then {}", w[0], w[1]);
    }
}

#[test]
fn one_output_feeds_several_inputs() {
    let mut ctrl = controller();
    for rx in ["a", "b", "c"] {
        ctrl.link("src.samples", &format!("{rx}.samples")).unwrap();
    }
    ctrl.spawn("a", receiver("")).unwrap();
    ctrl.spawn("src", source(2e6)).unwrap();
    // A late receiver gets what was queued for it.
    ctrl.spawn("b", receiver("")).unwrap();
    ctrl.wait("src").unwrap();
    ctrl.spawn("c", receiver("")).unwrap();
    for rx in ["a", "b", "c"] {
        assert_eq!(
            numbers(&ctrl.wait(rx).unwrap()),
            (0..N).collect::<Vec<_>>(),
            "{rx}"
        );
    }
    let stats = ctrl.link_stats("src.samples").unwrap();
    assert!(stats.closed && !stats.parked && stats.queued == 0);
}

#[test]
fn selecting_a_link_moves_the_stream_at_once() {
    for hold in [Hold::Keep, Hold::Discard] {
        let mut ctrl = controller();
        ctrl.link("src.samples", "a.samples").unwrap();
        ctrl.link("src.samples", "b.samples").unwrap();
        ctrl.park("b.samples", Hold::Keep).unwrap();
        ctrl.spawn("a", receiver("")).unwrap();
        ctrl.spawn("b", receiver("")).unwrap();
        ctrl.spawn("src", source(400_000.0)).unwrap();
        assert!(ctrl.link_stats("b.samples").unwrap().parked);
        assert!(!ctrl.link_stats("src.samples").unwrap().parked);
        let mut turn = 0;
        while !ctrl.link_stats("src.samples").unwrap().closed {
            sleep(Duration::from_millis(40));
            turn += 1;
            let rx = if turn % 2 == 1 { "b" } else { "a" };
            ctrl.select(&format!("{rx}.samples"), hold).unwrap();
        }
        assert!(turn >= 4, "{turn} turns");
        let a = numbers(&ctrl.wait("a").unwrap());
        let b = numbers(&ctrl.wait("b").unwrap());
        assert_increasing(&a);
        assert_increasing(&b);
        assert!(!a.is_empty() && !b.is_empty());
        let mut all: Vec<u64> = a.iter().chain(&b).copied().collect();
        all.sort();
        assert_increasing(&all);
        if hold == Hold::Keep {
            assert_eq!(all, (0..N).collect::<Vec<_>>(), "nothing lost");
        }
    }
}

#[test]
fn a_parked_link_holds_nothing_and_unparks() {
    let mut ctrl = controller();
    ctrl.link("src.samples", "rx.samples").unwrap();
    ctrl.spawn("src", source(2e6)).unwrap();
    // Parked once items are queued: Discard drops them.
    sleep(Duration::from_millis(20));
    ctrl.park("rx.samples", Hold::Discard).unwrap();
    ctrl.wait("src").unwrap();
    let stats = ctrl.link_stats("rx.samples").unwrap();
    assert_eq!((stats.queued, stats.parked, stats.closed), (0, true, true));
    ctrl.unpark("rx.samples").unwrap();
    assert!(!ctrl.link_stats("rx.samples").unwrap().parked);
    ctrl.spawn("rx", receiver("")).unwrap();
    assert!(numbers(&ctrl.wait("rx").unwrap()).is_empty());

    for bad in ["nodot", "rx.other", "other.samples"] {
        assert!(ctrl.park(bad, Hold::Keep).is_err(), "{bad}");
        assert!(ctrl.unpark(bad).is_err(), "{bad}");
        assert!(ctrl.select(bad, Hold::Keep).is_err(), "{bad}");
        assert!(ctrl.unlink(bad).is_err(), "{bad}");
    }
}

#[test]
fn links_can_be_removed_while_their_input_does_not_run() {
    let mut ctrl = controller();
    ctrl.link("src.samples", "a.samples").unwrap();
    ctrl.link("src.samples", "b.samples").unwrap();
    ctrl.link("src.samples", "c.samples").unwrap();
    ctrl.park("b.samples", Hold::Keep).unwrap();
    ctrl.spawn("src", source(2e6)).unwrap();
    ctrl.spawn("a", receiver("")).unwrap();
    assert!(ctrl.unlink("a.samples").is_err(), "a is running");
    ctrl.wait("src").unwrap();
    assert_eq!(ctrl.link_stats("c.samples").unwrap().queued as u64, N);

    // Linked again, with a queue of its own that has nothing yet.
    for rx in ["b", "c"] {
        let port = format!("{rx}.samples");
        ctrl.unlink(&port).unwrap();
        assert!(ctrl.link_stats(&port).is_none());
        ctrl.link("src.samples", &port).unwrap();
        let stats = ctrl.link_stats(&port).unwrap();
        assert_eq!((stats.queued, stats.parked), (0, false), "{rx}");
        ctrl.spawn(rx, receiver("")).unwrap();
    }
    assert_eq!(numbers(&ctrl.wait("a").unwrap()).len() as u64, N);
    assert!(numbers(&ctrl.wait("b").unwrap()).is_empty());
    assert!(numbers(&ctrl.wait("c").unwrap()).is_empty());
}

/// Numbered messages out of message output `numbers`.
fn numbers_source(start: u64, n: u64, every_us: u64) -> Description {
    desc(&format!(
        r#"
        [blocks.tx]
        type = "Numbers"
        start = {start}
        n = {n}
        every_us = {every_us}
        [message_outputs]
        numbers = "tx.out"
        "#
    ))
}

/// Collects message input `numbers` in block `rx`.
fn numbers_receiver() -> Description {
    desc(
        r#"
        [blocks.rx]
        type = "Collect"
        [message_inputs]
        numbers = "rx.in"
        "#,
    )
}

fn collected(done: &Finished) -> Vec<u64> {
    done.block::<Collect>("rx").unwrap().got.clone()
}

/// Take `n` messages from `tap`, within ten seconds.
fn take(tap: &mut Tap, n: usize) -> Vec<u64> {
    let since = Instant::now();
    let mut got = Vec::new();
    while got.len() < n {
        assert!(since.elapsed() < Duration::from_secs(10), "{got:?}");
        match tap.try_recv() {
            Some(Pmt::U64(v)) => got.push(v),
            Some(other) => panic!("{other:?}"),
            None => sleep(Duration::from_millis(1)),
        }
    }
    got
}

#[test]
fn messages_cross_flowgraphs_through_replacements() {
    const M: u64 = 3000;
    let mut ctrl = controller();
    ctrl.set_drain_timeout(Duration::from_secs(5));
    ctrl.link("tx.numbers", "rx.numbers").unwrap();
    let mut tap = ctrl.tap("tx.numbers").unwrap();
    assert_eq!(tap.name(), "tx.numbers");
    ctrl.spawn("rx", numbers_receiver()).unwrap();
    ctrl.spawn("tx", numbers_source(0, M, 100)).unwrap();

    let mut got = Vec::new();
    for _ in 0..3 {
        sleep(Duration::from_millis(40));
        let replaced = ctrl.replace("rx", numbers_receiver(), Hold::Keep).unwrap();
        got.extend(collected(&replaced.old.wait().unwrap()));
    }
    ctrl.wait("tx").unwrap();
    let since = Instant::now();
    while ctrl.link_stats("rx.numbers").unwrap().queued > 0 {
        assert!(since.elapsed() < Duration::from_secs(10));
        sleep(Duration::from_millis(1));
    }
    sleep(Duration::from_millis(20));
    got.extend(collected(&ctrl.stop("rx").unwrap()));
    assert_eq!(
        got,
        (0..M).collect::<Vec<_>>(),
        "every message, once, in order"
    );
    assert_eq!(take(&mut tap, M as usize), (0..M).collect::<Vec<_>>());
    assert_eq!(tap.stats(), (0, 0));

    // A standby publishes once committed; a dropped one never does.
    let dropped = ctrl.prepare("tx", numbers_source(500, 5, 0)).unwrap();
    let standby = ctrl.prepare("tx", numbers_source(1000, 5, 0)).unwrap();
    sleep(Duration::from_millis(30));
    assert!(tap.try_recv().is_none());
    drop(dropped);
    ctrl.commit(standby, Hold::Keep).unwrap();
    assert_eq!(take(&mut tap, 5), (1000..1005).collect::<Vec<_>>());
    sleep(Duration::from_millis(30));
    assert!(tap.try_recv().is_none(), "nothing of the dropped standby");

    // Messages say which flowgraph posted them: its description's name, or
    // the name it runs under.
    let mut named = numbers_source(2000, 1, 0);
    named.name = Some("numbers v2".into());
    ctrl.replace("tx", named, Hold::Keep).unwrap();
    ctrl.wait("tx").unwrap();
    ctrl.spawn("tx", numbers_source(3000, 1, 0)).unwrap();
    ctrl.wait("tx").unwrap();
    let since = Instant::now();
    let mut from = Vec::new();
    while from.len() < 2 {
        assert!(since.elapsed() < Duration::from_secs(10));
        match tap.try_recv_from() {
            Some((origin, pmt)) => from.push((origin.to_string(), pmt)),
            None => sleep(Duration::from_millis(1)),
        }
    }
    assert_eq!(
        from,
        [
            ("numbers v2".to_string(), Pmt::U64(2000)),
            ("tx".to_string(), Pmt::U64(3000)),
        ]
    );

    // The tap ends with the controller.
    let waiting = std::thread::spawn(move || futuresdr::runtime::block_on(tap.recv()));
    sleep(Duration::from_millis(20));
    drop(ctrl);
    assert_eq!(waiting.join().unwrap(), None);
}

#[test]
fn a_parked_message_link_gets_nothing() {
    let mut ctrl = controller();
    ctrl.link("tx.numbers", "a.numbers").unwrap();
    ctrl.link("tx.numbers", "b.numbers").unwrap();
    let mut tap = ctrl.tap("tx.numbers").unwrap();
    ctrl.spawn("a", numbers_receiver()).unwrap();
    ctrl.spawn("b", numbers_receiver()).unwrap();
    ctrl.select("a.numbers", Hold::Discard).unwrap();
    ctrl.spawn("tx", numbers_source(0, 10, 0)).unwrap();
    assert_eq!(
        take(&mut tap, 10),
        (0..10).collect::<Vec<_>>(),
        "taps are not parked"
    );
    ctrl.wait("tx").unwrap();
    sleep(Duration::from_millis(30));
    let stats = ctrl.link_stats("b.numbers").unwrap();
    assert_eq!((stats.queued, stats.parked), (0, true));
    ctrl.unpark("b.numbers").unwrap();
    ctrl.park("a.numbers", Hold::Keep).unwrap();
    ctrl.spawn("tx", numbers_source(10, 10, 0)).unwrap();
    ctrl.wait("tx").unwrap();
    sleep(Duration::from_millis(30));
    assert_eq!(
        collected(&ctrl.stop("a").unwrap()),
        (0..10).collect::<Vec<_>>()
    );
    assert_eq!(
        collected(&ctrl.stop("b").unwrap()),
        (10..20).collect::<Vec<_>>()
    );
}

#[test]
fn stream_and_message_ports_do_not_mix() {
    let mut ctrl = controller();
    ctrl.link("tx.numbers", "rx.samples").unwrap();
    ctrl.spawn("tx", numbers_source(0, 1, 0)).unwrap();
    let err = ctrl.spawn("rx", receiver("")).unwrap_err();
    assert!(format!("{err:#}").contains("message output"), "{err:#}");

    let mut ctrl = controller();
    ctrl.link("src.samples", "rx.numbers").unwrap();
    ctrl.spawn("src", source(1e6)).unwrap();
    let err = ctrl.spawn("rx", numbers_receiver()).unwrap_err();
    assert!(format!("{err:#}").contains("stream output"), "{err:#}");
    let err = ctrl.tap("src.samples").unwrap_err();
    assert!(format!("{err:#}").contains("stream output"), "{err:#}");
    assert!(ctrl.tap("nodot").is_err());

    // A replacement keeps the kind of the ports that are linked or tapped.
    let mut ctrl = controller();
    let _tap = ctrl.tap("tx.numbers").unwrap();
    ctrl.spawn("tx", numbers_source(0, 1, 100_000)).unwrap();
    let stream = desc(
        r#"
        [blocks.snk]
        type = "NullSource<f32>"
        [outputs]
        numbers = "snk.output"
        "#,
    );
    let err = ctrl.replace("tx", stream, Hold::Keep).unwrap_err();
    assert!(format!("{err:#}").contains("carries messages"), "{err:#}");
    let none = desc("[blocks.snk]\ntype = \"NullSource<f32>\"");
    let err = ctrl.replace("tx", none, Hold::Keep).unwrap_err();
    assert!(format!("{err:#}").contains("no port 'numbers'"), "{err:#}");
}

/// A source whose items are its frequency, which it offers as control
/// `frequency`.
fn tuned_source(freq: f64) -> Description {
    desc(&format!(
        r#"
        [blocks.tuner]
        type = "Tuner<f32>"
        freq = {freq}
        [outputs]
        samples = "tuner.output"
        [controls]
        frequency = "tuner.freq"
        "#
    ))
}

fn tuner_calls(done: &Finished) -> usize {
    done.block::<Tuner>("tuner").unwrap().calls
}

fn frequencies(done: &Finished) -> Vec<f32> {
    let mut f = items(done);
    f.dedup();
    f
}

fn controls(value: f64) -> BTreeMap<String, Pmt> {
    BTreeMap::from([("frequency".to_string(), Pmt::F64(value))])
}

#[test]
fn receivers_retune_their_source_before_they_switch() {
    let mut ctrl = controller();
    ctrl.link("src.samples", "rx.samples").unwrap();
    // Asked before the source runs: set before its output goes live.
    ctrl.spawn("rx", receiver("frequency = 1")).unwrap();
    ctrl.spawn("src", tuned_source(0.0)).unwrap();
    assert_eq!(ctrl.controls("src"), controls(1.0));
    sleep(Duration::from_millis(50));

    let replaced = ctrl
        .replace("rx", receiver("frequency = 2.0"), Hold::Discard)
        .unwrap();
    assert!(replaced.timings.controls >= Duration::from_millis(10));
    assert_eq!(frequencies(&replaced.old.wait().unwrap()), [1.0]);
    assert_eq!(ctrl.controls("src"), controls(2.0));

    // Same frequency: nothing to set.
    sleep(Duration::from_millis(50));
    let replaced = ctrl
        .replace("rx", receiver("frequency = 2 # again"), Hold::Discard)
        .unwrap();
    assert!(replaced.timings.controls < Duration::from_millis(10));
    assert_eq!(frequencies(&replaced.old.wait().unwrap()), [2.0]);

    // A new source gets what the old one was asked for.
    sleep(Duration::from_millis(50));
    let replaced = ctrl.replace("src", tuned_source(0.0), Hold::Keep).unwrap();
    assert_eq!(tuner_calls(&replaced.old.wait().unwrap()), 2);
    sleep(Duration::from_millis(50));

    // A refused value fails the replacement, which changes nothing.
    let err = ctrl
        .replace("rx", receiver("frequency = -1"), Hold::Discard)
        .unwrap_err();
    assert!(format!("{err:#}").contains("not a valid value"), "{err:#}");
    assert_eq!(ctrl.controls("src"), controls(2.0));

    // Settings nobody offers are ignored.
    ctrl.replace("rx", receiver("frequency = 2\ngain = 30"), Hold::Keep)
        .unwrap();
    sleep(Duration::from_millis(40));

    let src = ctrl.stop("src").unwrap();
    assert_eq!(tuner_calls(&src), 2, "set to 2, and the refused -1");
    assert!(ctrl.controls("src").is_empty(), "stopped");
    assert_eq!(frequencies(&ctrl.stop("rx").unwrap()), [2.0]);
}

#[test]
fn a_selected_link_retunes_first_and_a_parked_one_asks_for_nothing() {
    let mut ctrl = controller();
    ctrl.link("src.samples", "a.samples").unwrap();
    ctrl.link("src.samples", "b.samples").unwrap();
    ctrl.park("b.samples", Hold::Keep).unwrap();
    ctrl.spawn("a", receiver("frequency = 1")).unwrap();
    ctrl.spawn("b", receiver("frequency = 5")).unwrap();
    ctrl.spawn("src", tuned_source(0.0)).unwrap();
    assert_eq!(ctrl.controls("src"), controls(1.0), "b is parked");
    for (rx, f) in [("b", 5.0), ("a", 1.0), ("b", 5.0)] {
        sleep(Duration::from_millis(40));
        ctrl.select(&format!("{rx}.samples"), Hold::Discard)
            .unwrap();
        assert_eq!(ctrl.controls("src"), controls(f));
    }
    sleep(Duration::from_millis(40));
    ctrl.stop("src").unwrap();
    assert_eq!(frequencies(&ctrl.stop("a").unwrap()), [1.0]);
    assert_eq!(frequencies(&ctrl.stop("b").unwrap()), [5.0]);

    // A refused value selects nothing.
    ctrl.link("src.samples", "c.samples").unwrap();
    ctrl.park("c.samples", Hold::Keep).unwrap();
    ctrl.spawn("src", tuned_source(0.0)).unwrap();
    assert_eq!(ctrl.controls("src"), controls(5.0), "as asked last");
    ctrl.spawn("c", receiver("frequency = -3")).unwrap();
    let err = ctrl.select("c.samples", Hold::Keep).unwrap_err();
    assert!(format!("{err:#}").contains("not a valid value"), "{err:#}");
    assert!(ctrl.link_stats("c.samples").unwrap().parked);
    assert!(!ctrl.link_stats("b.samples").unwrap().parked);
    ctrl.stop("src").unwrap();
}

#[test]
fn demands_go_up_through_flowgraphs_without_the_control() {
    let mut ctrl = controller();
    ctrl.link("src.samples", "mid.samples").unwrap();
    ctrl.link("mid.out", "rx.samples").unwrap();
    let mid = desc(
        r#"
        [blocks.copy]
        type = "Copy<f32>"
        [inputs]
        samples = "copy.input"
        [outputs]
        out = "copy.output"
        [radio]
        gain = 3
        "#,
    );
    ctrl.spawn("src", tuned_source(0.0)).unwrap();
    ctrl.spawn("mid", mid).unwrap();
    ctrl.spawn("rx", receiver("frequency = 7")).unwrap();
    assert_eq!(ctrl.controls("src"), controls(7.0));
    sleep(Duration::from_millis(40));
    // A source started again gets what was asked of it.
    ctrl.stop("src").unwrap();
    ctrl.spawn("src", tuned_source(0.0)).unwrap();
    assert_eq!(ctrl.controls("src"), controls(7.0));
    sleep(Duration::from_millis(40));
    ctrl.stop("src").unwrap();
    ctrl.stop("mid").unwrap();
    // What the copy block held when the source was set may come first.
    let f = frequencies(&ctrl.stop("rx").unwrap());
    assert!(f == [7.0] || f == [0.0, 7.0], "{f:?}");
}

#[test]
fn links_are_selected_and_committed_from_a_task_of_the_runtime() {
    let mut ctrl = controller();
    ctrl.link("src.samples", "a.samples").unwrap();
    ctrl.link("src.samples", "b.samples").unwrap();
    ctrl.park("b.samples", Hold::Keep).unwrap();
    let ctrl = ctrl.run(|mut ctrl| async move {
        ctrl.spawn_async("a", receiver("frequency = 1")).await?;
        ctrl.spawn_async("b", receiver("frequency = 2")).await?;
        ctrl.spawn_async("src", tuned_source(0.0)).await?;
        Timer::after(Duration::from_millis(30)).await;
        ctrl.select_async("b.samples", Hold::Discard).await?;
        Timer::after(Duration::from_millis(30)).await;
        let standby = ctrl.prepare_async("a", receiver("frequency = 3")).await?;
        ctrl.commit_async(standby, Hold::Keep).await?;
        anyhow::Ok(ctrl)
    });
    let mut ctrl = ctrl.unwrap();
    assert_eq!(ctrl.controls("src"), controls(2.0), "a is parked now");
    ctrl.stop("src").unwrap();
    assert_eq!(frequencies(&ctrl.stop("b").unwrap()), [2.0]);
    assert!(items(&ctrl.stop("a").unwrap()).is_empty());
}
