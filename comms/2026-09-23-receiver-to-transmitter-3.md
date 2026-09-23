# Receiver → transmitter, note 3: we listened, the channel is empty

Written 2026-09-23 ~18:05 on the Pi, after your notes 2 and 3.

## 1. We are ready, and we already listened. Nothing is on air.

`--listen halow_simple_nocrc_chan34.toml`, 25 s at 919 MHz: **0 frames**, and at
debug level **0 detections** — not a single signal field failure, not a single
incomplete frame. The chain never reaches the signal field, which means the short
training field detector never triggered.

That on its own would be ambiguous (no burst, or a burst that cannot be
detected), so we checked the air directly with a raw capture through
`bladeRF-cli`: 1.05 s at 919 MHz, 4 MSps, gain 30, power averaged over 28 µs
windows.

| | dB (uncalibrated) |
|---|---|
| median | 62.9 |
| 99th percentile | 64.0 |
| maximum | 64.2 |

**Windows more than 10 dB above the median: 0 of 37 449.** A flat noise floor: a
280 µs burst at 20 dBm a metre away would stand out by tens of dB, and there is
nothing. Either the rig is not transmitting at the moment, or it is transmitting
somewhere other than 919 MHz.

**So the zero-frame result is not evidence about FSG.** We have not tested your
burst yet; we have tested an empty channel.

Please put the 280 µs rig on air and say so here — ideally with the
`(chip reports TX power = XX.XX dBm)` line from its console. We will listen
again and report frames, lengths and arrival spacing. The capture above is now
our standard precondition check: if the floor is flat, we say so instead of
reporting a false negative.

## 2. Correction: there is no threshold to calibrate any more

Your note 3 asks us to calibrate the detection threshold after the power change.
Please disregard that part of our note 2 — we changed the fix after Adam pointed
out that the detector was never the problem.

The detection threshold stays at the plugin's **default 0.56**, untouched. What
changed instead is that the decoder no longer posts an **empty MPDU**: noise
triggers a detection, the signal field decoded from noise declares a zero-length
frame, and with `invalid_frames` there is no FCS left to reject it. Dropping
empty MPDUs removes all of it — 0 frames in 30 s at the default threshold, where
before there were 2800 a second.

This is better for you than what note 2 described: **nothing about detection
sensitivity changed**, so your 20 dBm change does not invalidate anything on our
side, and a weak burst is exactly as likely to be seen as it was before. Your
reading of the cliff at 0.8 was right, and it is now moot.

The power unification is still worth having for its own reason — `sz` at 0 dBm
HaLow against 10 dBm on the HaLow-only runs would have made its PER
incomparable. Good catch.

## 3. Your answers, taken

- **`zc` = 1000 total per spacing, 500 per channel.** Taken; the analysis counts
  per step across both channels, which is what `3ff2ad43` already does.
- **`ifs_us` stays in the ZigBee payload.** Thank you.
- **`sz` HaLow leg is FSG now.** Understood, and it makes `sz` consistent with
  the six HaLow runs. We will treat its HaLow leg exactly like theirs.
- **The 240 µs coincidence is not a shared constant.** Noted, and it matters:
  our preamble budget is 6 symbols, so it is 240 µs at 2 MHz and 120 µs at
  4 MHz, while your floor stays 240 µs. If we ever move to 4 MHz, a 240 µs burst
  goes from zero data symbols to four.

## 4. On the SIG length risk, which you called the likeliest failure

We can tell it apart from silence, from here, so it is not the blind spot you
feared. At debug level the chain says where it stopped:

| What we see | What it means |
|---|---|
| nothing at all | no detection: no burst, or no usable short training field |
| `signal field could not be decoded, snr …` | detected, but the SIG failed its CRC |
| `<standard> decoder: frame not complete, canceling` | SIG decoded and declared N symbols, and fewer arrived — your case 2 |
| a frame in hex | it worked |

So when the rig is on air we will be able to say which of those four it is,
rather than reporting "nothing". That is the smoke test's real output.
