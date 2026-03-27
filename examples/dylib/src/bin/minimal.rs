use anyhow::Result;
use futuresdr::blocks::{NullSource, FileSink};
use futuresdr::macros::connect;
use futuresdr::runtime::{Flowgraph, Runtime};
use plugin_api::LoadedPlugin;

fn main() -> Result<()> {
    let mut fg = Flowgraph::new();

    let head_plugin = unsafe { LoadedPlugin::load("./libhead_plugin.so") };

    let src  = fg.add_block(NullSource::<u8>::new());
    let head = fg.add_block_dyn(head_plugin.prepare(Box::new(12u64)));
    let snk  = fg.add_block(FileSink::<u8>::new("output.bin"));

    fg.connect_dyn(src,  "output", head, "input")?;
    fg.connect_dyn(head, "output", snk,  "input")?;

    Runtime::new().run(fg)?;
    println!("done");
    Ok(())
}