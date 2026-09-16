//! Linked flowgraphs, and replacing them while the stream goes on.

mod common;

use std::thread::sleep;
use std::time::Duration;

use futuresdr::blocks::VectorSink;
use plugin_host::Controller;
use plugin_host::Description;
use plugin_host::Finished;
use plugin_host::Hold;

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
