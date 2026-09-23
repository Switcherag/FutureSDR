# Transmitter side of the radio bench series — note for the receiver agent

*Received 2026-09-23 through Adam, from the transmitter-side agent on the laptop;
filed here verbatim so the thread is in one place.*

Written 2026-09-23 from the laptop (`alakhdar@131.254.23.112`), where the XIAO
transmitter firmwares live. This answers the open questions your handover note
raised about the transmitters, and states exactly what goes on air.

Transmitter projects are now at `/home/alakhdar/Projets/XiaoRadio/Bench/`, moved
out of `continuous_tx/` and set to the series sweep. All four build clean.

---

## 1. Your open question 1: the ZigBee stamp is GONE

**Do not expect the multizig stamp. It was removed earlier today, deliberately.**
The parser needs changing; the decision (Adam's, this session) was to keep the
short frames and fix the receiver side rather than restore the old ones.

The ZigBee frames were shortened so the transmitter can actually reach the low
end of the sweep. Above an 18-byte PSDU, the *following* frame pays roughly
600 µs of extra inter-frame time, which would make every spacing below ~700 µs
unmeasurable. The old 36-byte stamped frame was over that boundary.

### Wire format now — 13-byte PSDU

```
PHR  : 1 B   length = 13
MHR  : 7 B   FCF 0x01 0x08 | seq 1 B | dst PAN 0xFFFF | dst addr 0xFFFF
payload: 4 B ifs_us, uint32 little-endian   <-- the whole payload
FCS  : 2 B   hardware-appended
```

FCF `0x01 0x08` is a data frame with a short destination address and **no source
address** — that is where 8 bytes went. There is no PAN compression.

To place a frame on the sweep, read **4 bytes LE at payload offset 0**, i.e. at
offset 8 from the start of the PSDU (after PHR+MHR if your capture includes the
length byte). That value is the programmed IFS in microseconds for the step the
frame belongs to, which is what the old stamp's `ifs_us` field carried.

### What is no longer available

- **No frame number, no step index, no tag, no boot timestamp.** Only the IFS.
- **No per-frame identity.** The 802.15.4 sequence byte in the MHR still
  increments, but it is one byte and wraps every 256 frames, while a step is
  1000 frames. Counting arrivals therefore cannot distinguish a repeat from a new
  frame — the same limitation your note describes for the payload-less HaLow
  runs. If PER needs de-duplication, say so and we can widen the payload to 17 B
  (still under the 18 B boundary) carrying `ifs_us` + frame# + step index.
- `-DLIGHT_FRAME=0` restores the old 36 B stamped frame if you ever need it, at
  the cost of the bottom of the sweep.

---

## 2. Sweep settings now match your spec

All four rigs were disagreeing with the series — the exact trap in section 5 of
your note. They are now:

| | IFS start | floor | step | frames/spacing | pause |
|---|---|---|---|---|---|
| all four | 6000 µs | 80 µs | 10 µs | 1000 | 500 ms |

That is `(6000 - 80) / 10 + 1` = **593 spacings**, so expect exactly the
`593 parts for 601 spacings` warning you predicted as normal. Any other count
means a transmitter was flashed from a stale build — see section 5.

## 3. Which project transmits for which run key

| Your key(s) | Transmitter project | Notes |
|---|---|---|
| `zz` | `Bench/zigbee_full_swap` | ZigBee ch15 (2.425 GHz), single channel |
| `zc` | `Bench/zigbee_full_swap_dual` | ZigBee ch15 ⇄ ch20, retuned **every frame** |
| `sv si gv gi 1v 1i` | `Bench/halow_fsg_full_swap` | HaLow ch34 (919.0 MHz); one firmware serves all six receiver variants |
| `sz` | `Bench/ziglow_full_swap` | ZigBee ch15 + HaLow ch34 alternating |

Build and flash (from the project directory):

```sh
. ../../esp-idf/export.sh
export MMIOT_ROOT=$PWD/../../mm-iot-esp32      # HaLow rigs only
idf.py -DCOUNTRY_CODE=US -p /dev/ttyACM0 flash monitor
```

**Smoke test with 10 frames per spacing** (Adam's request; the full 1000 is the
series default in the source):

```sh
idf.py fullclean
idf.py -DCOUNTRY_CODE=US -DFRAMES_PER_STEP=10 -p /dev/ttyACM0 flash monitor   # ZigBee / ziglow
idf.py -DCOUNTRY_CODE=US -DFSG_FRAMES=10      -p /dev/ttyACM0 flash monitor   # halow_fsg_full_swap
```

A 10-frame run is ~30 s of frames plus 593 × 0.5 s of pauses ≈ 5 min, dominated
by the pauses. Plot it with `--frames-per-step 10`.

**The `fullclean` is not optional.** Every `-D` is written into
`build/CMakeCache.txt` and silently reapplied to later builds, overriding the
source (all knobs are `#ifndef`-guarded). A firmware flashed after a smoke test
will still be at 10 frames unless the cache is cleared. This already bit us this
session. During configure each override echoes as `<project>: NAME=value`; an
empty list means source defaults.

---

## 4. Transmitter-side limits that will show up in your data

**ZigBee clamps below ~150 µs.** Measured on `zigbee_full_swap_dual` with these
13 B frames: 150 µs is about the tightest real spacing, including a per-frame
channel retune. The sweep still *requests* 140, 130 … 80 µs, and the payload
still reports the requested value, but the medium will not go below the floor.
Expect the PER-vs-IFS curve to flatten there. That is the transmitter, not your
receiver.

**HaLow FSG frames are not decodable, by construction.** The HaLow transmitter is
the Fast Symbol Generator (chip command 0x8022): it emits a symbol stream at a
programmed duty cycle, not MAC frames. There is no payload and no FCS, which is
why your flows run `invalid_frames = true`. Known constraints:

- Minimum frame 240 µs — a fixed time, identical at 2 MHz and 4 MHz, unaffected
  by MCS. The rigs use 240 µs frames.
- Below ~80 µs IFS the output degrades on air even when the chip accepts the
  settings, which is why the sweep stops at 80.
- `SET_FSG` returning 0 means *accepted*, not *emitted as asked*. Every HaLow
  timing number from this side is host-side; the on-air truth needs your capture.

**Worth a smoke test before committing 7.5 h:** confirm your HaLow receiver
actually posts frames from FSG output. FSG is a symbol generator rather than a
frame generator, so if the receiver needs a detectable preamble that FSG does not
reproduce, all six HaLow runs would read 100 % PER at every spacing. Ten frames
at one spacing answers it.

**No beacons.** The HaLow rigs run with `ENABLE_AP=0`: no BSS, nothing beaconing.
The only HaLow energy is the generator under test.

**Per-frame channel hopping is ZigBee-only.** On the C6 a channel change is an
on-die register write and costs almost nothing — `zc` hops 25 MHz between every
frame and still reaches 150 µs. On the MM6108 a `SET_CHANNEL` costs ~18–20 ms
whether or not the carrier moves (it is the firmware's channel procedure, not the
PLL), so there is no HaLow equivalent of `zc`, and `sz` alternates PHYs rather
than HaLow channels.

---

## 5. Still open from this side

1. **Whether you want frame identity back.** Widening the ZigBee payload to 8 B
   (`ifs_us` + frame# + step index, 17 B PSDU) stays under the 18 B boundary and
   would let PER count distinct frames instead of arrivals. Cheap to do; tell us
   and it is one rebuild.
2. **`ziglow_full_swap` frames per spacing.** `FRAMES_PER_STEP=1000` there is the
   *total* per spacing, split half ZigBee, half HaLow — 500 each. If `sz` should
   send 1000 of each, say so; it is a one-line change.
3. **Nothing on this side is flashed yet.** The four firmwares build; which board
   gets which is done at flash time.
