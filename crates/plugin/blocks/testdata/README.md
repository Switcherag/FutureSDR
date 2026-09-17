# Recorded frames

Single frames received over the air, cut from one-second recordings of the
`dyn` branch (`examples/real_device_swap/recording`). Each `.cf32` file is
little-endian `f32` pairs (I, Q); the `.meta.json` next to it gives the
sample rate, the center frequency and where the cut was taken.

| File | Standard | Rate | Samples |
|------|----------|------|---------|
| `halow_frame.cf32` | 802.11ah, 2 MHz, 919 MHz | 4 MSps | 2920 |
| `zigbee_frame.cf32` | 802.15.4, channel 11 | 4 MSps | 5640 |

## Expected frames

`expected/` holds, one hex line per frame, what the receivers the plugins
come from decoded:

| File | Receiver | Input |
|------|----------|-------|
| `bpsk-1-2-15db.wlan.txt` | `examples/wlan` (802.11a) | `examples/wlan/data/bpsk-1-2-15db.cf32` |
| `bpsk-3-4-30db.wlan.txt` | `examples/wlan` (802.11a) | `examples/wlan/data/bpsk-3-4-30db.cf32` |
| `halow_frame.v6.txt` | `dyn`, HaLow v6 (`rx_csv --rx v6`) | `halow_frame.cf32` with 16000 samples of the recording's noise on each side |
| `halow_raw.v6.txt` | `dyn`, HaLow v6 | the whole one-second recording, `halow_raw.cf32` (32 MB, not copied here) |

The wlan plugin's tests compare its frames with these; the last file only
when `WLAN_HALOW_RECORDING` names the recording.
