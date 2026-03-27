// Benchmark: Hot-swap N FFT blocks → N IFFT blocks using dynamic plugins (v2).
//
// Phase 1: VectorSource → N×FFT → NullSink  (run to completion)
// Terminate flowgraph
// Phase 2: VectorSource → N×IFFT → NullSink (run to completion)
//
// Measures the full swap cycle: terminate + reimport + rebuild + reconnect + rerun.
//
// CSV: name,n_blocks,fft_size,n_samples,phase1_run_s,terminate_s,import_s,add_fg_s,connect_s,rt_create_s,fg_init_s,fg_exec_s

use anyhow::Result;
use futuresdr::async_io::block_on;
use clap::Parser;
use futuresdr::blocks::{NullSink, VectorSource};
use futuresdr::num_complex::Complex32;
use futuresdr::runtime::{Flowgraph, Runtime};
use plugin_api::LoadedPlugin;
use std::fs::OpenOptions;
use std::io::Write;
use std::time::Instant;

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

    /// Path to plugin directory
    #[arg(long, default_value = ".")]
    plugin_dir: String,

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

    let plugin_path = format!("{}/libfft_complex_plugin.so", args.plugin_dir);

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

        // Import N FFT plugins
        let mut plugins = Vec::with_capacity(args.n_blocks);
        for _ in 0..args.n_blocks {
            plugins.push(unsafe { LoadedPlugin::load(&plugin_path) });
        }

        let mut block_ids = Vec::with_capacity(args.n_blocks);
        for plugin in &plugins {
            let id = fg.add_block_dyn(plugin.prepare(Box::new((args.fft_size, false))));
            block_ids.push(id);
        }

        fg.connect_dyn(src_id, "output", block_ids[0], "input")?;
        for w in block_ids.windows(2) {
            fg.connect_dyn(w[0], "output", w[1], "input")?;
        }
        fg.connect_dyn(*block_ids.last().unwrap(), "output", snk_id, "input")?;

        // Phase 1: run to completion (VectorSource is finite)
        let t_phase1 = Instant::now();
        rt.run(fg)?;
        let phase1_run = t_phase1.elapsed();

        // ── Terminate: measure cleanup cost ─────────────────────────
        let t_term = Instant::now();
        // Don't drop plugins (dlclose would invalidate code the runtime references).
        // Forget them — they stay mapped in memory like the dyn_phy-swap example.
        std::mem::forget(plugins);
        let terminate_time = t_term.elapsed();

        // ── Phase 2: Swap to N IFFT blocks ─────────────────────────

        // Stage: Import
        let t_import = Instant::now();
        let mut plugins2 = Vec::with_capacity(args.n_blocks);
        for _ in 0..args.n_blocks {
            plugins2.push(unsafe { LoadedPlugin::load(&plugin_path) });
        }
        let import_time = t_import.elapsed();

        // Stage: Build flowgraph
        let t_add = Instant::now();
        let mut fg2 = Flowgraph::new();
        let src2 = fg2.add_block(VectorSource::<Complex32>::new(qam_data));
        let snk2 = fg2.add_block(NullSink::<Complex32>::new());
        let src2_id = src2.get()?.id;
        let snk2_id = snk2.get()?.id;

        let mut block_ids2 = Vec::with_capacity(args.n_blocks);
        for plugin in &plugins2 {
            // is_inverse = true → IFFT
            let id = fg2.add_block_dyn(plugin.prepare(Box::new((args.fft_size, true))));
            block_ids2.push(id);
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

        // Forget phase 2 plugins too (keep .so mapped)
        std::mem::forget(plugins2);

        writeln!(
            file,
            "swap_dynv2,{},{},{},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6}",
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
