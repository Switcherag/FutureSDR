ptb-75h5c54.irisa.fr: 13th Gen Intel(R) Core(TM) i7-13850HX, 28 CPUs; --cpus auto --keep-awake; c423d959

| Swap | Swap time (median) | PER ≤ 1 % from | PER above 1 ms (mean) | Samples dropped |
|------|--------------------|----------------|-----------------------|-----------------|
| ZigBee → ZigBee | 0.038 ms | 0.00 ms | 0.00 % | 0 |
| HaLow simple → simple | 0.147 ms | 0.20 ms | 0.00 % | 0 |
| HaLow simple ⇄ ZigBee | 0.140 ms | 0.19 ms | 0.00 % | 0 |
| HaLow granular → granular | 0.187 ms | 0.30 ms | 0.00 % | 0 |
| HaLow granular, decoder only (inverse ⇄ Viterbi) | 0.049 ms | 0.00 ms | 0.00 % | 0 |
| HaLow single block → single | 0.157 ms | 0.24 ms | 0.00 % | 0 |

Samples dropped: by the replay (its output full for longer than a radio's
buffers last) and by the link to the receiver (full); what a radio would
have lost to a receiver that fell behind. If not 0, the machine could not
keep up during that run.
