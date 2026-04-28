# SDR Cross-Flowgraph Example

Hot-swap SDR receiver chains at runtime without restarting the SDR source.

Replicates the `examples/dyn_phy-swap` chains (WLAN 802.11 and Zigbee 802.15.4)
as TOML-defined swappable flowgraphs.

## Architecture

```
FG 0 (permanent):  SeifySource ──> [auto bridge c32] ──> FG 1

FG 1 (swappable):  [auto bridge c32] ──> Zigbee RX / WLAN RX / Discard
```

## Available flows

| Flow | File | Description |
|------|------|-------------|
| Zigbee RX | `flows/zigbee_rx.toml` | Demod → ClockRecovery → Decoder → Mac → BlobToUdp |
| WLAN RX | `flows/wlan_rx.toml` | DC Offset → SyncShort/Long → FFT → FrameEQ → Decoder → BlobToUdp |
| Discard | `flows/discard.toml` | NullSink — drops all samples |

## Build

```sh
cargo build \
  -p sdr-cross-flowgraph-example \
  -p seify_source_plugin \
  -p null_sink_plugin \
  -p zigbee_demod_plugin \
  -p clock_recovery_mm_plugin \
  -p zigbee_decoder_plugin \
  -p zigbee_mac_plugin \
  -p blob_to_udp_plugin \
  -p wlan_dc_offset_plugin \
  -p delay_complex_plugin \
  -p complex_to_mag2_plugin \
  -p moving_average_f32_plugin \
  -p moving_average_complex_plugin \
  -p mult_conj_plugin \
  -p divide_mag_plugin \
  -p wlan_sync_short_plugin \
  -p wlan_sync_long_plugin \
  -p fft_complex_plugin \
  -p wlan_frame_equalizer_plugin \
  -p wlan_decoder_plugin
```

## Run

```sh
cd examples/sdr_cross_flowgraph
../../target/debug/sdr_cross_fg
```

Edit `flows/fg0_sdr.toml` for your SDR device and target frequency/rate.
Default: Zigbee channel 11 (2.405 GHz, 4 Msps, gain 40 dB).
For WLAN: set frequency to 5 GHz band and sample rate to 20 Msps.

## UDP control (port 7879)

```sh
echo -n '-s flows/zigbee_rx.toml' | nc -u 127.0.0.1 7879
echo -n '-s flows/wlan_rx.toml'   | nc -u 127.0.0.1 7879
echo -n '-s flows/discard.toml'   | nc -u 127.0.0.1 7879
echo -n Q                         | nc -u 127.0.0.1 7879
```

Decoded frames are sent as UDP blobs:
- Zigbee: rftap on `127.0.0.1:55555`
- WLAN: rx_frames on `127.0.0.1:55555`, rftap on `127.0.0.1:55556`
