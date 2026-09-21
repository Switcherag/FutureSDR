//! Recorded frames in place of the radio.
//!
//! One 802.11ah (H) and one 802.15.4 (Z) recording, at 4 MSps, replayed
//! H Z H Z ... on the wall clock. The gap after an H frame is the time the
//! receiver has to change from HaLow to ZigBee (and retune from 919 MHz to
//! 2.425 GHz); the gap after a Z frame, the time to change back. Each step
//! of a run replays one stream, with the gap after H of that step.
//!
//! A `Tune` block after the replay offers the `frequency` control a radio
//! would: setting it takes as long as the radio's retune, and the samples
//! that arrive meanwhile are lost.

use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use anyhow::Result;
use futuresdr::runtime::Timer;
use futuresdr::runtime::dev::prelude::*;
use plugin_api::BlockType;
use plugin_api::Plugin;
use plugin_api::add_kernel;
use plugin_host::ReuseCpuReader;
use plugin_host::ReuseCpuWriter;

pub const RATE: f64 = 4e6;
/// Silence before the first frame and after the last.
const LEAD: Duration = Duration::from_millis(50);

/// The streams to replay, by step; the `Replay` block takes its `step`.
static STREAMS: Mutex<Vec<Arc<Vec<Complex32>>>> = Mutex::new(Vec::new());
/// When the replay of the current step emitted its first sample.
pub static STARTED: Mutex<Option<Instant>> = Mutex::new(None);
/// Samples the replay emits at once, as a radio delivers them.
pub static CHUNK: AtomicUsize = AtomicUsize::new(256);
/// Chunks emitted more than 0.2 ms after they were due: (when, seconds
/// from the replay's start; how late, seconds).
pub static LATE_CHUNKS: Mutex<Vec<(f64, f64)>> = Mutex::new(Vec::new());
/// Samples the replay's output holds, as a radio's buffers do before they
/// overflow.
pub static SOURCE_BUFFER: AtomicUsize = AtomicUsize::new(13_107);
/// Samples the replay dropped, as a radio would, because its output had no
/// room for them in time.
pub static SOURCE_DROPPED: AtomicUsize = AtomicUsize::new(0);
/// How long each `Tune` retune took (always its delay; for the report).
pub static RETUNES: Mutex<Vec<Duration>> = Mutex::new(Vec::new());

/// CPUs for the source's thread (the replay's, or the radio's), away from
/// the runtime's; empty: those it inherits.
pub static SOURCE_CPUS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// Restrict the calling thread to `cpus` (threads it starts inherit them).
pub fn set_thread_cpus(cpus: &[usize]) -> Result<()> {
    // SAFETY: a zeroed cpu_set_t is an empty set; the calls only read and
    // write the set passed.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        for &cpu in cpus {
            libc::CPU_SET(cpu, &mut set);
        }
        if libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
            anyhow::bail!(
                "setting the CPU affinity to {cpus:?}: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    Ok(())
}

/// Real-time priority (SCHED_FIFO) for the runtime's threads and the
/// source's; 0: none.
pub static RT_PRIORITY: AtomicUsize = AtomicUsize::new(0);

