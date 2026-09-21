//! Receiving recorded frames, compared with what the receivers these blocks
//! come from received from the same recordings (`../testdata/expected`).

use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use futuresdr::blocks::MessagePipe;
use futuresdr::runtime::channel::mpsc;

use crate::test_rng::Rng;
use crate::*;

/// A standard normal sample.
pub fn normal(rng: &mut Rng) -> f32 {
    let mut uniform = || ((rng.next_u64() >> 11) as f64 + 1.0) / (1u64 << 53) as f64;
    let (u, v) = (uniform(), uniform());
    ((-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()) as f32
}

fn read_cf32(path: &PathBuf) -> Vec<Complex32> {
    std::fs::read(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .chunks_exact(8)
        .map(|c| {
            let v = |b: &[u8]| f32::from_le_bytes(b.try_into().unwrap());
            Complex32::new(v(&c[..4]), v(&c[4..]))
        })
        .collect()
}

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../testdata")
        .join(name)
}

/// `examples/wlan`'s recordings.
fn wlan_data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../examples/wlan/data")
        .join(name)
}

fn expected(name: &str) -> Vec<Vec<u8>> {
    let path = testdata("expected").join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .map(|l| {
            (0..l.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&l[i..i + 2], 16).unwrap())
                .collect()
        })
        .collect()
}

/// Emits `samples`, all it can at once, or in chunks of random sizes.
#[derive(Block)]
struct ChunkSource {
    #[output]
    output: DefaultCpuWriter<Complex32>,
    samples: Vec<Complex32>,
    pos: usize,
    chunks: Option<Rng>,
}

impl Kernel for ChunkSource {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let out = self.output.slice();
        let left = &self.samples[self.pos..];
        let mut n = out.len().min(left.len());
        if let Some(rng) = self.chunks.as_mut() {
            n = n.min(rng.range(1, 400));
        }
        out[..n].copy_from_slice(&left[..n]);
        self.output.produce(n);
        self.pos += n;
        if self.pos == self.samples.len() {
            io.finished = true;
        } else if n > 0 {
            io.call_again = true;
        }
        Ok(())
    }
}

/// The MPDUs the receiver for `S` posts from `samples`, chunked by seed
/// `chunks` if given.
fn receive<S: Standard>(
    samples: Vec<Complex32>,
    chunks: Option<u64>,
    invalid_frames: bool,
) -> Result<Vec<Vec<u8>>> {
    receive_with::<S>(samples, chunks, invalid_frames, false, false)
}

