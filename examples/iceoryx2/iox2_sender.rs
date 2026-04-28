use anyhow::Result;
use futuresdr::blocks::Head;
use futuresdr::blocks::NullSource;
use futuresdr::blocks::Throttle;
use futuresdr::blocks::iceoryx2::PubSinkBuilder;
use futuresdr::prelude::*;

fn main() -> Result<()> {
    let mut fg = Flowgraph::new();

    let src = NullSource::<u8>::new();
    let head = Head::<u8>::new(1_000_000);
    let throttle = Throttle::<u8>::new(100e3);
    let snk = PubSinkBuilder::<u8>::new()
        .service_name("futuresdr/iox2-example")
        .build();

    connect!(fg, src > head > throttle > snk);

    Runtime::new().run(fg)?;
    Ok(())
}
