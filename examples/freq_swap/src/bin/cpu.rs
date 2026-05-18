use anyhow::Result;
use futuresdr::blocks::Apply;
use futuresdr::blocks::Fft;
use futuresdr::blocks::FftDirection;
use futuresdr::blocks::FileSink;
use futuresdr::blocks::MovingAvg;
use futuresdr::blocks::Split;
use futuresdr::blocks::WebsocketSinkBuilder;
use futuresdr::blocks::WebsocketSinkMode;
use futuresdr::blocks::seify::Builder;
use futuresdr::prelude::*;

const FFT_SIZE: usize = 2048;

fn main() -> Result<()> {
    let mut fg = Flowgraph::new();

    let src = fg.add_block(
        Builder::new("")?
            .frequency(100e6)
            .sample_rate(3.2e6)
            .gain(34.0)
            .build_source()?,
    );
    let fft: BlockRef<Fft> = fg.add_block(Fft::with_options(
        FFT_SIZE,
        FftDirection::Forward,
        true,
        None,
    ));
    let mag_sqr = fg.add_block(Apply::<_, _, _>::new(|x: &Complex32| x.norm_sqr()));
    let keep = fg.add_block(MovingAvg::<FFT_SIZE>::new(0.1, 3));
    let split = fg.add_block(Split::<_, _, _, _>::new(|v: &f32| (*v, *v)));
    let snk = fg.add_block(
        WebsocketSinkBuilder::<f32>::new(9001)
            .mode(WebsocketSinkMode::FixedBlocking(FFT_SIZE))
            .build(),
    );
    let save = fg.add_block(FileSink::<f32>::new("output.bin"));

    fg.connect_stream(&mut src.get()?.outputs()[0], &mut fft.get()?.input());
    fg.connect_stream(fft.get()?.output(), &mut mag_sqr.get()?.input());
    fg.connect_stream(mag_sqr.get()?.output(), &mut keep.get()?.input());
    fg.connect_stream(keep.get()?.output(), &mut split.get()?.input());
    fg.connect_stream(split.get()?.output0(), &mut snk.get()?.input());
    fg.connect_stream(split.get()?.output1(), &mut save.get()?.input());

    Runtime::new().run(fg)?;
    Ok(())
}
