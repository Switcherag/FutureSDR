// Benchmark: String → 64-QAM → N×(IFFT→FFT) chain using static blocks.
//
// All blocks are statically linked (no plugins).
//
// Outputs CSV: name,fft_pairs,fft_size,n_samples,import_s,add_fg_s,connect_s,rt_create_s,fg_init_s,fg_exec_s

use anyhow::Result;
use clap::Parser;
use futuresdr::async_io::block_on;
use futuresdr::blocks::{Fft, FftDirection, NullSink, VectorSource};
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::{DefaultCpuReader, DefaultCpuWriter};
use futuresdr::runtime::{Flowgraph, Runtime};

type FftC32 = Fft<DefaultCpuReader<Complex32>, DefaultCpuWriter<Complex32>>;
use std::fs::OpenOptions;
use std::io::Write;
use std::time::Instant;

// 64-QAM constellation table (from IEEE 802.11 standard)
const LEVEL: f32 = 0.1543033499620919;
const QAM64: [Complex32; 64] = [
    Complex32::new(-7.0 * LEVEL, -7.0 * LEVEL),
    Complex32::new(7.0 * LEVEL, -7.0 * LEVEL),
    Complex32::new(-1.0 * LEVEL, -7.0 * LEVEL),
    Complex32::new(1.0 * LEVEL, -7.0 * LEVEL),
    Complex32::new(-5.0 * LEVEL, -7.0 * LEVEL),
    Complex32::new(5.0 * LEVEL, -7.0 * LEVEL),
    Complex32::new(-3.0 * LEVEL, -7.0 * LEVEL),
    Complex32::new(3.0 * LEVEL, -7.0 * LEVEL),
    Complex32::new(-7.0 * LEVEL, 7.0 * LEVEL),
    Complex32::new(7.0 * LEVEL, 7.0 * LEVEL),
    Complex32::new(-1.0 * LEVEL, 7.0 * LEVEL),
    Complex32::new(1.0 * LEVEL, 7.0 * LEVEL),
    Complex32::new(-5.0 * LEVEL, 7.0 * LEVEL),
    Complex32::new(5.0 * LEVEL, 7.0 * LEVEL),
    Complex32::new(-3.0 * LEVEL, 7.0 * LEVEL),
    Complex32::new(3.0 * LEVEL, 7.0 * LEVEL),
    Complex32::new(-7.0 * LEVEL, -1.0 * LEVEL),
    Complex32::new(7.0 * LEVEL, -1.0 * LEVEL),
    Complex32::new(-1.0 * LEVEL, -1.0 * LEVEL),
    Complex32::new(1.0 * LEVEL, -1.0 * LEVEL),
    Complex32::new(-5.0 * LEVEL, -1.0 * LEVEL),
    Complex32::new(5.0 * LEVEL, -1.0 * LEVEL),
    Complex32::new(-3.0 * LEVEL, -1.0 * LEVEL),
    Complex32::new(3.0 * LEVEL, -1.0 * LEVEL),
    Complex32::new(-7.0 * LEVEL, 1.0 * LEVEL),
    Complex32::new(7.0 * LEVEL, 1.0 * LEVEL),
    Complex32::new(-1.0 * LEVEL, 1.0 * LEVEL),
    Complex32::new(1.0 * LEVEL, 1.0 * LEVEL),
    Complex32::new(-5.0 * LEVEL, 1.0 * LEVEL),
    Complex32::new(5.0 * LEVEL, 1.0 * LEVEL),
    Complex32::new(-3.0 * LEVEL, 1.0 * LEVEL),
    Complex32::new(3.0 * LEVEL, 1.0 * LEVEL),
    Complex32::new(-7.0 * LEVEL, -5.0 * LEVEL),
    Complex32::new(7.0 * LEVEL, -5.0 * LEVEL),
    Complex32::new(-1.0 * LEVEL, -5.0 * LEVEL),
    Complex32::new(1.0 * LEVEL, -5.0 * LEVEL),
    Complex32::new(-5.0 * LEVEL, -5.0 * LEVEL),
    Complex32::new(5.0 * LEVEL, -5.0 * LEVEL),
    Complex32::new(-3.0 * LEVEL, -5.0 * LEVEL),
    Complex32::new(3.0 * LEVEL, -5.0 * LEVEL),
    Complex32::new(-7.0 * LEVEL, 5.0 * LEVEL),
    Complex32::new(7.0 * LEVEL, 5.0 * LEVEL),
    Complex32::new(-1.0 * LEVEL, 5.0 * LEVEL),
    Complex32::new(1.0 * LEVEL, 5.0 * LEVEL),
    Complex32::new(-5.0 * LEVEL, 5.0 * LEVEL),
    Complex32::new(5.0 * LEVEL, 5.0 * LEVEL),
    Complex32::new(-3.0 * LEVEL, 5.0 * LEVEL),
    Complex32::new(3.0 * LEVEL, 5.0 * LEVEL),
    Complex32::new(-7.0 * LEVEL, -3.0 * LEVEL),
    Complex32::new(7.0 * LEVEL, -3.0 * LEVEL),
    Complex32::new(-1.0 * LEVEL, -3.0 * LEVEL),
    Complex32::new(1.0 * LEVEL, -3.0 * LEVEL),
    Complex32::new(-5.0 * LEVEL, -3.0 * LEVEL),
    Complex32::new(5.0 * LEVEL, -3.0 * LEVEL),
    Complex32::new(-3.0 * LEVEL, -3.0 * LEVEL),
    Complex32::new(3.0 * LEVEL, -3.0 * LEVEL),
    Complex32::new(-7.0 * LEVEL, 3.0 * LEVEL),
    Complex32::new(7.0 * LEVEL, 3.0 * LEVEL),
    Complex32::new(-1.0 * LEVEL, 3.0 * LEVEL),
    Complex32::new(1.0 * LEVEL, 3.0 * LEVEL),
    Complex32::new(-5.0 * LEVEL, 3.0 * LEVEL),
    Complex32::new(5.0 * LEVEL, 3.0 * LEVEL),
    Complex32::new(-3.0 * LEVEL, 3.0 * LEVEL),
    Complex32::new(3.0 * LEVEL, 3.0 * LEVEL),
];

