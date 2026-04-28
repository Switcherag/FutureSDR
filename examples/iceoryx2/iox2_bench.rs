use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

const SVC_PING: &str = "futuresdr/bench-ping";
const SVC_PONG: &str = "futuresdr/bench-pong";
const WARMUP: usize = 200;
const ITERATIONS: usize = 2000;

/// Payload sizes in bytes
const PAYLOAD_SIZES: &[usize] = &[
    256,        // 0.25 KB
    1024,       // 1 KB
    4 * 1024,   // 4 KB
    16 * 1024,  // 16 KB
    64 * 1024,  // 64 KB
    256 * 1024, // 256 KB
    1024 * 1024,    // 1024 KB
    4 * 1024 * 1024, // 4096 KB
];

fn size_label(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{} MB", bytes / (1024 * 1024))
    }
}

struct LatencyResult {
    payload_bytes: usize,
    min_us: f64,
    median_us: f64,
    p90_us: f64,
    p99_us: f64,
    max_us: f64,
    mean_us: f64,
}

/// Spawn a "pong" reflector thread: receives on SVC_PING, sends back on SVC_PONG.
fn spawn_reflector(
    stop: Arc<AtomicBool>,
    max_payload: usize,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        use iceoryx2::prelude::*;

        let node = NodeBuilder::new()
            .create::<ipc::Service>()
            .expect("reflector: node");

        let ping_sn = ServiceName::new(SVC_PING).expect("reflector: ping name");
        let pong_sn = ServiceName::new(SVC_PONG).expect("reflector: pong name");

        let ping_svc = node
            .service_builder(&ping_sn)
            .publish_subscribe::<[u8]>()
            .open_or_create()
            .expect("reflector: ping service");
        let pong_svc = node
            .service_builder(&pong_sn)
            .publish_subscribe::<[u8]>()
            .open_or_create()
            .expect("reflector: pong service");

        let subscriber = ping_svc
            .subscriber_builder()
            .create()
            .expect("reflector: subscriber");
        let publisher = pong_svc
            .publisher_builder()
            .initial_max_slice_len(max_payload)
            .create()
            .expect("reflector: publisher");

        while !stop.load(Ordering::Relaxed) {
            match subscriber.receive() {
                Ok(Some(sample)) => {
                    let data = sample.payload();
                    let reply = publisher
                        .loan_slice_uninit(data.len())
                        .expect("reflector: loan");
                    let reply = reply.write_from_slice(data);
                    reply.send().expect("reflector: send");
                }
                Ok(None) => {
                    std::hint::spin_loop();
                }
                Err(e) => {
                    eprintln!("reflector error: {:?}", e);
                    break;
                }
            }
        }
    })
}

/// Measure round-trip latency for a given payload size.
fn measure_latency(payload_bytes: usize) -> LatencyResult {
    use iceoryx2::prelude::*;

    let stop = Arc::new(AtomicBool::new(false));
    let reflector = spawn_reflector(stop.clone(), payload_bytes);

    // Let reflector set up
    std::thread::sleep(Duration::from_millis(100));

    let node = NodeBuilder::new()
        .create::<ipc::Service>()
        .expect("sender: node");

    let ping_sn = ServiceName::new(SVC_PING).expect("sender: ping name");
    let pong_sn = ServiceName::new(SVC_PONG).expect("sender: pong name");

    let ping_svc = node
        .service_builder(&ping_sn)
        .publish_subscribe::<[u8]>()
        .open_or_create()
        .expect("sender: ping service");
    let pong_svc = node
        .service_builder(&pong_sn)
        .publish_subscribe::<[u8]>()
        .open_or_create()
        .expect("sender: pong service");

    let publisher = ping_svc
        .publisher_builder()
        .initial_max_slice_len(payload_bytes)
        .create()
        .expect("sender: publisher");
    let subscriber = pong_svc
        .subscriber_builder()
        .create()
        .expect("sender: subscriber");

    // Warm up
    for _ in 0..WARMUP {
        let sample = publisher
            .loan_slice_uninit(payload_bytes)
            .expect("warmup: loan");
        let sample = sample.write_from_fn(|_| 0u8);
        sample.send().expect("warmup: send");
        loop {
            if let Ok(Some(_)) = subscriber.receive() {
                break;
            }
            std::hint::spin_loop();
        }
    }

    // Measure
    let mut latencies = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        let sample = publisher
            .loan_slice_uninit(payload_bytes)
            .expect("bench: loan");
        let sample = sample.write_from_fn(|_| 42u8);

        let t0 = Instant::now();
        sample.send().expect("bench: send");
        loop {
            if let Ok(Some(_)) = subscriber.receive() {
                break;
            }
            std::hint::spin_loop();
        }
        let rtt = t0.elapsed();
        latencies.push(rtt.as_nanos() as f64 / 1000.0); // us
    }

    stop.store(true, Ordering::Relaxed);
    reflector.join().unwrap();

    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let mean_us = latencies.iter().sum::<f64>() / latencies.len() as f64;

    LatencyResult {
        payload_bytes,
        min_us: latencies[0],
        median_us: latencies[latencies.len() / 2],
        p90_us: latencies[(latencies.len() as f64 * 0.90) as usize],
        p99_us: latencies[(latencies.len() as f64 * 0.99) as usize],
        max_us: *latencies.last().unwrap(),
        mean_us,
    }
}

fn results_to_json(results: &[LatencyResult]) -> String {
    let entries: Vec<String> = results
        .iter()
        .map(|r| {
            format!(
                concat!(
                    "  {{\"payload_bytes\": {}, \"payload_label\": \"{}\", ",
                    "\"min_us\": {:.3}, \"median_us\": {:.3}, \"mean_us\": {:.3}, ",
                    "\"p90_us\": {:.3}, \"p99_us\": {:.3}, \"max_us\": {:.3}}}"
                ),
                r.payload_bytes,
                size_label(r.payload_bytes),
                r.min_us,
                r.median_us,
                r.mean_us,
                r.p90_us,
                r.p99_us,
                r.max_us,
            )
        })
        .collect();
    format!("[\n{}\n]", entries.join(",\n"))
}

fn main() -> Result<()> {
    println!("=== iceoryx2 Shared Memory Latency Benchmark ===");
    println!("  Ping-pong round-trip, {} warmup, {} iterations\n", WARMUP, ITERATIONS);
    println!(
        "  {:>10}  {:>10}  {:>10}  {:>10}  {:>10}  {:>10}  {:>10}",
        "Payload", "Min", "Median", "Mean", "P90", "P99", "Max"
    );
    println!("  {}", "-".repeat(76));

    let mut results = Vec::new();

    for &sz in PAYLOAD_SIZES {
        let r = measure_latency(sz);
        println!(
            "  {:>10}  {:>9.2}u  {:>9.2}u  {:>9.2}u  {:>9.2}u  {:>9.2}u  {:>9.2}u",
            size_label(sz),
            r.min_us,
            r.median_us,
            r.mean_us,
            r.p90_us,
            r.p99_us,
            r.max_us,
        );
        results.push(r);
    }

    let json = results_to_json(&results);
    fs::write("bench_results.json", &json)?;
    println!("\nResults written to bench_results.json");
    println!("=== Done ===");
    Ok(())
}
