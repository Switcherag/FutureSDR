# Receiver side → transmitter side, round 2

Written 2026-09-23 on the Pi (`framboise`), answering your transmitter note.
Short version: **set the HaLow FSG burst to 280 µs**, keep the 4-byte `ifs_us`
payload on the ZigBee frames, and expect three extra sweeps at the end of the
series for the library load/unload experiment.

---

## 1. HaLow: 280 µs is right, and here is why it is the threshold

You found a data ramp above 240 µs. That matches the receiver exactly, which is
a good sign that we are looking at the same boundary from two sides.

For `Ah` (S1G 2 MHz at 4 MSps) the receiver's symbol is `FFT_SIZE` 128 +
`CP_LEN` 32 = 160 samples = **40 µs**. Before one data symbol it needs:

| Stage | Cost |
|---|---|
| STF detection (moving averages warm up over 224 samples) | ~56 µs plus the threshold plateau |
| 2 LTF symbols → channel estimate | 80 µs |
| SIG field → MCS, length, aggregation bit | 80 µs |

The code budgets six symbols of overhead for this
(`MAX_SAMPLES = (6 + MAX_SYMBOLS) * SYMBOL_LEN`), i.e. **240 µs before data**.
So 240 µs is all preamble and no payload — which is why nothing was posted — and
**280 µs gives exactly one data symbol**.

Please also prepare **320 µs** (two data symbols) as a second test point. If 280
posts frames and 320 posts frames, the burst carries a real SIG field and we are
done. If neither posts while the energy is clearly on air, the burst has no
usable SIG and we are back to the question of whether the MM6108 can transmit
real PPDUs back to back.

**What the receiver needs from the burst, beyond length.** All three must hold,
and all three are about the SIG field rather than the duration:

1. The SIG field must decode: its CRC must check out and the receiver must
   accept the MCS and length it declares.
2. The declared length must match what is actually emitted. If SIG says three
   data symbols and the burst carries one, the decoder waits for symbols that
   never arrive and cancels the frame — no post, same visible result as a frame
   that was never detected.
3. If the SIG's aggregation bit is set, the A-MPDU parser may find no subframe
   delimiters in what is effectively noise and post nothing. Non-aggregated is
   the safe setting. If you cannot control that bit, tell us: it is a three-line
   fallback on our side (post the raw PSDU when the parse yields nothing).

With a non-aggregated SIG, **any** decoded payload is posted, including a
one-byte garbage PSDU, because our flows run `invalid_frames = true`. So you do
not need the payload to be meaningful — only the preamble and SIG to be real.

## 2. ZigBee: please keep the 4-byte `ifs_us` payload

You wrote that the 500 ms separation between spacings removes the need to number
frames. It removes the need for a frame *number*, and we agree — but please do
not drop `ifs_us` itself from the ZigBee payload. It costs 4 bytes in a 13-byte
PSDU, far under your 18-byte boundary, and it buys two things that the pause
separation cannot:

- **Immunity to drift.** Cutting a stream into groups at the pauses identifies a
  spacing only by its position in the sequence. If one spacing yields no frames
  at all, or a run of consecutive losses splits one group into two, every later
  spacing shifts by one step and the curve is silently wrong. That is the same
  class of failure as the 0.01/0.1 ms mix-up that cost the 2026-09-22 run, and
  it is likeliest exactly where this experiment gets interesting: the low end,
  where loss is high.
- **A cross-check on your sweep.** With the IFS in the frame, the receiver can
  confirm that the transmitter's spacing is the one the analysis assumes,
  per frame, rather than assuming it.

HaLow cannot carry it, so the HaLow runs will be placed by pause-cutting and will
have to live with that fragility. That is a reason to keep the ZigBee runs sound,
not a reason to make them equally fragile.

We are not asking for the frame number or the step index any more: with `ifs_us`
in the payload and 1000 frames per spacing known from the firmware, PER is
countable. If widening to 17 B is free for you, the step index is still a nice
independent check, but it is no longer a request.

**Timing cross-check (our side, no action for you).** Within a group, the median
gap between arrivals is IFS + frame time, which identifies the spacing on its
own. We will use it to verify the pause-cut mapping wherever the gaps are still
distinguishable — i.e. above your ~150 µs ZigBee clamp.

## 3. Still open from our side

1. **`zc` frames per spacing**: is `FRAMES_PER_STEP=1000` the total across both
   channels (500 each, as the old firmware did — ch15 even numbers, ch20 odd) or
   1000 per channel? The analysis divides by it.
2. `sz` at 500 + 500 is right, keep it. It matches the previous run and keeps
   1000 frames per spacing across every run.

## 4. The small test, in order

1. Flash the HaLow rig at **280 µs**, 10 frames per spacing (`-DFSG_FRAMES=10`,
   after `idf.py fullclean` — thank you for the warning about the cache).
2. We run `--listen halow_simple_nocrc_chan34.toml`, which prints every decoded
   frame in hex. Ten frames at one spacing answers it.
3. If nothing: reflash at 320 µs and repeat.
4. If still nothing: we capture raw samples and look at where the chain stops
   (detection, LTF, or SIG), and you look at whether the burst can carry a real
   preamble.
5. Once frames appear, the 10-frame full sweep (≈5 min, dominated by the pauses)
   for one ZigBee key and one HaLow key, plotted with `--frames-per-step 10`, to
   confirm the 593 groups line up before the long series.

## 5. What the warm/cold library experiment asks of you

Separate from the PER series, we are making each decoder its own plugin library,
so that a swap can load the code it needs and unload the code it replaces. Three
conditions get measured: the library **resident** (as today), **reloaded warm**
(unloaded after each swap, file still in the page cache), and **reloaded cold**
(the file evicted first). It answers "what does a first change cost against a
re-change", which is your ramp question in software form.

Most of that is measured on the replayed front end and needs no transmitter at
all. What we do want over the air is one comparison, because loading code on
demand lengthens a swap and therefore eats IFS headroom, which shows up as the
PER curve shifting right:

- **One receiver pair** (HaLow simple, decoder replaced in place), **three
  sweeps** — resident, warm reload, cold reload — at the normal 1000 frames per
  spacing. That is about 2.5 h of extra transmitter time, after the nine runs of
  the main series.

Nothing changes on your side for those: same firmware, same sweep, run three
times. We will say when.
