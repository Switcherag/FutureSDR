//! ZigBee (IEEE 802.15.4, O-QPSK) as a plugin.
//!
//! The blocks are those of `examples/zigbee`, compiled from its sources as
//! they are, plus the receiver's demodulator, which the example writes as a
//! closure:
//!
//! ```text
//! ZigbeeDemod > ClockRecoveryMm > ZigbeeDecoder | rx.ZigbeeMac    receive
//! ZigbeeMac > ZigbeeModulator > IqDelay                            transmit
//! ```

extern crate futuresdr_plugin_rt as futuresdr;

use futuresdr::prelude::*;

#[path = "../../../../../examples/zigbee/src/clock_recovery_mm.rs"]
mod clock_recovery_mm;
#[path = "../../../../../examples/zigbee/src/decoder.rs"]
mod decoder;
#[path = "../../../../../examples/zigbee/src/iq_delay.rs"]
#[allow(dead_code)]
mod iq_delay;
#[path = "../../../../../examples/zigbee/src/mac.rs"]
mod mac;
#[path = "../../../../../examples/zigbee/src/modulator.rs"]
mod modulator;

// With their default stream buffers.
pub type ClockRecoveryMm = clock_recovery_mm::ClockRecoveryMm;
pub type Decoder = decoder::Decoder;
pub type IqDelay = iq_delay::IqDelay;
pub type Mac = mac::Mac;

/// Phase-difference demodulator with DC removal: the front end of
/// `examples/zigbee/src/bin/rx.rs`. `alpha` is the weight of the DC
/// estimator.
#[derive(Block)]
pub struct Demod {
    #[input]
    input: DefaultCpuReader<Complex32>,
    #[output]
    output: DefaultCpuWriter<f32>,
    last: Complex32,
    dc: f32,
    alpha: f32,
}

impl Demod {
    pub fn new(alpha: f32) -> Self {
        Self {
            input: DefaultCpuReader::default(),
            output: DefaultCpuWriter::default(),
            last: Complex32::new(0.0, 0.0),
            dc: 0.0,
            alpha,
        }
    }
}

impl Kernel for Demod {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let o = self.output.slice();
        let (i_len, n) = (i.len(), i.len().min(o.len()));
        for (x, y) in i[..n].iter().zip(&mut o[..n]) {
            let phase = (self.last.conj() * x).arg();
            self.last = *x;
            self.dc = (1.0 - self.alpha) * self.dc + self.alpha * phase;
            *y = phase - self.dc;
        }
        self.input.consume(n);
        self.output.produce(n);
        if self.input.finished() && n == i_len {
            io.finished = true;
        }
        Ok(())
    }
}

