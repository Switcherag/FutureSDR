//! Basic FutureSDR blocks as a plugin, and two blocks of its own.
//!
//! Generic blocks are exported for `u8, i16, i32, f32, f64, Complex32`
//! (`types: default`), as `Head<f32>`, `Copy<Complex32>`, ...
//!
//! FutureSDR's own blocks take their stream ports as type parameters, which
//! default to its buffer; they are named here, since a flowgraph the
//! controller builds uses the buffer whose ring is kept (`ReuseCpu*`, what
//! the plugin prelude's `DefaultCpu*` are).

extern crate futuresdr_plugin_rt as futuresdr;

use std::time::Duration;

use futuresdr::prelude::*;

/// Items converted from a running index.
pub trait FromIndex: CpuSample + std::marker::Copy {
    /// Item for index `i`.
    fn from_index(i: u64) -> Self;
}

macro_rules! from_index {
    ($($t:ty),*) => {$(
        impl FromIndex for $t {
            fn from_index(i: u64) -> Self {
                i as $t
            }
        }
    )*};
}
from_index!(u8, i16, i32, f32, f64);

impl FromIndex for Complex32 {
    fn from_index(i: u64) -> Self {
        Complex32::new(i as f32, 0.0)
    }
}

/// Emit `start, start + 1, ...` (wrapping for small integer types), `n`
/// items in total, at most `chunk` per call.
#[derive(Block)]
pub struct Counter<T: FromIndex> {
    #[output]
    output: DefaultCpuWriter<T>,
    next: u64,
    end: u64,
    chunk: usize,
}

impl<T: FromIndex> Counter<T> {
    /// Count `n` items from `start`.
    pub fn new(start: u64, n: u64, chunk: usize) -> Self {
        Self {
            output: DefaultCpuWriter::default(),
            next: start,
            end: start + n,
            chunk: chunk.max(1),
        }
    }
}

impl<T: FromIndex> Kernel for Counter<T> {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let out = self.output.slice();
        let m = out
            .len()
            .min(self.chunk)
            .min((self.end - self.next) as usize);
        for (k, v) in out[..m].iter_mut().enumerate() {
            *v = T::from_index(self.next + k as u64);
        }
        self.output.produce(m);
        self.next += m as u64;
        if self.next == self.end {
            io.finished = true;
        } else if m > 0 {
            io.call_again = true;
        }
        Ok(())
    }
}

/// Multiply every item by `factor`.
#[derive(Block)]
pub struct Scale<T: CpuSample> {
    #[input]
    input: DefaultCpuReader<T>,
    #[output]
    output: DefaultCpuWriter<T>,
    factor: T,
}

impl<T: CpuSample> Scale<T> {
    /// Scale by `factor`.
    pub fn new(factor: T) -> Self {
        Self {
            input: DefaultCpuReader::default(),
            output: DefaultCpuWriter::default(),
            factor,
        }
    }
}

impl<T> Kernel for Scale<T>
where
    T: CpuSample + std::ops::Mul<Output = T> + std::marker::Copy,
{
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let input = self.input.slice();
        let output = self.output.slice();
        let available = input.len();
        let n = available.min(output.len());
        for (o, i) in output[..n].iter_mut().zip(&input[..n]) {
            *o = *i * self.factor;
        }
        self.input.consume(n);
        self.output.produce(n);
        if self.input.finished() && n == available {
            io.finished = true;
        }
        Ok(())
    }
}

fn drop_policy(s: &Settings) -> anyhow::Result<blocks::SelectorDropPolicy> {
    let name: String = s.get_or("drop_policy", "same-rate".to_string())?;
    name.parse()
        .map_err(|e| anyhow::anyhow!("block '{}': drop_policy: {e}", s.block()))
}

export_plugin! {
    name: "basic",
    blocks: [
        {
            name: "NullSource",
            types: default,
            description: "Endless stream of default values.",
            add: |_s| blocks::NullSource::<T, ReuseCpuWriter<T>>::new(),
        },
        {
            name: "NullSink",
            types: default,
            description: "Consume and drop the input.",
            add: |_s| blocks::NullSink::<T, ReuseCpuReader<T>>::new(),
        },
        {
            name: "Head",
            types: default,
            description: "Forward the first `n_items` items, then finish.",
            add: |s| blocks::Head::<T, ReuseCpuReader<T>, ReuseCpuWriter<T>>::new(s.get("n_items")?),
        },
        {
            name: "Copy",
            types: default,
            description: "Forward the input unchanged.",
            add: |_s| blocks::Copy::<T, ReuseCpuReader<T>, ReuseCpuWriter<T>>::new(),
        },
        {
            name: "Delay",
            types: default,
            description: "Delay (positive `n`) or skip (negative `n`) items.",
            add: |s| blocks::Delay::<T, ReuseCpuReader<T>, ReuseCpuWriter<T>>::new(s.get("n")?),
        },
        {
            name: "Throttle",
            types: default,
            description: "Forward at most `rate` items per second.",
            add: |s| blocks::Throttle::<T, ReuseCpuReader<T>, ReuseCpuWriter<T>>::new(s.get("rate")?),
        },
        {
            name: "VectorSource",
            types: default,
            description: "Emit `items`, then finish.",
            add: |s| blocks::VectorSource::<T, ReuseCpuWriter<T>>::new(s.get("items")?),
        },
        {
            name: "VectorSink",
            types: default,
            description: "Collect the input; read it with `items()` after the run.",
            add: |s| blocks::VectorSink::<T, ReuseCpuReader<T>>::new(s.get_or("capacity", 1024)?),
        },
        {
            name: "Selector1x2",
            types: default,
            description: "Route one input to one of two outputs (`output_index`).",
            add: |s| blocks::Selector::<T, 1, 2, ReuseCpuReader<T>, ReuseCpuWriter<T>>::with_buffers(drop_policy(s)?),
        },
        {
            name: "Selector2x1",
            types: default,
            description: "Route one of two inputs (`input_index`) to the output.",
            add: |s| blocks::Selector::<T, 2, 1, ReuseCpuReader<T>, ReuseCpuWriter<T>>::with_buffers(drop_policy(s)?),
        },
        {
            name: "Counter",
            types: default,
            description: "Emit `start`, `start + 1`, ... (`n` items, at most `chunk` per call).",
            add: |s| Counter::<T>::new(s.get_or("start", 0)?, s.get("n")?, s.get_or("chunk", 4096)?),
        },
        {
            name: "Scale",
            types: [i16, i32, f32, f64, Complex32],
            description: "Multiply every item by `factor`.",
            add: |s| Scale::<T>::new(s.get("factor")?),
        },
        {
            name: "MessageSource",
            description: "Post `message` every `interval_ms`, `count` times (forever if absent).",
            add: |s| blocks::MessageSource::new(
                s.get("message")?,
                Duration::from_secs_f64(s.get::<f64>("interval_ms")? / 1000.0),
                s.get_opt("count")?,
            ),
        },
        {
            name: "MessageCopy",
            description: "Forward messages from `in` to `out`.",
            add: |_s| blocks::MessageCopy::new(),
        },
        {
            name: "MessageSink",
            description: "Count and drop messages.",
            add: |_s| blocks::MessageSink::new(),
        },
    ]
}