fn qam64_map(byte: u8) -> Complex32 {
    QAM64[(byte & 0x3F) as usize]
}

fn encode_qam64(text: &str, n_samples: usize) -> Vec<Complex32> {
    let symbols: Vec<Complex32> = text.as_bytes().iter().map(|&b| qam64_map(b)).collect();
    if symbols.is_empty() {
        return vec![QAM64[0]; n_samples];
    }
    symbols.into_iter().cycle().take(n_samples).collect()
}

#[derive(Parser)]
struct Args {
    /// Input text to encode
    #[arg(long, default_value = "Hello, World! This is a 64-QAM FFT benchmark.")]
    text: String,

    /// FFT size (must be power of 2)
    #[arg(long, default_value_t = 64)]
    fft_size: usize,

    /// Number of IFFT→FFT pairs in the chain
    #[arg(long, default_value_t = 4)]
    fft_pairs: usize,

    /// Total number of Complex32 samples to process
    #[arg(long, default_value_t = 64000)]
    n_samples: usize,

    /// Number of benchmark iterations
    #[arg(long, default_value_t = 1)]
    loop_count: u32,

    /// Output CSV file
    #[arg(long, default_value = "result_fft_bench.txt")]
    output: String,
}

fn main() -> Result<()> {
    futuresdr::runtime::init();

    let args = Args::parse();
    let n_samples = (args.n_samples / args.fft_size) * args.fft_size;

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&args.output)?;

    for i in 0..args.loop_count {
        eprintln!("==> Run {}/{}", i + 1, args.loop_count);

        // Stage 1: Import blocks (no-op for static, but time it for fairness)
        let start = Instant::now();
        // Nothing to import — blocks are statically linked
        let import_time = start.elapsed();

        // Stage 2: Build flowgraph — QAM64 encode + add all blocks
        let t2 = Instant::now();

        let qam_data = encode_qam64(&args.text, n_samples);
        let mut fg = Flowgraph::new();

        let src = fg.add_block(VectorSource::<Complex32>::new(qam_data));
        let snk = fg.add_block(NullSink::<Complex32>::new());

        // Add N IFFT + N FFT blocks statically, extract IDs for connect_dyn
        let mut fft_ids = Vec::with_capacity(args.fft_pairs * 2);
        for _ in 0..args.fft_pairs {
            let ifft = fg.add_block(
                FftC32::with_direction(args.fft_size, FftDirection::Inverse),
            );
            let fft = fg.add_block(FftC32::new(args.fft_size));
            fft_ids.push(ifft.get().unwrap().id);
            fft_ids.push(fft.get().unwrap().id);
        }

        let add_fg_time = t2.elapsed();

        // Stage 3: Connect the chain using connect_dyn (same as dynv2 for fair comparison)
        let t3 = Instant::now();

        let src_id = src.get().unwrap().id;
        let snk_id = snk.get().unwrap().id;

        // src → first IFFT
        fg.connect_dyn(src_id, "output", fft_ids[0], "input")?;
        // Chain all FFT blocks
        for w in fft_ids.windows(2) {
            fg.connect_dyn(w[0], "output", w[1], "input")?;
        }
        // Last FFT → sink
        fg.connect_dyn(*fft_ids.last().unwrap(), "output", snk_id, "input")?;

        let connect_time = t3.elapsed();

        // Stage 4a: Create Runtime
        let t4a = Instant::now();
        let rt = Runtime::new();
        let rt_create_time = t4a.elapsed();

        // Stage 4b: Flowgraph init (scheduler + block init + notify)
        let t4b = Instant::now();
        let (task, _handle) = rt.start_sync(fg)?;
        let fg_init_time = t4b.elapsed();

        // Stage 4c: Pure execution (data processing)
        let t4c = Instant::now();
        let _fg = block_on(task)?;
        let fg_exec_time = t4c.elapsed();

        writeln!(
            file,
            "fft_static,{},{},{},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6}",
            args.fft_pairs,
            args.fft_size,
            n_samples,
            import_time.as_secs_f64(),
            add_fg_time.as_secs_f64(),
            connect_time.as_secs_f64(),
            rt_create_time.as_secs_f64(),
            fg_init_time.as_secs_f64(),
            fg_exec_time.as_secs_f64(),
        )?;
    }

    Ok(())
}
