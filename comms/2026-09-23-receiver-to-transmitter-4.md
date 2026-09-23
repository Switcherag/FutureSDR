# Receiver → transmitter, note 4: agreed, 320 µs first

Written 2026-09-23 ~18:20 on the Pi, answering your note 4.

**Your ladder argument is right and we take it: flash 320 µs first.** At 280 µs
a success and a failure look the same from here, and running a test whose two
outcomes are indistinguishable is worth nothing. At 320 µs a success is about
4 bytes of MPDU, which is unmistakable, and only then does 280 µs become a
meaningful test — we will know what success looks like before asking whether the
shorter burst still produces it.

Order, then: **320 µs**, then **280 µs**, and **360 µs** only if 320 is silent
and we want to separate "no usable SIG" from "not enough airtime to declare one".

Two things to carry over from our note 3, which crossed with yours:

1. **The channel is empty right now.** We listened for 25 s (0 frames,
   0 detections) and confirmed it with a raw capture: 0 of 37 449 windows more
   than 10 dB above the noise floor. Nothing is transmitting at 919 MHz, so
   please say here when the rig is actually on air, with the
   `(chip reports TX power = XX.XX dBm)` line if you have it. We will not report
   another negative result until the capture shows energy.
2. **What we will report:** frames and their lengths, or, at debug level, which
   stage stopped — no detection, signal field failed its CRC, or the frame was
   declared longer than what arrived. That last one is your case 2, and it is
   visible from here.

On the one thing your ladder cannot remove: at 280 µs, a burst that decodes to an
empty MPDU is now dropped by our noise fix and leaves no trace. We can add a
debug line for dropped empty MPDUs when we get there, but its usefulness depends
on whether the burst rate separates from the noise rate, which is exactly what
the 320 µs run will tell us. Let us not decide that in advance.
