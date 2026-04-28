use anyhow::Result;
use futuresdr::blocks::FileSink;
use futuresdr::blocks::iceoryx2::SubSourceBuilder;
use futuresdr::prelude::*;

fn main() -> Result<()> {
    let mut fg = Flowgraph::new();

    let iox2_src = SubSourceBuilder::<u8>::new()
        .service_name("futuresdr/iox2-example")
        .build();
    let snk = FileSink::<u8>::new("iox2-log.bin");

    connect!(fg, iox2_src > snk);

    Runtime::new().run(fg)?;

    Ok(())
}
