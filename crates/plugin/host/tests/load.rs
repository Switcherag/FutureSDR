//! Loading a plugin built against this process, and using its blocks.

mod common;

use std::collections::HashMap;

use futuresdr::blocks::VectorSink;
use futuresdr::prelude::*;
use plugin_host::Settings;

fn settings(block: &str, pairs: &[(&str, Pmt)]) -> Settings {
    let values: HashMap<String, Pmt> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    Settings::new(block, values)
}

#[test]
fn basic_plugin_exports_its_block_types() {
    let registry = common::registry();
    for name in [
        "Head<f32>",
        "Copy<Complex32>",
        "VectorSource<u8>",
        "VectorSink<i16>",
        "Scale<i32>",
        "Selector1x2<f64>",
        "Counter<f32>",
        "MessageCopy",
        "MessageSink",
    ] {
        assert!(registry.get(name).is_some(), "{name} is missing");
    }
    assert!(registry.get("Scale<u8>").is_none());
    let origin = &registry.get("Head<f32>").unwrap().origin;
    assert_eq!(origin.plugin, "basic");
    assert_eq!(
        origin.library.as_deref(),
        Some(common::basic_plugin().canonicalize().unwrap().as_path())
    );
}

#[test]
fn loading_the_same_library_twice_does_nothing() {
    let mut registry = common::registry();
    assert!(registry.load(&common::basic_plugin()).unwrap().is_empty());
}

#[test]
fn a_copy_exporting_the_same_types_is_refused() {
    let copy = common::scratch("copy").join("libfsdr_blocks_basic_bis.so");
    std::fs::copy(common::basic_plugin(), &copy).unwrap();
    let mut registry = common::registry();
    let err = registry.load(&copy).unwrap_err();
    assert!(
        err.to_string()
            .contains("already registered by plugin 'basic'"),
        "{err:#}"
    );

    // On its own, the copy is a separate library with the same blocks.
    let mut registry = plugin_host::Registry::new();
    assert!(!registry.load(&copy).unwrap().is_empty());
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    assert!(maps.contains("libfsdr_blocks_basic_bis.so"));
}

#[test]
fn not_a_plugin_is_refused() {
    let mut registry = plugin_host::Registry::new();
    let libc = std::path::Path::new("/lib64/libm.so.6");
    let err = registry.load(libc).unwrap_err();
    assert!(
        err.to_string().contains("not a FutureSDR plugin"),
        "{err:#}"
    );
}

#[test]
fn chain_of_plugin_blocks() -> anyhow::Result<()> {
    let registry = common::registry();
    let mut fg = Flowgraph::new();
    let items: Vec<Pmt> = (0..10).map(|i| Pmt::F64(i as f64)).collect();
    let src = registry.add(
        &mut fg,
        "VectorSource<f32>",
        &settings("src", &[("items", Pmt::VecPmt(items))]),
    )?;
    let scale = registry.add(
        &mut fg,
        "Scale<f32>",
        &settings("scale", &[("factor", Pmt::F64(2.0))]),
    )?;
    let snk = registry.add(&mut fg, "VectorSink<f32>", &settings("snk", &[]))?;
    fg.stream_dyn(src.id, "output", scale.id, "input")?;
    fg.stream_dyn(scale.id, "output", snk.id, "input")?;

    let done = Runtime::new().run(fg)?;
    let snk = snk
        .block_ref
        .downcast::<BlockRef<VectorSink<f32>>>()
        .expect("the plugin's VectorSink<f32> is the host's VectorSink<f32>");
    let items = done.block(&snk)?.items().clone();
    assert_eq!(items, (0..10).map(|i| 2.0 * i as f32).collect::<Vec<_>>());
    Ok(())
}

#[test]
fn settings_errors_name_the_block() {
    let registry = common::registry();
    let mut fg = Flowgraph::new();
    let err = registry
        .add(&mut fg, "Head<u8>", &settings("first", &[]))
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("first") && msg.contains("n_items"), "{msg}");
    let err = registry
        .add(&mut fg, "Head<u16>", &settings("second", &[]))
        .unwrap_err();
    assert!(format!("{err:#}").contains("Head<u8>"), "{err:#}");
}
