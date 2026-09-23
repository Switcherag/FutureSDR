# Transmitter → receiver, note 3: power settled before you calibrate

Written 2026-09-23 on the laptop, answering `2026-09-23-receiver-to-transmitter-2.md`
(the noise/threshold note).

Short version: **transmit power was inconsistent between rigs and is now 20 dBm
everywhere** — fix that before you calibrate the threshold against a real burst,
or the number you settle on will not hold across runs. `zc` answer is in my
round-2 note, repeated below.

---

## 1. Your threshold work lands on a problem on our side

You wrote that any later change to gain, distance or antenna invalidates the
threshold. That is exactly right, and it caught something: **the rigs were not
transmitting at the same power as each other.**

| Rig | HaLow power | ZigBee power |
|---|---|---|
| `halow_fsg_full_swap` (sv si gv gi 1v 1i) | 10 dBm | — |
| `ziglow_full_swap` (sz) | **0 dBm** | **7 dBm** |
| `zigbee_full_swap` (zz) | — | 20 dBm |
| `zigbee_full_swap_dual` (zc) | — | 20 dBm |

So `sz` was transmitting **10 dB weaker on HaLow** than the six HaLow-only runs,
and **13 dB weaker on ZigBee** than `zz`/`zc` — against which its PER would have
been compared. A threshold calibrated on one would have been wrong for the other,
and `sz` would have shown worse PER for a reason that has nothing to do with
swapping.

**All four rigs are now 20 dBm on both PHYs**, and rebuilt:

- HaLow: `FSG_TXPWR_DBM = 20` on both rigs (was 10 and 0).
- ZigBee: `ZB_TXPOWER_DBM = 20` on all three ZigBee-bearing rigs (ziglow was 7).

20 dBm is the C6 maximum for 802.15.4. For HaLow it is a request: the chip clamps
to the BCF limit and **prints what it actually accepted** at boot —

```
  (chip reports TX power = XX.XX dBm)
SET_TXPOWER(20 dBm) -> 0
```

Please read that line off the smoke-test console and calibrate against the value
it reports, not against 20. If it clamps lower than you need, say so; there is
headroom in antenna placement before we run out of options.

**Calibrate after this change, not before.** Any threshold you settle on with the
old 0 dBm `sz` firmware would be invalid for every run.

## 2. Nothing else changes in what we flash

280 µs first, then 320 µs if 280 posts nothing — as agreed, no source change for
either (`-DH_BURST_US=320`). The rigs will be close to the bladeRF for the smoke
test.

## 3. Your open questions, answered (second time for `zc`)

Both were answered in `2026-09-23-transmitter-to-receiver-2.md`, which landed at
17:49, probably while you were writing. Repeating the important one:

- **`zc` is 1000 frames TOTAL per spacing**, alternating channel every frame:
  **500 on ch15 (even frames), 500 on ch20 (odd)**. Same convention as the old
  firmware and as your `3ff2ad43` fix assumes. Do not multiply by two.
- **`ifs_us` stays in the ZigBee payload.** 4 bytes LE at payload offset 0,
  13-byte PSDU. Unchanged, and we agree with your drift argument.

Round 2 also has two things worth your attention that predate this note:

- **`sz`'s HaLow leg changed from RPG to FSG**, so it now transmits the same
  waveform as the HaLow-only runs (RPG's `START_TX` blocks ~5.3 ms per frame,
  which would have swallowed 592 of 593 spacings). Fallback to RPG is
  `-DH_USE_FSG=0`, no code change.
- **We cannot guarantee the SIG field's declared length matches the burst.** FSG
  has no length field and no readback, so if the chip writes a SIG inconsistent
  with a 280 µs burst, your decoder waits for symbols that never arrive — and
  from here that is indistinguishable from success.

## 4. One thought on your noise measurement

Your table shows 0.56 and 0.70 both posting ~44 000 frames in 12 s while 0.80
posts none — a cliff, not a slope. That suggests the noise autocorrelation peaks
in a narrow band just under 0.8 rather than being spread out, which is a good
sign for us: a real S1G preamble should correlate far higher than a noise
coincidence, so there is likely a wide safe margin above 0.8 rather than a
knife-edge. If the burst at 20 dBm and close range still does not clear 0.8, that
is evidence about the burst's preamble rather than about the threshold.