/// The receiver with the fused blocks, or (`granular`) as `examples/wlan`
/// builds it, block by block.
fn receive_with<S: Standard>(
    samples: Vec<Complex32>,
    chunks: Option<u64>,
    invalid_frames: bool,
    granular: bool,
    hard: bool,
) -> Result<Vec<Vec<u8>>> {
    let mut fg = Flowgraph::new();
    let src = fg.add(ChunkSource {
        output: DefaultCpuWriter::default(),
        samples,
        pos: 0,
        chunks: chunks.map(Rng::new),
    })?;
    let long = fg.add(SyncLong::<S>::new())?;
    let dec = fg.add(Decoder::<S>::with_hard(invalid_frames, hard))?;
    let (tx, rx) = mpsc::channel(10_000);
    let pipe = fg.add(MessagePipe::new(tx))?;
    if granular {
        type Delay =
            blocks::Delay<Complex32, DefaultCpuReader<Complex32>, DefaultCpuWriter<Complex32>>;
        let delay = fg.add(Delay::new(S::STF_DELAY as isize))?;
        let mag = fg.add(MagSquared::new())?;
        let mult = fg.add(MultiplyConj::new())?;
        let power = fg.add(MovingSum::<f64>::new(S::STF_POWER_WIN))?;
        let corr = fg.add(MovingSum::<Complex64>::new(S::STF_CORR_WIN))?;
        let div = fg.add(DivideMag::new())?;
        let sync = fg.add(SyncShortGranular::<S>::new(sync_short::THRESHOLD))?;
        let fft = fg.add(granular::Fft::<S>::new())?;
        let eq = fg.add(FrameEqualizer::<S>::with_fft(false))?;
        for (a, ap, b, bp) in [
            (src.id(), "output", delay.id(), "input"),
            (src.id(), "output", mag.id(), "input"),
            (src.id(), "output", mult.id(), "in0"),
            (delay.id(), "output", mult.id(), "in1"),
            (mag.id(), "output", power.id(), "input"),
            (mult.id(), "output", corr.id(), "input"),
            (corr.id(), "output", div.id(), "in0"),
            (power.id(), "output", div.id(), "in1"),
            (delay.id(), "output", sync.id(), "in_sig"),
            (corr.id(), "output", sync.id(), "in_abs"),
            (div.id(), "output", sync.id(), "in_cor"),
            (sync.id(), "output", long.id(), "input"),
            (long.id(), "output", fft.id(), "input"),
            (fft.id(), "output", eq.id(), "input"),
            (eq.id(), "output", dec.id(), "input"),
        ] {
            fg.stream_dyn(a, ap, b, bp)?;
        }
    } else {
        let sync = fg.add(SyncShort::<S>::default())?;
        let eq = fg.add(FrameEqualizer::<S>::new())?;
        fg.stream_dyn(src.id(), "output", sync.id(), "input")?;
        fg.stream_dyn(sync.id(), "output", long.id(), "input")?;
        fg.stream_dyn(long.id(), "output", eq.id(), "input")?;
        fg.stream_dyn(eq.id(), "output", dec.id(), "input")?;
    }
    fg.message(dec.id(), "rx_frames", pipe.id(), "in")?;

    let running = Runtime::new().start(fg)?;
    let mut frames = Vec::new();
    let since = Instant::now();
    loop {
        match rx.try_recv() {
            Ok(Pmt::Blob(frame)) => frames.push(frame),
            Ok(Pmt::Finished) => break,
            Ok(p) => panic!("{p:?}"),
            Err(_) if since.elapsed() > Duration::from_secs(120) => panic!("no end of stream"),
            Err(_) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    // The pipe runs until stopped.
    let (task, handle) = running.split();
    block_on(handle.stop())?;
    block_on(task)?;
    Ok(frames)
}

/// The receiver as one block (`WlanReceiver`).
fn receive_single<S: Standard>(
    samples: Vec<Complex32>,
    chunks: Option<u64>,
) -> Result<Vec<Vec<u8>>> {
    let mut fg = Flowgraph::new();
    let src = fg.add(ChunkSource {
        output: DefaultCpuWriter::default(),
        samples,
        pos: 0,
        chunks: chunks.map(Rng::new),
    })?;
    let rx = fg.add(Receiver::<S>::new(sync_short::THRESHOLD, false))?;
    let (tx, frames_rx) = mpsc::channel(10_000);
    let pipe = fg.add(MessagePipe::new(tx))?;
    fg.stream_dyn(src.id(), "output", rx.id(), "input")?;
    fg.message(rx.id(), "rx_frames", pipe.id(), "in")?;
    let running = Runtime::new().start(fg)?;
    let mut frames = Vec::new();
    let since = Instant::now();
    loop {
        match frames_rx.try_recv() {
            Ok(Pmt::Blob(frame)) => frames.push(frame),
            Ok(Pmt::Finished) => break,
            Ok(p) => panic!("{p:?}"),
            Err(_) if since.elapsed() > Duration::from_secs(120) => panic!("no end of stream"),
            Err(_) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    let (task, handle) = running.split();
    block_on(handle.stop())?;
    block_on(task)?;
    Ok(frames)
}

fn hex(frames: &[Vec<u8>]) -> Vec<String> {
    frames
        .iter()
        .map(|f| f.iter().map(|b| format!("{b:02x}")).collect())
        .collect()
}

#[test]
fn a_recordings_give_the_frames_of_examples_wlan() -> Result<()> {
    for name in ["bpsk-1-2-15db", "bpsk-3-4-30db"] {
        let samples = read_cf32(&wlan_data(&format!("{name}.cf32")));
        let frames = receive::<A>(samples, None, false)?;
        let want = expected(&format!("{name}.wlan.txt"));
        assert_eq!(hex(&frames), hex(&want), "{name}");
    }
    Ok(())
}

#[test]
fn a_frames_do_not_depend_on_chunks() -> Result<()> {
    let samples = read_cf32(&wlan_data("bpsk-1-2-15db.cf32"));
    let want = expected("bpsk-1-2-15db.wlan.txt");
    for seed in 0..3 {
        let frames = receive::<A>(samples.clone(), Some(seed), false)?;
        assert_eq!(hex(&frames), hex(&want), "seed {seed}");
    }
    Ok(())
}

#[test]
fn invalid_frames_are_posted_whole_on_demand() -> Result<()> {
    let samples = read_cf32(&wlan_data("bpsk-1-2-15db.cf32"));
    let valid = expected("bpsk-1-2-15db.wlan.txt");
    let all = receive::<A>(samples, None, true)?;
    // The valid frames, in order, and others whose FCS is wrong.
    let mut rest = valid.iter().peekable();
    for frame in &all {
        if rest.peek() == Some(&frame) {
            rest.next();
        } else {
            assert!(!fcs_ok(frame), "{:?}", hex(&[frame.clone()]));
        }
    }
    assert!(rest.next().is_none());
    Ok(())
}

/// The recorded HaLow frame between stretches of noise as strong as the
/// recording's.
fn halow_frame(rng: &mut Rng) -> Vec<Complex32> {
    let sigma = (10f32.powf(-4.81) / 2.0).sqrt();
    let mut noise = |n: usize| -> Vec<Complex32> {
        (0..n)
            .map(|_| Complex32::new(normal(rng), normal(rng)) * sigma)
            .collect()
    };
    let mut samples = noise(16_000);
    samples.extend(read_cf32(&testdata("halow_frame.cf32")));
    samples.extend(noise(16_000));
    samples
}

#[test]
fn ah_recording_gives_the_frame_of_v6() -> Result<()> {
    let samples = halow_frame(&mut Rng::new(1));
    let frames = receive::<Ah>(samples, None, false)?;
    assert_eq!(hex(&frames), hex(&expected("halow_frame.v6.txt")));
    Ok(())
}

#[test]
fn ah_frames_do_not_depend_on_chunks() -> Result<()> {
    let want = expected("halow_frame.v6.txt");
    for seed in 0..3 {
        let samples = halow_frame(&mut Rng::new(seed));
        let frames = receive::<Ah>(samples, Some(seed), false)?;
        assert_eq!(hex(&frames), hex(&want), "seed {seed}");
    }
    Ok(())
}

/// The front end built block by block sees the same frames as the fused
/// one, however the samples come.
#[test]
fn the_granular_receiver_gives_the_same_frames() -> Result<()> {
    for name in ["bpsk-1-2-15db", "bpsk-3-4-30db"] {
        let samples = read_cf32(&wlan_data(&format!("{name}.cf32")));
        let want = expected(&format!("{name}.wlan.txt"));
        for chunks in [None, Some(1)] {
            let frames = receive_with::<A>(samples.clone(), chunks, false, true, false)?;
            assert_eq!(hex(&frames), hex(&want), "{name} {chunks:?}");
        }
    }
    let want = expected("halow_frame.v6.txt");
    for seed in 0..3 {
        let samples = halow_frame(&mut Rng::new(seed));
        let frames = receive_with::<Ah>(samples, Some(seed), false, true, false)?;
        assert_eq!(hex(&frames), hex(&want), "seed {seed}");
    }
    Ok(())
}

/// The receiver as one block decodes what the four blocks decode.
#[test]
fn the_single_block_receiver_gives_the_same_frames() -> Result<()> {
    for name in ["bpsk-1-2-15db", "bpsk-3-4-30db"] {
        let samples = read_cf32(&wlan_data(&format!("{name}.cf32")));
        let want = expected(&format!("{name}.wlan.txt"));
        for chunks in [None, Some(1), Some(2)] {
            let frames = receive_single::<A>(samples.clone(), chunks)?;
            assert_eq!(hex(&frames), hex(&want), "{name} {chunks:?}");
        }
    }
    let want = expected("halow_frame.v6.txt");
    for seed in 0..3 {
        let frames = receive_single::<Ah>(halow_frame(&mut Rng::new(seed)), Some(seed))?;
        assert_eq!(hex(&frames), hex(&want), "seed {seed}");
    }
    Ok(())
}

/// Undoing the code without Viterbi gives the same frame when nothing is
/// wrong (the HaLow frame is at rate 1/2), and on a noisy recording no
/// frame Viterbi does not give.
#[test]
fn the_hard_decoder_decodes_clean_frames_only() -> Result<()> {
    let want = expected("halow_frame.v6.txt");
    let frames = receive_with::<Ah>(halow_frame(&mut Rng::new(1)), None, false, false, true)?;
    assert_eq!(hex(&frames), hex(&want));

    let samples = read_cf32(&wlan_data("bpsk-1-2-15db.cf32"));
    let viterbi = hex(&expected("bpsk-1-2-15db.wlan.txt"));
    let hard = hex(&receive_with::<A>(samples, None, false, false, true)?);
    assert!(hard.iter().all(|f| viterbi.contains(f)), "{hard:?}");
    eprintln!(
        "bpsk 1/2 at 15 dB: {} frames by Viterbi, {} without",
        viterbi.len(),
        hard.len()
    );
    Ok(())
}

/// The one-second recording the HaLow frame was cut from (32 MB, in the
/// `dyn` branch's `examples/real_device_swap/recording/halow_raw.cf32`),
/// named by `WLAN_HALOW_RECORDING`.
#[test]
#[ignore = "needs the recording in WLAN_HALOW_RECORDING"]
fn ah_long_recording_gives_the_frames_of_v6() -> Result<()> {
    let path = std::env::var("WLAN_HALOW_RECORDING").expect("WLAN_HALOW_RECORDING");
    let samples = read_cf32(&PathBuf::from(path));
    let want = expected("halow_raw.v6.txt");
    let (mut best, mut cpu) = (f64::INFINITY, f64::INFINITY);
    for _ in 0..5 {
        let (since, cpu_since) = (Instant::now(), cpu_time());
        let frames = receive_single::<Ah>(samples.clone(), None)?;
        best = best.min(since.elapsed().as_secs_f64());
        cpu = cpu.min(cpu_time() - cpu_since);
        assert_eq!(hex(&frames), hex(&want));
    }
    eprintln!(
        "single  : {} samples in {best:.4} s: {:.1} MSps, {:.0} ms of CPU",
        samples.len(),
        samples.len() as f64 / best / 1e6,
        cpu * 1e3
    );
    for granular in [false, true] {
        let (mut best, mut cpu) = (f64::INFINITY, f64::INFINITY);
        for _ in 0..5 {
            let (since, cpu_since) = (Instant::now(), cpu_time());
            let frames = receive_with::<Ah>(samples.clone(), None, false, granular, false)?;
            best = best.min(since.elapsed().as_secs_f64());
            cpu = cpu.min(cpu_time() - cpu_since);
            assert_eq!(frames.len(), want.len());
            assert_eq!(hex(&frames), hex(&want));
        }
        let rate = samples.len() as f64 / best / 1e6;
        let front = if granular { "granular" } else { "fused" };
        eprintln!(
            "{front:8}: {} samples in {best:.4} s: {rate:.1} MSps, {:.0} ms of CPU",
            samples.len(),
            cpu * 1e3
        );
    }
    Ok(())
}

/// Seconds of CPU time this process used (Linux), including the test
/// harness's own few milliseconds.
fn cpu_time() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .map_or("", |(_, rest)| rest)
        .split_whitespace()
        .collect();
    let ticks = |i: usize| {
        fields
            .get(i)
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    // utime and stime, in clock ticks of 10 ms.
    (ticks(11) + ticks(12)) / 100.0
}
