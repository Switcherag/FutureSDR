# Demo Bundle

Run the packaged release build from this folder with:

```bash
./real_device_swap
```

Live packet monitor:

```bash
python live_packets.py
```

What is bundled here:
- `real_device_swap.bin`: the frozen release binary
- `lib*.so`: the release plugins and local Rust/FutureSDR shared libraries it needs
- `flows/`: the copied TOML files used by the rotation policy
- `live_packets.py`: a live matplotlib view of Zigbee and HaLow packet rate and decoded payload fields

Notes:
- This avoids rebuilding and avoids depending on whatever is currently in `target/release`.
- It still depends on system SDR libraries and drivers already installed on this machine, such as SoapySDR and the radio backend.
- If the source code changes and you want a newer demo, rebuild once and refresh this folder.
