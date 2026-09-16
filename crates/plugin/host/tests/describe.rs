//! Flowgraphs from TOML descriptions.

mod common;

use futuresdr::blocks::MessageSink;
use futuresdr::blocks::VectorSink;
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;
use futuresdr::runtime::dev::CpuSample;
use plugin_host::Description;
use plugin_host::ItemType;
use plugin_host::build;

/// What `Counter<T>` emits for index `i`.
trait Index: CpuSample + PartialEq {
    fn at(i: u64) -> Self;
}
macro_rules! index {
    ($($t:ty),*) => {$(
        impl Index for $t {
            fn at(i: u64) -> Self { i as $t }
        }
    )*};
}
index!(u8, i16, i32, f32, f64);
impl Index for Complex32 {
    fn at(i: u64) -> Self {
        Complex32::new(i as f32, 0.0)
    }
}

fn run_chain<T: Index>(item: ItemType) {
    let registry = common::registry();
    let desc = Description::from_toml(&format!(
        r#"
        connections = """
        src > copy > head   # a chain
        head > snk
        """

        [blocks.src]
        type = "Counter<{item}>"
        n = 100
        chunk = 7

        [blocks.copy]
        type = "Copy<{item}>"

        [blocks.head]
        type = "Head<{item}>"
        n_items = 40

        [blocks.snk]
        type = "VectorSink<{item}>"
        "#
    ))
    .unwrap();
    let built = build(&registry, &desc).unwrap();
    let snk = built.blocks.block_ref::<VectorSink<T>>("snk").unwrap();
    let done = Runtime::new().run(built.flowgraph).unwrap();
    let items = done.block(&snk).unwrap().items().clone();
    let expected: Vec<T> = (0..40).map(T::at).collect();
    assert_eq!(items, expected, "{item}");
}

#[test]
fn chain_for_every_item_type() {
    run_chain::<u8>(ItemType::U8);
    run_chain::<i16>(ItemType::I16);
    run_chain::<i32>(ItemType::I32);
    run_chain::<f32>(ItemType::F32);
    run_chain::<f64>(ItemType::F64);
    run_chain::<Complex32>(ItemType::Complex32);
}

#[test]
fn settings_reach_the_blocks() {
    let registry = common::registry();
    let desc = Description::from_toml(
        r#"
        connections = "src > scale > snk"
        [blocks.src]
        type = "VectorSource<Complex32>"
        items = [1, [0.5, -1.0], 2.5]
        [blocks.scale]
        type = "Scale<Complex32>"
        factor = [0.0, 2.0]
        [blocks.snk]
        type = "VectorSink<Complex32>"
        capacity = 3
        "#,
    )
    .unwrap();
    let built = build(&registry, &desc).unwrap();
    let snk = built
        .blocks
        .block_ref::<VectorSink<Complex32>>("snk")
        .unwrap();
    let done = Runtime::new().run(built.flowgraph).unwrap();
    let j = Complex32::new(0.0, 2.0);
    assert_eq!(
        done.block(&snk).unwrap().items(),
        &vec![
            Complex32::new(1.0, 0.0) * j,
            Complex32::new(0.5, -1.0) * j,
            Complex32::new(2.5, 0.0) * j
        ]
    );
}

#[test]
fn message_connections() {
    let registry = common::registry();
    let desc = Description::from_toml(
        r#"
        connections = "tick | fwd | sink"
        [blocks.tick]
        type = "MessageSource"
        message = "ping"
        interval_ms = 1
        count = 5
        [blocks.fwd]
        type = "MessageCopy"
        [blocks.sink]
        type = "MessageSink"
        "#,
    )
    .unwrap();
    let built = build(&registry, &desc).unwrap();
    let sink = built.blocks.block_ref::<MessageSink>("sink").unwrap();
    let done = Runtime::new().run(built.flowgraph).unwrap();
    assert_eq!(done.block(&sink).unwrap().received(), 5);
}

#[test]
fn plugins_are_found_next_to_the_description() {
    let dir = common::scratch("plugins-path");
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::copy(
        common::basic_plugin(),
        dir.join("lib/libfsdr_blocks_basic.so"),
    )
    .unwrap();
    std::fs::write(
        dir.join("fg.toml"),
        r#"
        plugins = ["lib/libfsdr_blocks_basic.so"]
        connections = "src > snk"
        [blocks.src]
        type = "VectorSource<i16>"
        items = [3, 2, 1]
        [blocks.snk]
        type = "VectorSink<i16>"
        "#,
    )
    .unwrap();
    let desc = Description::from_file(dir.join("fg.toml")).unwrap();
    let mut registry = plugin_host::Registry::new();
    registry.load_all(&desc.plugins).unwrap();
    let built = build(&registry, &desc).unwrap();
    let snk = built.blocks.block_ref::<VectorSink<i16>>("snk").unwrap();
    let done = Runtime::new().run(built.flowgraph).unwrap();
    assert_eq!(done.block(&snk).unwrap().items(), &vec![3, 2, 1]);
}

#[test]
fn errors_point_at_the_description() {
    let registry = common::registry();
    let cases = [
        (
            "[blocks.x]\ntype = 'Head<u16>'",
            "unknown block type 'Head<u16>'",
        ),
        (
            "[blocks.x]\ntype = 'Head<u8>'\nn_items = 'ten'",
            "setting 'n_items'",
        ),
        (
            "connections = '''\n\nsrc > nope.snk\n'''\n[blocks.src]\ntype = 'NullSource<u8>'\n[blocks.snk]\ntype = 'NullSink<u8>'",
            "connections line 2",
        ),
    ];
    for (text, expected) in cases {
        let desc = Description::from_toml(text).unwrap();
        let err = match build(&registry, &desc) {
            Ok(_) => panic!("{text:?} built"),
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains(expected), "{text:?}: {err}");
        assert!(err.contains("'x'") || !text.contains("blocks.x"), "{err}");
    }
}