/// Give the calling thread SCHED_FIFO at `priority` (threads it starts
/// inherit it).
pub fn set_thread_rt_priority(priority: usize) -> Result<()> {
    let param = libc::sched_param {
        sched_priority: priority as i32,
    };
    // SAFETY: sets the calling thread's policy from a valid parameter.
    if unsafe { libc::sched_setscheduler(0, libc::SCHED_FIFO, &param) } != 0 {
        anyhow::bail!(
            "real-time priority {priority}: {} (grant it with `ulimit -r` / \
             /etc/security/limits.conf rtprio, or setcap cap_sys_nice+ep on the binary)",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

/// Keep `cpus` out of their idle states and at full clock without taking
/// them from anyone: a thread on each, spinning at the lowest priority
/// (SCHED_IDLE), which any other thread there preempts at once. An idle
/// core of this laptop sleeps in C3, which takes about 1 ms to leave, and
/// its clock falls to 800 MHz; a runtime thread woken there starts late.
/// Costs a core's power each; `tuned-adm profile latency-performance` does
/// the same with root (idle states up to C1, minimum clock 100 %).
pub fn keep_awake(cpus: &[usize]) -> Result<()> {
    for &cpu in cpus {
        std::thread::Builder::new()
            .name(format!("awake-{cpu}"))
            .spawn(move || {
                let param = libc::sched_param { sched_priority: 0 };
                // SAFETY: sets the calling thread's policy from a valid
                // parameter; SCHED_IDLE needs no privilege.
                unsafe { libc::sched_setscheduler(0, libc::SCHED_IDLE, &param) };
                if set_thread_cpus(&[cpu]).is_ok() {
                    loop {
                        // Into the kernel and back each turn: with lazy
                        // preemption, a spinner that stays in user space
                        // gives way to a woken thread only at the next
                        // tick, up to a millisecond later.
                        // SAFETY: no arguments, no memory.
                        unsafe { libc::sched_yield() };
                    }
                }
            })?;
    }
    Ok(())
}

/// Move the calling thread to the source's CPUs, if there are any, and give
/// it the real-time priority asked for.
pub fn pin_source_thread() -> Result<()> {
    let cpus = SOURCE_CPUS.lock().unwrap().clone();
    if !cpus.is_empty() {
        set_thread_cpus(&cpus)?;
    }
    match RT_PRIORITY.load(Ordering::Relaxed) {
        0 => Ok(()),
        priority => set_thread_rt_priority(priority),
    }
}

/// Wait until `t`: asleep until shortly before, then spinning, since a
/// sleep overshoots by tens of microseconds.
fn wait_until(t: Instant) {
    loop {
        let now = Instant::now();
        if now >= t {
            return;
        }
        let left = t - now;
        if left > Duration::from_micros(200) {
            std::thread::sleep(left - Duration::from_micros(150));
        } else {
            std::hint::spin_loop();
        }
    }
}

/// Seconds since the replay of the current step began.
pub fn now() -> f64 {
    STARTED
        .lock()
        .unwrap()
        .map_or(0.0, |start| start.elapsed().as_secs_f64())
}

/// One transmitted frame: its PHY (0 H, 1 Z) and when it began and ended,
/// in seconds from the first replayed sample.
#[derive(Debug, Clone, Copy)]
pub struct Sent {
    pub phy: usize,
    pub start: f64,
    pub end: f64,
}

/// A replay run: one stream per step, and what each sent when.
pub struct Steps {
    /// The IFS after the first receiver's frames, by step.
    pub ifs_ms: Vec<f64>,
    /// The IFS after the second receiver's frames, by step.
    pub ifs_after_second_ms: Vec<f64>,
    pub sent: Vec<Vec<Sent>>,
}

pub fn read_cf32(path: &Path) -> Result<Vec<Complex32>> {
    let bytes = std::fs::read(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    Ok(bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|[a, b, c, d, e, f, g, h]| {
            Complex32::new(
                f32::from_le_bytes([*a, *b, *c, *d]),
                f32::from_le_bytes([*e, *f, *g, *h]),
            )
        })
        .collect())
}

/// Where the frame is in a recording: from the first to the last sample
/// whose power, averaged over 8 µs, is within 6 dB of the frame's (its 90th
/// percentile). The recordings are cut with some silence around the frame,
/// which would otherwise add to every IFS.
pub fn burst(recording: &[Complex32]) -> std::ops::Range<usize> {
    let power: Vec<f64> = recording.iter().map(|x| x.norm_sqr() as f64).collect();
    let mut sorted = power.clone();
    sorted.sort_by(f64::total_cmp);
    let reference = sorted[sorted.len() * 9 / 10];
    let w = (8e-6 * RATE) as usize;
    // Centred moving average, as numpy's convolve(..., "same").
    let mut prefix = vec![0.0; power.len() + 1];
    for (k, p) in power.iter().enumerate() {
        prefix[k + 1] = prefix[k] + p;
    }
    let above = |k: usize| {
        let lo = (k + w / 2).saturating_sub(w - 1);
        let hi = (k + w / 2 + 1).min(power.len());
        (prefix[hi] - prefix[lo]) / w as f64 > reference * 10f64.powf(-0.6)
    };
    let start = (0..power.len()).find(|&k| above(k)).unwrap_or(0);
    let end = (0..power.len())
        .rfind(|&k| above(k))
        .map_or(power.len(), |k| k + 1);
    start..end
}

/// How to make the stream of each step.
struct Plan {
    recordings: [Arc<Vec<Complex32>>; 2],
    frames: usize,
    lead: usize,
    /// Samples of silence after each recording, by step.
    gaps: Vec<[usize; 2]>,
}

static PLAN: Mutex<Option<Plan>> = Mutex::new(None);

/// Plan the streams: `frames` frames per step, `recordings[0]` (of PHY
/// `phys[0]`) first, then `recordings[1]`, in turn; after the first ones
/// the step's IFS from `ifs_ms`, after the second ones `ifs_after_second_ms`
/// (the step's IFS too if `None`). A step's stream is made by [`load`] just
/// before it runs: all of them at once would not fit in memory (4 ms apart,
/// 400 frames are 50 MB).
pub fn prepare(
    recordings: [&[Complex32]; 2],
    phys: [usize; 2],
    frames: usize,
    ifs_ms: Vec<f64>,
    ifs_after_second_ms: Option<f64>,
) -> Steps {
    let samples = |ms: f64| (ms.max(0.0) / 1e3 * RATE).round() as usize;
    let lead = samples(LEAD.as_secs_f64() * 1e3);
    let mut sent_per_step = Vec::new();
    let mut after_second = Vec::new();
    let mut gaps_per_step = Vec::new();
    for &ifs in &ifs_ms {
        let second = ifs_after_second_ms.unwrap_or(ifs);
        after_second.push(second);
        let gaps = [samples(ifs), samples(second)];
        // Where each frame goes, as `load` puts it.
        let mut at = lead;
        let mut sent = Vec::new();
        for i in 0..frames {
            let k = i % 2;
            let start = at;
            at += recordings[k].len();
            sent.push(Sent {
                phy: phys[k],
                start: start as f64 / RATE,
                end: at as f64 / RATE,
            });
            at += if i + 1 == frames { lead } else { gaps[k] };
        }
        sent_per_step.push(sent);
        gaps_per_step.push(gaps);
    }
    *PLAN.lock().unwrap() = Some(Plan {
        recordings: [
            Arc::new(recordings[0].to_vec()),
            Arc::new(recordings[1].to_vec()),
        ],
        frames,
        lead,
        gaps: gaps_per_step,
    });
    STREAMS.lock().unwrap().clear();
    Steps {
        ifs_ms,
        ifs_after_second_ms: after_second,
        sent: sent_per_step,
    }
}

/// Make the stream of step `step`, for its `Replay` block.
pub fn load(step: usize) {
    let plan = PLAN.lock().unwrap();
    let plan = plan.as_ref().expect("prepared");
    let gaps = plan.gaps[step];
    let mut out = vec![Complex32::default(); plan.lead];
    for i in 0..plan.frames {
        let k = i % 2;
        out.extend_from_slice(&plan.recordings[k]);
        let gap = if i + 1 == plan.frames {
            plan.lead
        } else {
            gaps[k]
        };
        out.resize(out.len() + gap, Complex32::default());
    }
    let mut streams = STREAMS.lock().unwrap();
    if streams.len() <= step {
        streams.resize(step + 1, Arc::new(Vec::new()));
    }
    streams[step] = Arc::new(out);
}

/// Free the stream of step `step`.
pub fn unload(step: usize) {
    if let Some(stream) = STREAMS.lock().unwrap().get_mut(step) {
        *stream = Arc::new(Vec::new());
    }
}

/// The radio flowgraph of step `step`: the replay, and a front end that
/// takes `retune_us` to change frequency, or none if `retune_us` is 0 (the
/// receivers' `[radio]` demands then go nowhere: a software swap only).
pub fn head(step: usize, retune_us: u64) -> String {
    if retune_us == 0 {
        return format!(
            r#"
            name = "radio"
            [blocks.replay]
            type = "Replay"
            step = {step}
            [outputs]
            samples = {{ port = "replay.output", type = "c32" }}
            "#
        );
    }
    format!(
        r#"
        name = "radio"
        connections = "replay > tune"
        [blocks.replay]
        type = "Replay"
        step = {step}
        [blocks.tune]
        type = "Tune"
        delay_us = {retune_us}
        [outputs]
        samples = {{ port = "tune.output", type = "c32" }}
        [controls]
        frequency = "tune.freq"
        "#
    )
}

/// Emits a stream at `RATE` samples per second of wall clock, in whole
/// chunks, from its first call on. It runs on a thread of its own, as a
/// radio's driver does: on the runtime's threads, it would wait for them
/// whenever they are busy (building a receiver, say), and then deliver
/// what it owes in a burst, which closes the gaps between frames.
#[derive(Block)]
#[blocking]
struct Replay {
    #[output]
    output: ReuseCpuWriter<Complex32>,
    samples: Arc<Vec<Complex32>>,
    pos: usize,
    start: Option<Instant>,
    /// The most room its output ever had.
    room: usize,
    pinned: bool,
}

impl Kernel for Replay {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        if !std::mem::replace(&mut self.pinned, true) {
            pin_source_thread()?;
        }
        let start = *self.start.get_or_insert_with(|| {
            let now = Instant::now();
            *STARTED.lock().unwrap() = Some(now);
            now
        });
        let chunk = CHUNK.load(Ordering::Relaxed);
        let len = self.samples.len();
        if self.pos == len {
            io.finished = true;
            return Ok(());
        }
        // The end of the chunk the next sample is in.
        let next = (self.pos + chunk - self.pos % chunk).min(len);
        wait_until(start + Duration::from_secs_f64(next as f64 / RATE));

        let due = ((start.elapsed().as_secs_f64() * RATE) as usize).min(len);
        let owed = due - self.pos.min(due);
        let ready = if due == len {
            owed
        } else {
            owed - owed % chunk
        };
        let out = self.output.slice();
        self.room = self.room.max(out.len());
        // A radio does not wait for its reader: what does not fit is lost.
        let n = ready.min(out.len());
        out[..n].copy_from_slice(&self.samples[self.pos..self.pos + n]);
        self.output.produce(n);
        if n > 0 {
            // The last of these samples was due at (pos + n) / RATE.
            let now = start.elapsed().as_secs_f64();
            let late = now - (self.pos + n) as f64 / RATE;
            if late > 2e-4 {
                LATE_CHUNKS.lock().unwrap().push((now, late));
            }
        }
        if n < ready {
            SOURCE_DROPPED.fetch_add(ready - n, Ordering::Relaxed);
        }
        self.pos += ready;
        if self.pos == len {
            io.finished = true;
        } else {
            io.call_again = true;
        }
        Ok(())
    }
}

/// Passes samples on; message input `freq` takes `delay` to set, and the
/// samples that arrive meanwhile are lost, as with a radio front end.
#[derive(Block)]
#[message_inputs(freq)]
struct Tune {
    #[input]
    input: ReuseCpuReader<Complex32>,
    #[output]
    output: ReuseCpuWriter<Complex32>,
    delay: Duration,
    freq: f64,
    drop: bool,
}

impl Tune {
    async fn freq(
        &mut self,
        _io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
        p: Pmt,
    ) -> Result<Pmt> {
        let Pmt::F64(f) = p else {
            return Ok(Pmt::InvalidValue);
        };
        if f != self.freq {
            let t = Instant::now();
            Timer::after(self.delay).await;
            RETUNES.lock().unwrap().push(t.elapsed());
            self.freq = f;
            self.drop = true;
        }
        Ok(Pmt::Ok)
    }
}

impl Kernel for Tune {
    async fn work(
        &mut self,
        io: &mut WorkIo,
        _mo: &mut MessageOutputs,
        _meta: &BlockMeta,
    ) -> Result<()> {
        let i = self.input.slice();
        let len = i.len();
        if std::mem::take(&mut self.drop) {
            self.input.consume(len);
        } else {
            let o = self.output.slice();
            let n = len.min(o.len());
            o[..n].copy_from_slice(&i[..n]);
            self.input.consume(n);
            self.output.produce(n);
            if n < len {
                io.call_again = true;
            }
        }
        if self.input.finished() && self.input.slice().is_empty() {
            io.finished = true;
        }
        Ok(())
    }
}

/// The `Replay` and `Tune` block types.
pub fn blocks() -> Plugin {
    Plugin::new(
        "replay",
        vec![
            BlockType {
                name: "Replay".into(),
                description: "A prepared stream (setting `step`), at 4 MSps.",
                add: |fg, s| {
                    let step: usize = s.get("step")?;
                    let samples = STREAMS.lock().unwrap()[step].clone();
                    add_kernel(
                        fg,
                        Replay {
                            output: {
                                let mut output = ReuseCpuWriter::default();
                                output.set_min_buffer_size_in_items(
                                    SOURCE_BUFFER.load(Ordering::Relaxed),
                                );
                                output
                            },
                            samples,
                            pos: 0,
                            start: None,
                            room: 0,
                            pinned: false,
                        },
                    )
                },
            },
            BlockType {
                name: "Tune".into(),
                description: "A front end that takes `delay_us` to retune.",
                add: |fg, s| {
                    add_kernel(
                        fg,
                        Tune {
                            input: ReuseCpuReader::default(),
                            output: ReuseCpuWriter::default(),
                            delay: Duration::from_micros(s.get("delay_us")?),
                            freq: f64::NAN,
                            drop: false,
                        },
                    )
                },
            },
        ],
    )
}
