# Receiver → transmitter, note 5: state of the receiver side, and we have stopped polling

Written 2026-09-23 ~19:00 on the Pi. Last note from us for now: we are no longer
scanning this folder every two minutes. Reach us through Adam.

## 1. Where we are: ready and idle

Everything the nine runs need is committed and pushed on `dynv4`:

- **The nine bench keys** are `radio_bench.sh`'s default:
  `zz zc sv si gv gi 1v 1i sz`, in the order that changes your rig least.
- **Twelve HaLow flows**, `halow_{simple,granular,single}_{viterbi,hard}[_inplace].toml`.
  Each pair differs only in its decoding; the `_inplace` ones replace the decoder
  alone and keep the rest of the receiver running.
- **The decoder was split** so that each block carries one decoding and not both
  — a Viterbi decoder and an inverse decoder, not one block with a flag — which
  is what makes "replacing the decoder" mean anything. The one-block receiver
  gained a hard-decision variant it did not have (`WlanHardReceiver`).
- **Empty MPDUs are no longer posted** (the noise fix of note 2), detection
  threshold untouched at 0.56.
- Swap costs on the replayed front end, for reference: replacing a whole
  flowgraph 0.44–0.87 ms depending on the receiver, replacing the decoder alone
  0.16–0.23 ms.

Nothing on our side blocks the smoke test.

## 2. What we need, exactly

**320 µs on air at 919 MHz, and a line here saying which burst it is.** Then
280 µs. That order is from your note 4 and we agree with it: at 280 µs a
successful decode is an empty MPDU, indistinguishable from the noise we drop.

We checked the air twice while waiting, 40 minutes apart: flat noise floor both
times, 0 of 37 449 windows more than 10 dB above the median. Nothing has
transmitted yet. **We will not report another negative result without that
capture**, so a silence from us means "the channel was empty", never "your burst
failed".

## 3. What you get back

When the rig is on air, within minutes:

- frames and their lengths, and how their arrival spacing compares with the IFS
  you programmed; or
- which stage stopped, which is the useful answer when nothing decodes:

| What we see | What it means |
|---|---|
| no detection at all | no burst, or no usable short training field |
| `signal field could not be decoded, snr …` | detected, SIG failed its CRC |
| `decoder: frame not complete, canceling` | SIG declared N symbols, fewer arrived — your note 2, section 2 |
| frames in hex | it works |

## 4. Settled between us

- `zc`: 1000 frames per spacing **total**, 500 per channel. Taken.
- `ifs_us` stays in the ZigBee payload, 4 bytes LE at payload offset 0 of a
  13-byte PSDU. Thank you — and note the offset is **7** in what our decoder
  hands the parser, since it posts the PSDU without the length byte.
- `sz`: 500 ZigBee + 500 HaLow per spacing, HaLow leg on FSG like the others.
- Sweep: 6000 → 80 µs, 10 µs steps, 593 spacings, 1000 frames each, 500 ms
  pauses. Expect the `593 parts for 601 spacings` note in our analysis; it is
  normal, anything else is not.
- Transmit power unified at 20 dBm: good catch, and it does not invalidate
  anything on our side, since we changed no detection threshold.

## 5. Still open on our side, before the full series

- **The ZigBee parser for your 13-byte frame is not written yet.** It is not
  needed for the HaLow smoke test; it is needed before `zz`, `zc` and `sz`.
- **A decision for Adam on `ltf_taps`**: rebuilding a HaLow receiver recomputes
  its matched filter, ~0.3 ms of every swap that replaces a whole flowgraph.
  Caching it makes the series measure the runtime rather than one constructor.
  It must be decided before the series, not between runs.
- The warm/cold plugin-library runs come after the nine, and ask nothing new of
  you: same firmware, same sweep, one pair run three times.
