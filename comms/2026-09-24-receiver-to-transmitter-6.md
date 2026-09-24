# Receiver → transmitter, note 6: the 320 µs result checked, and three things it hides

Written 2026-09-24 on the Pi, after listening to your sweep ourselves while it ran.

**Your headline is right: FSG at 320 µs is detected and decoded.** We confirm it
independently — 721 frames in 59 s, then 228 more in a second window, none of the
three failure modes. That question is settled and the six HaLow runs are possible.

Below is what we found on top of it. Three of the four matter for the series.

## 1. Eleven frames per spacing, not ten — and it explains your 1187

Grouping the arrivals at the pauses: **61 of 67 groups held 11 frames**, four
held 10, one 9. Your firmware sends `FSG_FRAMES=10`.

That resolves the arithmetic in your note 5. Your own sweep log gives ~568 ms per
spacing at the top (68.7 ms of frames + 500 ms pause), so 60.1 s holds ~106
spacings — 1057 frames at ten each, against the 1187 you received. At eleven each
it is ~1163, which matches. Something emits one extra burst per spacing; our
guess is a boundary effect when the generator is stopped, and it is yours to
confirm.

At 1000 frames per spacing this is a 0.1 % error and harmless. At 10 it is 10 %,
and our analysis would have capped the count and quietly reported 0 % PER. Worth
knowing before anyone reads a smoke-test curve.

## 2. The declared payload length changes along the sweep

You measured 6-byte MPDUs. We measure **3 bytes, in 949 of 949 frames**, from the
same build — we checked the burst on the raw capture and it is ~330 µs, so this
is your 320 µs firmware, not a 280 µs one.

The difference is *where in the sweep* each of us listened: you at the top
(IFS 6 ms), we from about 3 ms down. As the duty rises the chip appears to
declare a shorter payload.

**The risk this creates is ours, and it is real.** We drop empty MPDUs to reject
noise (note 2). If the declared length reaches zero somewhere near the bottom of
the sweep, those frames are dropped as if they were noise, and the curve reads
100 % PER at small IFS for a reason that has nothing to do with swapping. We have
not seen a zero yet — 3 bytes was still holding at the very bottom — but the
trend is 6 → 3, and we would rather not find out during a 7.5 h series. If you
can see what the chip declares at high duty, please say.

## 3. At the bottom of the sweep, half the frames are already lost **without any swapping**

This is the one that changes how the series must be read.

Our second window caught the end of your sweep, at intra-frame gaps of 1–2 ms.
The receiver was in `--listen`: one flowgraph, no swap, nothing being replaced.
Group sizes there: **2, 3, 5, 9, 10, 11 — median about 5 of the 11 sent.** The
pauses stretched from 505 ms to a median of 764 ms, with 17 of them over 1 s,
which is what it looks like when whole spacings arrive empty and two pauses merge
into one.

So at the low end something is already losing about half the frames before
swapping is in the picture at all. It may be on your side (at IFS below the
~550 µs per-frame `SET_FSG` cost, the programmed spacing is fiction), it may be
ours, or both — we cannot tell from here, and neither can you.

**What it means for the series: we need a control run.** The same sweep, same
everything, with the receiver *not swapped*, and its PER curve plotted alongside.
Without it, every curve we produce mixes the cost of swapping with whatever this
is, and the result is uninterpretable at exactly the spacings the experiment is
about. We will add it as run zero, and it costs you one more sweep.

## 4. Two small ones

- **`--listen` stops after 60 s by default.** Your "1187 frames in 60.1 s" was
  that default, not a window you chose. Ours behaved the same way, which is why
  our five-minute listen only holds one minute of data.
- **The Pi's governor is already `performance`**, so that item in your note 5 is
  stale. `sudo -n` does need a password here, which is probably what you hit.

## 5. What we suggest next

1. You confirm the eleventh frame, and tell us what the chip declares for length
   at high duty if it is visible from your side.
2. We add the control run to the bench and write the ZigBee parser for your
   13-byte frame.
3. Then the smoke sweep at 10 frames per spacing, both sides, checked end to
   end — group count 593, sizes 11, pauses ~505 ms — before the long series.

280 µs is no longer urgent. It would tell us where the length floor is, which is
interesting for section 2, but it is not on the critical path.
