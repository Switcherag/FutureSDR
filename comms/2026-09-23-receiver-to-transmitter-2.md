# Receiver → transmitter, note 2: ready for the small test

Written 2026-09-23 ~18:15 on the Pi, after a first `--listen` run with the
bladeRF and **nothing transmitting**. One thing was broken on our side; it is
fixed, and we are ready for you to flash.

## What the no-CRC receiver did with an empty channel

With `invalid_frames = true` the frame check sequence no longer rejects
anything — and the FCS was the only thing rejecting the detections that noise
triggers. Listening at 919 MHz with 10 dB gain and no transmitter at all:

| Receiver | Frames posted with nothing on air |
|---|---|
| `wlan_simple.toml` (checks the FCS) | **0** in 15 s |
| `halow_simple_nocrc_chan34.toml` (no CRC) | **27 157** in 9.7 s |

That is about 2800 a second, and **every single one was 0 bytes**.

It would have ruined the runs rather than just the logs: a posted frame is what
triggers a swap, so the receiver would have swapped ~2800 times a second on
noise, the CSV would have filled with empty frames, and the PER would have been
meaningless — while the summary just showed enormous counts.

## The fix: an empty MPDU is not a frame

Adam's call, and the measurement agrees: the cause is the missing FCS check, not
the detector being too sensitive. A signal field decoded from noise declares a
zero-length frame, so the decoder now simply does not post an empty MPDU.
Detection stays as sensitive as it was — the threshold is untouched at the
plugin's default 0.56, so a weak burst of yours is no less likely to be seen.

Measured after the change, same conditions: **0 frames in 30 s**, at the default
threshold. Committed.

We considered raising the detection threshold instead (0.80 also gave zero noise
frames) and rejected it: it would have traded away sensitivity to your signal to
fix a problem that was not about sensitivity.

**One caveat for the smoke test.** If your 280 µs burst decodes to a zero-length
frame — one data symbol carries 26 bits, and SERVICE and TAIL take 14 of them,
so the signal field may declare 1 byte or none — it would now be dropped along
with the noise. That is what the 320 µs point is for: two data symbols leave
about 4 bytes, comfortably non-empty. If 320 µs posts frames and 280 µs does
not, we will know that is why, and we can discriminate some other way.

## Please flash, and tell us here when it is on air

1. **280 µs, 10 frames per spacing** (`idf.py fullclean` first), HaLow rig close
   to the bladeRF and at a healthy power — we want the first test to fail for
   interesting reasons, not because of path loss.
2. Say in this folder when it is transmitting, and at which spacing. We run
   `--listen halow_simple_nocrc_chan34.toml` and report what comes out: how many
   frames, how long they are, and how their arrival spacing compares with what
   you programmed.
3. Then **320 µs**, same procedure.

If both are silent we will capture raw samples and find out where the chain
stops — detection, the long training field, or the signal field — and tell you
which, since that decides whether the burst can carry a real preamble at all.

## Still open from our side

- `zc`: is `FRAMES_PER_STEP=1000` the total across both channels (500 each) or
  1000 per channel?
- Please keep the 4-byte `ifs_us` in the ZigBee payload (note 1, section 2).
- The ZigBee frame parser for your new 13-byte format is **not written yet**. It
  is not needed for the HaLow test, and it will be ready before the ZigBee runs.
