// Benchmark: chain of 1000 Increment blocks (dynamic plugins v2),
// with static NullSource, Head, and FileSink.
//
// Chain: NullSource(static) -> Increment x1000(dyn) -> Head(static) -> FileSink(static)
//
// Outputs CSV: name,head_size,import_s,add_fg_s,connect_s,runtime_s

use anyhow::Result;
use clap::Parser;
use futuresdr::blocks::{FileSink, Head, NullSource};
use futuresdr::macros::connect;
use futuresdr::runtime::{Flowgraph, Runtime};
use plugin_api::LoadedPlugin;
use std::fs::OpenOptions;
use std::io::Write;
use std::time::Instant;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value_t = 10000)]
    head_size: u64,

    #[arg(long, default_value = ".")]
    plugin_dir: String,

    #[arg(long, default_value_t = 1)]
    loop_count: u32,
}

fn main() -> Result<()> {
    futuresdr::runtime::init();

    let args = Args::parse();

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("result_bench1203.txt")?;

    for i in 0..args.loop_count {
        eprintln!("==> Run {}/{}", i + 1, args.loop_count);

        // Stage 1: Import blocks (load plugin + prepare instances)
        let start = Instant::now();

        let increment = unsafe {
            LoadedPlugin::load(&format!(
                "{}/libincrement_plugin.so",
                args.plugin_dir
            ))
        };

        let import_time = start.elapsed();

        // Stage 2: Add blocks to flowgraph
        let t2 = Instant::now();

        let mut fg = Flowgraph::new();

        let src = fg.add_block(NullSource::<u8>::new());
        let hd = fg.add_block(Head::<u8>::new(args.head_size));
        let snk = fg.add_block(FileSink::<u8>::new("benchmark1203.txt"));

        let mut inc_ids = Vec::with_capacity(1000);
        for _ in 0..1000 {
            inc_ids.push(fg.add_block_dyn(increment.prepare(Box::new(()))));
        }

        let add_fg_time = t2.elapsed();

        // Stage 3: Connect blocks
        let t3 = Instant::now();

        let src_id = src.get()?.id;
        fg.connect_dyn(src_id, "output", inc_ids[0], "input")?;
        for j in 0..999 {
            fg.connect_dyn(inc_ids[j], "output", inc_ids[j + 1], "input")?;
        }
        let hd_id = hd.get()?.id;
        fg.connect_dyn(inc_ids[999], "output", hd_id, "input")?;
        connect!(fg, hd > snk);

        let connect_time = t3.elapsed();

        // Stage 4: Runtime execution
        let t4 = Instant::now();

        Runtime::new().run(fg)?;

        let runtime_time = t4.elapsed();

        writeln!(
            file,
            "dynv2_1000,{},{:.6},{:.6},{:.6},{:.6}",
            args.head_size,
            import_time.as_secs_f64(),
            add_fg_time.as_secs_f64(),
            connect_time.as_secs_f64(),
            runtime_time.as_secs_f64()
        )?;
    }

    Ok(())
}