export_plugin! {
    name: "zigbee",
    blocks: [
        {
            name: "ZigbeeDemod",
            description: "Phase-difference demodulator with DC removal (setting alpha).",
            add: |s| Demod::new(s.get_or("alpha", 0.00016)?),
        },
        {
            name: "ClockRecoveryMm",
            description: "Mueller & Muller clock recovery on f32 samples.",
            add: |s| ClockRecoveryMm::new(
                s.get_or("omega", 2.0)?,
                s.get_or("gain_omega", 0.000225)?,
                s.get_or("mu", 0.5)?,
                s.get_or("gain_mu", 0.03)?,
                s.get_or("omega_relative_limit", 0.0002)?,
            ),
        },
        {
            name: "ZigbeeDecoder",
            description: "Chip decoder; posts frames on `out` (setting threshold).",
            add: |s| Decoder::new(s.get_or("threshold", 6)?),
        },
        {
            name: "ZigbeeMac",
            description: "MAC: frames to send on `tx`, received on `rx`, posts `rxed` and `rftap`.",
            add: |_s| Mac::new(),
        },
        {
            name: "ZigbeeModulator",
            description: "O-QPSK chips from the MAC's bytes.",
            build: |fg, _s| Ok(Added::untyped(block_on(modulator::modulator(fg))?)),
        },
        {
            name: "IqDelay",
            description: "Delays Q by half a chip, for O-QPSK.",
            add: |_s| IqDelay::new(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futuresdr::blocks::MessagePipe;
    use futuresdr::blocks::NullSink;
    use futuresdr::blocks::VectorSource;
    use futuresdr::runtime::channel::mpsc;

    use super::*;

    /// Posts `frames` on `out` once, then waits.
    #[derive(Block)]
    #[message_outputs(out)]
    struct MessageSender {
        frames: Vec<Vec<u8>>,
    }

    impl Kernel for MessageSender {
        async fn init(&mut self, mo: &mut MessageOutputs, _meta: &BlockMeta) -> Result<()> {
            for frame in self.frames.drain(..) {
                mo.post("out", Pmt::Blob(frame)).await?;
            }
            Ok(())
        }
    }

    fn recording(name: &str) -> Vec<Complex32> {
        let path = format!("{}/../testdata/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{path}: {e}"))
            .chunks_exact(8)
            .map(|c| {
                let v = |b: &[u8]| f32::from_le_bytes(b.try_into().unwrap());
                Complex32::new(v(&c[..4]), v(&c[4..]))
            })
            .collect()
    }

    /// The receiver of `examples/zigbee`, from `src` to frames in `rx`.
    fn receiver(fg: &mut Flowgraph, src: BlockId) -> Result<mpsc::Receiver<Pmt>> {
        let demod = fg.add(Demod::new(0.00016))?;
        let clock = ClockRecoveryMm::new(2.0, 0.000225, 0.5, 0.03, 0.0002);
        let clock = fg.add(clock)?;
        let decoder = Decoder::new(6);
        let decoder = fg.add(decoder)?;
        let mac = Mac::new();
        let mac = fg.add(mac)?;
        let snk = fg.add(NullSink::<u8>::new())?;
        let (tx, rx) = mpsc::channel(16);
        let pipe = fg.add(MessagePipe::new(tx))?;
        fg.stream_dyn(src, "output", demod.id(), "input")?;
        fg.stream_dyn(demod.id(), "output", clock.id(), "input")?;
        fg.stream_dyn(clock.id(), "output", decoder.id(), "input")?;
        fg.stream_dyn(mac.id(), "output", snk.id(), "input")?;
        fg.message(decoder.id(), "out", mac.id(), "rx")?;
        fg.message(mac.id(), "rxed", pipe.id(), "in")?;
        Ok(rx)
    }

    /// Run `fg` until `rx` has `n` frames or ten seconds passed, then stop
    /// it; the receiver runs forever otherwise.
    fn run_until(fg: Flowgraph, rx: &mpsc::Receiver<Pmt>, n: usize) -> Result<Vec<Vec<u8>>> {
        let running = Runtime::new().start(fg)?;
        let mut frames = Vec::new();
        let since = std::time::Instant::now();
        while frames.len() < n && since.elapsed() < Duration::from_secs(10) {
            match rx.try_recv() {
                Ok(Pmt::Blob(frame)) => frames.push(frame),
                _ => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        // Anything more that arrives is an error too.
        std::thread::sleep(Duration::from_millis(50));
        while let Ok(Pmt::Blob(frame)) = rx.try_recv() {
            frames.push(frame);
        }
        let (task, handle) = running.split();
        block_on(handle.stop())?;
        block_on(task)?;
        Ok(frames)
    }

    #[test]
    fn a_recorded_frame_is_received() -> Result<()> {
        let mut samples = vec![Complex32::new(0.0, 0.0); 4000];
        samples.extend(recording("zigbee_frame.cf32"));
        samples.extend(vec![Complex32::new(0.0, 0.0); 4000]);

        let mut fg = Flowgraph::new();
        let src: VectorSource<Complex32> = VectorSource::new(samples);
        let src = fg.add(src)?;
        let rx = receiver(&mut fg, src.id())?;
        let frames = run_until(fg, &rx, 1)?;
        assert_eq!(frames.len(), 1, "{frames:?}");
        assert!(frames[0].len() > 11, "{:?}", frames[0]);
        Ok(())
    }

    #[test]
    fn transmitted_frames_are_received() -> Result<()> {
        let mut fg = Flowgraph::new();
        let mac = Mac::new();
        let mac = fg.add(mac)?;
        let modulator = block_on(modulator::modulator(&mut fg))?;
        let delay = IqDelay::new();
        let delay = fg.add(delay)?;
        fg.stream_dyn(mac.id(), "output", modulator, "input")?;
        fg.stream_dyn(modulator, "output", delay.id(), "input")?;
        let rx = receiver(&mut fg, delay.id())?;

        let payloads: Vec<Vec<u8>> = (0..3)
            .map(|i| format!("FutureSDR plugin {i}").into_bytes())
            .collect();
        let msgs = fg.add(MessageSender {
            frames: payloads.clone(),
        })?;
        fg.message(msgs.id(), "out", mac.id(), "tx")?;
        let frames = run_until(fg, &rx, payloads.len())?;

        let got: Vec<&[u8]> = frames.iter().map(|f| &f[9..f.len() - 2]).collect();
        assert_eq!(got, payloads.iter().map(Vec::as_slice).collect::<Vec<_>>());
        Ok(())
    }
}
