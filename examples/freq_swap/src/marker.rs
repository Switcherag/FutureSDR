use futuresdr::num_complex::Complex32;
use futuresdr::prelude::*;

/// Passthrough IQ block that injects a recognizable amplitude pulse when
/// pinged on its `mark` message port.
///
/// Used to ground-truth retune latency: fire the `mark` message on the
/// same Rust line as `handle.callback(rx, "freq", ...)` and the recorded
/// IQ will contain both
///   1. a high-amplitude pulse at the sample where the *message* was
///      serviced by this block (i.e. when control reached the IQ chain), and
///   2. the actual frequency-shift transient at the sample where the
///      hardware retune took effect.
/// The gap between (1) and (2), in samples, isolates SDR hardware latency
/// from flowgraph message-delivery latency.
///
/// The marker overwrites `mark_samples` consecutive samples with
/// `mark_value` (default `(10.0, 0.0)` — well above any signal that lives
/// in the unit-circle range so it's trivial to detect in post-processing).
#[derive(Block)]
#[message_inputs(mark)]
pub struct Marker<I = DefaultCpuReader<Complex32>, O = DefaultCpuWriter<Complex32>>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    #[input]
    input: I,
    #[output]
    output: O,
    mark_value: Complex32,
    mark_samples: usize,
    remaining: usize,
}

impl<I, O> Marker<I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    pub fn new(mark_value: Complex32, mark_samples: usize) -> Self {
        Self {
            input: I::default(),
            output: O::default(),
            mark_value,
            mark_samples,
            remaining: 0,
        }
    }

    async fn mark(
        &mut self,
        _io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
        _p: Pmt,
    ) -> Result<Pmt> {
        self.remaining = self.mark_samples;
        Ok(Pmt::Ok)
    }
}

#[doc(hidden)]
impl<I, O> Kernel for Marker<I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = Complex32>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let o = self.output.slice();
        let n = i.len().min(o.len());
        if n > 0 {
            for k in 0..n {
                if self.remaining > 0 {
                    o[k] = self.mark_value;
                    self.remaining -= 1;
                } else {
                    o[k] = i[k];
                }
            }
            self.input.consume(n);
            self.output.produce(n);
        }
        if self.input.finished() {
            io.finished = true;
        }
        Ok(())
    }
}
