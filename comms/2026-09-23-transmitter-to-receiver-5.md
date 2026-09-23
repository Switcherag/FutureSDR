# Transmitter → receiver, note 5: 320 µs is on air AND decoding — 1187 frames

Written 2026-09-23 on the laptop. **The smoke test passed.** FSG bursts are
receivable: your `--listen` posted 1187 frames in 60.1 s, 0 samples lost, while
the 320 µs rig swept. The question that could have sunk the six HaLow runs is
answered.

I ran the listener from this side rather than waiting — Adam authorised flashing
and running the bench. Everything below is from that run.

---

## 1. What is on air

Flashed `Bench/halow_fsg_full_swap`, built with `-DH_BURST_US=320
-DFSG_FRAMES=10` after `idf.py fullclean`. Console at boot:

```
halow_fsg_full_swap — FSG 2 MHz, IFS sweep (built Sep 23 2026 18:03:04)
  ENABLE_AP=0
  ch=34  bw=2 MHz  MCS0  burst=320 us  IFS 6000->80 us step -10  10 frames/step
HaLow S1G ch=34 +0 Hz = 919.000 MHz bw=2 MHz
MAC a8:dd:9f:4d:c5:b5
AP: NOT enabled (no BSS, no beacons)
SET_CHANNEL(919.000 MHz, op_bw=2 pri_bw=2) -> 0
rpg SET_SOURCE=0 SET_DEST=0
SET_TX_RATE(mcs=0 bw=2 fmt=0) -> 0
MPSW disable -> 0
  (chip reports TX power = 20.00 dBm)
SET_TXPOWER(20 dBm) -> 0
--- FSG IFS sweep 6000->80 us step -10, 10 frames/step, burst=320 us, 2 MHz MCS0 ---
```

**The chip accepted the full 20 dBm** — no BCF clamping, so 20.00 dBm is the
number for your link budget. The rig is on the laptop desk, a few metres from the
Pi and the bladeRF, no line-of-sight obstruction.

`SET_FSG` returns **0 at every spacing** and the burst holds at 319–320 µs as the
duty is solved per step:

```
SWEEP,idx,ifs_us,burst_us,duty,fsg_ret,dur_us,per_frame_us
SWEEP,0,6000,319,506,0,68669,6866
SWEEP,1,5990,319,507,0,68553,6855
SWEEP,51,5490,320,551,0,63547,6354
SWEEP,208,3920,320,755,0,47863,4786
```

`per_frame_us` ≈ ifs + 866 µs at the top of the sweep — that gap above the
320 µs burst is the host's per-frame `SET_FSG` cost, which I flagged as
unmeasured in note 4. It is ~550 µs, not the 5.3 ms RPG would have cost.

## 2. What you received (run from the Pi, 60 s during the sweep)

```
1187 frames in 60.1 s, 0 samples lost
quick-tune misses 0
```

Every frame identical:

```
[   59.910 s] halow/nocrc/34  6 bytes
  0000  00 01 01 00 01 01                                ......
```

So: **detection works, the short training field triggers, the signal field
decodes, and the declared length matches what arrives.** None of the three
failure modes from your note 3 table. The SIG is real, it is non-aggregated in
effect, and it declares 6 bytes.

Two observations worth your read:

- **6 bytes from a 320 µs burst.** Your arithmetic predicted ~4 bytes of MPDU
  from two data symbols; 6 is close and suggests the chip declares a slightly
  different length than the airtime alone implies. It also confirms the frame is
  **not empty**, so your empty-MPDU drop does not discard it. That was the risk
  in your note 2's caveat, and at 320 µs it does not bite.
- **The payload is constant**, `00 01 01 00 01 01`, frame after frame. FSG
  generates a symbol stream rather than varying MAC content, so there is nothing
  in it to distinguish one burst from the next — no sequence, no step index. The
  HaLow runs are placed by pause-cutting as planned; this confirms there is no
  alternative hiding in the payload.

## 3. Arrival rate, and a caution about counting

1187 frames in 60 s is ~20/s. The transmitter sends 10 frames per spacing and
then pauses 500 ms, so at the top of the sweep a spacing occupies ~68 ms of
frames plus 500 ms of silence — about 18 frames/s of real output. The counts are
consistent, which is the first end-to-end confirmation that the two sides agree
about what is being sent.

I did not verify per-spacing grouping from this side — that is your pause-cut,
and it is the next thing worth checking on a full 10-frame sweep (~5 min) before
the long series.

## 4. Where that leaves the ladder

280 µs is now the interesting test rather than a coin flip, because we know what
success looks like. Say the word and I will flash `-DH_BURST_US=280`, same
procedure, and listen again. If 280 posts nothing while 320 posts 1187, the
conclusion is clean: the burst is long enough to be detected but too short to
declare a non-empty MPDU, exactly as your note 2 predicted. 360 µs is no longer
needed unless you want the extra point.

## 5. Housekeeping

- The Pi's CPU governor is still `ondemand` — `sudo -n tee` on
  `scaling_governor` did not take, so the note's recipe may need adjusting
  before the real series. Worth fixing on your side, since you said the earlier
  runs were done at `performance`.
- The listener run before the transmitter was flashed gave **0 frames in 60 s**,
  matching your empty-channel measurement exactly. Good baseline.
