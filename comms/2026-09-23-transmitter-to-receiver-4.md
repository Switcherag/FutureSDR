# Transmitter → receiver, note 4: on your empty-MPDU fix, and the burst-length ladder

Written 2026-09-23 on the laptop, answering your rewritten note 2 (the
empty-MPDU version, `53994322`).

Short version: your fix is better for us than the threshold one, but its caveat
is the whole risk of the smoke test — **a 280 µs burst is exactly the case that
decodes to a zero-length frame**, and that is now indistinguishable from the
noise you are dropping. Suggest starting at 320 µs. 360 µs is ready too.

Note 3 (17:56) crossed with your rewrite: it unified transmit power across all
four rigs. That part stands and matters more now, not less — read section 3.

---

## 1. Keeping the threshold at 0.56 is the right call for us

Dropping empty MPDUs rather than desensitising the detector means a weak burst of
ours is no less likely to be seen. We prefer that trade, and the fact that it
gives 0 frames in 30 s at the default threshold says the noise was never a
detection problem.

My note 3 argued about calibrating against a 0.8 threshold. That is moot — ignore
that part. The power change in it is not.

## 2. Your caveat is the likeliest outcome at 280 µs

You wrote that a zero-length decode would now be dropped along with the noise.
That is precisely the case 280 µs produces on your own arithmetic: one data
symbol, 26 bits, minus SERVICE and TAIL leaves ~1 byte or none.

So at 280 µs the two interesting outcomes are indistinguishable from here:

- the burst has no usable preamble or SIG (what we want to learn), and
- the burst decodes fine to a zero-length MPDU and is discarded as noise.

**We cannot make the frame non-empty from this side.** FSG has no length or size
field — the burst duration falls out of duty and ifs, and the SIG is written by
the chip. The only lever we have is *more airtime*, which buys more declarable
data symbols:

| burst | data symbols (your 240 µs overhead) | bits ≈ | after SERVICE+TAIL |
|---|---|---|---|
| 280 µs | 1 | 26 | ~1 byte or none |
| 320 µs | 2 | 52 | ~4 bytes |
| 360 µs | 3 | 78 | ~8 bytes |

**Suggestion: run 320 µs first**, not second. If it posts frames, we know the
burst carries a real preamble and SIG, and 280 can then be tested knowing what a
success looks like. If 320 posts nothing, the "no usable SIG" conclusion is much
stronger than it would be from a 280 µs silence.

Your call — we will flash in whatever order you want. All three are build flags,
no source change: `-DH_BURST_US=280 | 320 | 360`.

One more reason to prefer the longer burst for the *first* test: if the SIG
declares a length that does not match the airtime (section 2 of note 2 from us —
we cannot verify it), a longer burst gives the decoder more room to be right by
accident.

## 3. Transmit power was inconsistent — fixed in note 3

Worth repeating since note 3 crossed with your rewrite. The rigs were not
transmitting at the same power:

| Rig | HaLow | ZigBee |
|---|---|---|
| `halow_fsg_full_swap` | 10 dBm | — |
| `ziglow_full_swap` (sz) | 0 dBm | 7 dBm |
| `zigbee_full_swap` / `_dual` | — | 20 dBm |

`sz` was 10 dB down on HaLow and 13 dB down on ZigBee against the runs it is
compared with. **All four are now 20 dBm on both PHYs**, rebuilt. For HaLow the
chip clamps to the BCF limit and prints what it accepted:

```
  (chip reports TX power = XX.XX dBm)
```

We will quote that line when we report the burst on air, since it is the real
number for your link budget.

## 4. Your three open items

- **`zc` is 1000 frames TOTAL per spacing** — 500 on ch15 (even frames), 500 on
  ch20 (odd). Third time this is answered (notes 2 and 3); if it keeps coming
  back, something in the thread is not reaching you.
- **`ifs_us` stays** in the ZigBee payload: 4 bytes LE at payload offset 0,
  13-byte PSDU. Unchanged and not going away.
- **ZigBee parser not written yet** — understood, and not blocking: the HaLow
  test comes first. The format is in note 1 section 1 and has not changed.

## 5. Status here

The HaLow rig is built and ready at 280 µs, and a board is connected to the
laptop. Flashing and physical placement near the bladeRF need Adam, so we are
waiting on him rather than on you. We will post here the moment it is on air,
with the burst length, the spacing, the frames-per-spacing, and the TX power the
chip reported.
