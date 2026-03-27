// Benchmark: Hot-swap N FFT blocks → N IFFT blocks using static blocks.
//
// Phase 1: VectorSource → N×FFT → NullSink  (run to completion)
// Terminate flowgraph
// Phase 2: VectorSource → N×IFFT → NullSink (run to completion)
//
// CSV: name,n_blocks,fft_size,n_samples,phase1_run_s,terminate_s,import_s,add_fg_s,connect_s,rt_create_s,fg_init_s,fg_exec_s

use anyhow::Result;
use futuresdr::async_io::block_on;
use clap::Parser;
use futuresdr::blocks::{Fft, FftDirection, NullSink, VectorSource};
use futuresdr::num_complex::Complex32;
use futuresdr::prelude::{DefaultCpuReader, DefaultCpuWriter};
use futuresdr::runtime::{Flowgraph, Runtime};
use std::fs::OpenOptions;
use std::io::Write;
use std::time::Instant;

type FftC32 = Fft<DefaultCpuReader<Complex32>, DefaultCpuWriter<Complex32>>;

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
    #[arg(long, default_value = "Hello, World! This is a swap benchmark.")]
    text: String,

    /// FFT size (must be power of 2)
    #[arg(long, default_value_t = 64)]
    fft_size: usize,

    /// Number of blocks (each is one FFT or IFFT)
    #[arg(long, default_value_t = 4)]
    n_blocks: usize,

    /// Total number of Complex32 samples to process per phase
    #[arg(long, default_value_t = 64000)]
    n_samples: usize,

    /// Number of benchmark iterations
    #[arg(long, default_value_t = 1)]
    loop_count: u32,

    /// Output CSV file
    #[arg(long, default_value = "result_swap_bench.txt")]
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

        let rt = Runtime::new();
        let qam_data = encode_qam64(&args.text, n_samples);

        // ── Phase 1: Run N FFT blocks ──────────────────────────────
        let mut fg = Flowgraph::new();
        let src = fg.add_block(VectorSource::<Complex32>::new(qam_data.clone()));
        let snk = fg.add_block(NullSink::<Complex32>::new());
        let src_id = src.get()?.id;
        let snk_id = snk.get()?.id;

        let mut block_ids = Vec::with_capacity(args.n_blocks);
        for _ in 0..args.n_blocks {
            let b = fg.add_block(FftC32::new(args.fft_size));
            block_ids.push(b.get().unwrap().id);
        }

        fg.connect_dyn(src_id, "output", block_ids[0], "input")?;
        for w in block_ids.windows(2) {
            fg.connect_dyn(w[0], "output", w[1], "input")?;
        }
        fg.connect_dyn(*block_ids.last().unwrap(), "output", snk_id, "input")?;

        let t_phase1 = Instant::now();
        rt.run(fg)?;
        let phase1_run = t_phase1.elapsed();

        // ── Terminate ──────────────────────────────────────────────
        let t_term = Instant::now();
        drop(block_ids);
        let terminate_time = t_term.elapsed();

        // ── Phase 2: Swap to N IFFT blocks ─────────────────────────

        // Stage: Import (no-op for static)
        let t_import = Instant::now();
        let import_time = t_import.elapsed();

        // Stage: Build flowgraph
        let t_add = Instant::now();
        let mut fg2 = Flowgraph::new();
        let src2 = fg2.add_block(VectorSource::<Complex32>::new(qam_data));
        let snk2 = fg2.add_block(NullSink::<Complex32>::new());
        let src2_id = src2.get()?.id;
        let snk2_id = snk2.get()?.id;

        let mut block_ids2 = Vec::with_capacity(args.n_blocks);
        for _ in 0..args.n_blocks {
            let b = fg2.add_block(FftC32::with_direction(args.fft_size, FftDirection::Inverse));
            block_ids2.push(b.get().unwrap().id);
        }
        let add_fg_time = t_add.elapsed();

        // Stage: Connect
        let t_conn = Instant::now();
        fg2.connect_dyn(src2_id, "output", block_ids2[0], "input")?;
        for w in block_ids2.windows(2) {
            fg2.connect_dyn(w[0], "output", w[1], "input")?;
        }
        fg2.connect_dyn(*block_ids2.last().unwrap(), "output", snk2_id, "input")?;
        let connect_time = t_conn.elapsed();

        // Stage: Create new Runtime
        let t_rt = Instant::now();
        let rt2 = Runtime::new();
        let rt_create_time = t_rt.elapsed();

        // Stage: Flowgraph init (scheduler setup + block Initialize + Notify)
        let t_init = Instant::now();
        let (task, _handle) = rt2.start_sync(fg2)?;
        let fg_init_time = t_init.elapsed();

        // Stage: Pure execution (data processing until all blocks done)
        let t_exec = Instant::now();
        let _fg2 = block_on(task)?;
        let fg_exec_time = t_exec.elapsed();

        writeln!(
            file,
            "swap_static,{},{},{},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6}",
            args.n_blocks,
            args.fft_size,
            n_samples,
            phase1_run.as_secs_f64(),
            terminate_time.as_secs_f64(),
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
