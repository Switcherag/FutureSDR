use futuresdr::prelude::*;

#[derive(Block)]
pub struct ZigbeeDemod<
    I: CpuBufferReader<Item = Complex32> = DefaultCpuReader<Complex32>,
    O: CpuBufferWriter<Item = f32> = DefaultCpuWriter<f32>,
> {
    #[input]
    input: I,
    #[output]
    output: O,
    last_sample: Complex32,
    iir_state: f32,
    alpha: f32,
}

impl<I, O> ZigbeeDemod<I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = f32>,
{
    pub fn new(alpha: f32) -> Self {
        Self {
            input: I::default(),
            output: O::default(),
            last_sample: Complex32::new(0.0, 0.0),
            iir_state: 0.0,
            alpha,
        }
    }
}

impl<I, O> Kernel for ZigbeeDemod<I, O>
where
    I: CpuBufferReader<Item = Complex32>,
    O: CpuBufferWriter<Item = f32>,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mio: &mut MessageOutputs,
        _meta: &mut BlockMeta,
    ) -> Result<()> {
        let m = {
            let i = self.input.slice();
            let o = self.output.slice();
            let m = std::cmp::min(i.len(), o.len());

            for k in 0..m {
                // FM demodulation: arg(z * conj(last))
                let product = i[k] * self.last_sample.conj();
                let demod = product.arg();
                self.last_sample = i[k];

                // IIR DC blocker: y[n] = x[n] - x_avg, where x_avg tracks via alpha
                self.iir_state = (1.0 - self.alpha) * self.iir_state + self.alpha * demod;
                o[k] = demod - self.iir_state;
            }
            m
        };

        self.input.consume(m);
        self.output.produce(m);

        if self.input.finished() {
            io.finished = true;
        }

        Ok(())
    }
}

plugin_api::export_plugin! {
    name: "ZigbeeDemod",
    description: "FM demodulation with IIR DC blocker for Zigbee",
    config: f32,
    create: |cfg, _id| {
        ZigbeeDemod::<DefaultCpuReader<Complex32>, DefaultCpuWriter<f32>>::new(cfg)
    }
}
