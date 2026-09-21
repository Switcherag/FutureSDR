# Running the swap tests on a Raspberry Pi 5

How to build and run `real_device_swap` on a Raspberry Pi 5 with a
bladeRF 2.0 micro: the replay tests first (no radio), then the radio.
Written for Raspberry Pi OS (Bookworm, 64-bit); commands run on the Pi
unless noted.

## 1. Hardware

- Raspberry Pi 5, 8 GB preferred (4 GB works, with the swap file of step 3).
- The **active cooler**. The tests keep the CPU busy for minutes; without
  it the Pi throttles, and the timings with it.
- The official 27 W (5 V, 5 A) supply. The bladeRF draws its power from the
  USB port; with a weaker supply the Pi limits USB current and the bladeRF
  can drop out under load.
- The bladeRF on a **blue USB 3 port**. At 20 MSps it streams 80 MB/s,
  which USB 2 cannot carry.

## 2. System packages

```sh
sudo apt update
sudo apt install -y build-essential cmake git pkg-config curl \
    libusb-1.0-0-dev libncurses-dev libedit-dev \
    clang libclang-dev \
    python3-matplotlib
```

`clang`/`libclang-dev` are for the bladeRF Rust bindings (bindgen
generates them from libbladeRF's header at build time); `python3-matplotlib`
is for the figures.

## 3. More swap (4 GB Pi only)

The release build of FutureSDR's shared library uses about 3 GB at its peak:

```sh
sudo dphys-swapfile swapoff
sudo sed -i 's/^CONF_SWAPSIZE=.*/CONF_SWAPSIZE=4096/' /etc/dphys-swapfile
sudo dphys-swapfile setup && sudo dphys-swapfile swapon
```

## 4. libbladeRF

Build it from Nuand's sources, as on the development laptop (libbladeRF
2.5.1): the example uses bladeRF 2 *quick tune*, which a distribution's
package may be too old for.

```sh
git clone https://github.com/Nuand/bladeRF.git ~/bladeRF
cd ~/bladeRF/host
mkdir -p build && cd build
cmake -DCMAKE_BUILD_TYPE=Release -DINSTALL_UDEV_RULES=ON \
      -DBLADERF_GROUP=plugdev ..
make -j4
sudo make install
sudo ldconfig
sudo usermod -aG plugdev "$USER"     # then log out and in again
```

The udev rules let your user open the bladeRF without `sudo`.

Check it (the bladeRF plugged in):

```sh
pkg-config --modversion libbladeRF   # 2.5.x
bladeRF-cli -p                       # lists the device
bladeRF-cli -e info -e version       # FPGA and firmware versions
```

**FPGA image.** A bladeRF 2.0 micro needs its FPGA loaded at each power-up.
Put the image for your model where libbladeRF loads it by itself (xA4
shown; use `hostedxA9-latest.rbf` for an xA9):

```sh
sudo mkdir -p /usr/local/share/Nuand/bladeRF
sudo curl -L -o /usr/local/share/Nuand/bladeRF/hostedxA4.rbf \
    https://www.nuand.com/fpga/hostedxA4-latest.rbf
```

If `bladeRF-cli -e version` reports an old firmware, update it
(`bladeRF-cli -f bladeRF_fw_latest.img`, from
<https://www.nuand.com/fx3/>) and power-cycle the bladeRF.

If the build of the example later cannot find libbladeRF, tell pkg-config
where it is: `export PKG_CONFIG_PATH=/usr/local/lib/pkgconfig`.

## 5. Rust

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"
```

The repository pins its toolchain (`rust-toolchain.toml`: nightly, with
rustfmt and clippy); rustup installs it on the first `cargo` command in the
tree.

## 6. The code

The work is on the `dynv4` branch, which is not on any remote. Copy it from
the laptop as a git bundle (on the laptop):

```sh
cd ~/"Projet 2/dynv4"
git bundle create /tmp/dynv4.bundle dynv4
scp /tmp/dynv4.bundle pi@<pi-address>:~
```

and on the Pi:

```sh
git clone -b dynv4 ~/dynv4.bundle ~/dynv4
```

(To update it later: a new bundle, then `git pull ~/dynv4.bundle dynv4`.)

## 7. Build

```sh
cd ~/dynv4/examples/real_device_swap
cargo build --release
```

The first build takes a long while on the Pi (FutureSDR's shared library
is built with one codegen unit); later builds only rebuild what changed.
On the first run the program also builds the three receiver plugins
(basic, wlan, zigbee) against that library, into `target/plugins`.

Always start it with `cargo run --release -- ...`: cargo sets the path to
the shared library the program and its plugins load. Run it from
`examples/real_device_swap`.

## 8. CPU setup

The Pi 5 has four identical Cortex-A76 cores, no hyper-threading. Keep one
for the system and the USB interrupts; give three to the runtime and let
the source (the replay, or the radio's reader) share the fourth, or give
the runtime all four:

- `--cpus 1,2,3`: runtime threads pinned to CPUs 1–3, the source on CPU 0.
- `--cpus auto --workers 3`: the same, chosen by the program.

Set the governor to `performance` for the tests (until the next reboot):

```sh
echo performance | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor
```

The program prints the governor and frequency limit of the CPUs it runs
on. Watch the temperature and throttling during a run:

```sh
watch -n 2 'vcgencmd measure_temp; vcgencmd get_throttled'
```

`get_throttled` must stay `0x0`; anything else means the Pi slowed down
(heat or supply) and the run's timings are not meaningful.

Optional:

- `--keep-awake` keeps the CPUs of `--cpus` busy at the lowest priority, so
  that they do not slow their clocks between samples. On the laptop it
  removed 1–2 ms wake-up delays; on the Pi it matters less (its idle states
  are shallow), costs power and heat, and needs the active cooler.
- `--rt-priority 50` gives the runtime and source threads real-time
  priority. It needs the right to: add a line `<user> - rtprio 90` to
  `/etc/security/limits.d/rtprio.conf`, then log in again (`ulimit -r`
  shows 90).
- Reserving the three CPUs for the tests: add `isolcpus=1,2,3` to the single
  line of `/boot/firmware/cmdline.txt` and reboot; then run with
  `--cpus 1,2,3`.

## 9. Tests without the radio

Each run writes a CSV (`--csv`), a row per IFS, and prints a table. The
Pi has fewer and slower cores than the laptop, so expect the swaps to take
longer and the edges of the PER curves to move to larger IFS.

Replacing the receiver after every frame, HaLow ⇄ ZigBee, IFS swept from
1 ms to 0, no retuning (the software swap only):

```sh
cargo run --release -- --source replay --cpus 1,2,3 --retune-us 0 \
    --swap zigbee.toml,wlan_simple.toml --frames-per-step 400 \
    --csv rpi_zh_simple.csv
```

The four swaps of `figures/software_ifs.png`, every 0.02 ms from 0 to
0.5 ms:

```sh
IFS=$(python3 -c "print(','.join([f'{x/100:.2f}' for x in range(0,52,2)]+['0.6','0.8','1']))")
for pair in zz:zigbee.toml,zigbee.toml \
            hh_simple:wlan_simple.toml,wlan_simple.toml \
            hh_granular:wlan_granular.toml,wlan_granular.toml \
            zh_simple:zigbee.toml,wlan_simple.toml; do
    cargo run --release -- --source replay --cpus 1,2,3 --retune-us 0 \
        --swap "${pair#*:}" --ifs "$IFS" --frames-per-step 400 \
        --csv "rpi_${pair%%:*}.csv"
done
```

Replacing only the decoder (`figures/block_swap.png`):

```sh
cargo run --release -- --source replay --cpus 1,2,3 --retune-us 0 \
    --swap wlan_granular_viterbi.toml,wlan_granular_hard.toml \
    --ifs "$IFS" --frames-per-step 400 --csv rpi_hh_granular_decoder.csv
```

With a retune time, as quick tune would take (300 µs):

```sh
cargo run --release -- --source replay --cpus 1,2,3 --retune-us 300 \
    --swap wlan_simple.toml,zigbee.toml --csv rpi_retune.csv
```

`REPLAY_LOSSES=1` before `cargo run` prints, for each lost frame, why: when
the frame before it was posted, how many samples waited, and whether the
replay delivered its samples late (on a loaded Pi, the first thing to
check).

To draw the figures from the Pi's runs, copy the CSVs over those in
`figures/` (named as there: `zz.csv`, `hh_simple.csv`, ...) and run
`python3 figures/plot_software_ifs.py` or `figures/plot_block_swap.py`;
or copy them to the laptop.

## 10. Tests with the bladeRF

First the quick-tune profiles: the program opens the bladeRF, tunes it once
to each receiver's channel (919 MHz, 2.425 GHz) and keeps the RFIC's state
for each:

```sh
cargo run --release -- --source bladerf --register-only
```

Then the receiver, replaced after every frame, for 60 s:

```sh
cargo run --release -- --source bladerf --cpus 1,2,3 --duration 60 \
    --csv rpi_bladerf.csv
```

It writes a row per frame: PHY, length, time, swap and retune times, and
for the dyn branch's multizig ZigBee frames the stamp (step, programmed IFS,
transmitter clock); for HaLow frames the sequence number. The summary at
the end gives the median swap and retune times and the quick-tune misses
(should be 0).

Useful options:

- `--first halow` starts on HaLow (ZigBee by default).
- `--no-quick-tune` retunes with `set_frequency`, to compare.
- `--gain-db N` (10 by default); the receivers' `[radio]` sections can ask
  for a gain too.
- `--sample-rate 20e6 --decim 5` by default: 20 MSps from the bladeRF,
  filtered and decimated to the receivers' 4 MSps. If the reader cannot
  keep up on the Pi (frames lost however far apart they are, one CPU at
  100 % in `top`), take 4 MSps directly: `--sample-rate 4e6 --decim 1`.
- `--drop-after-retune-us N` drops the samples that follow a retune, for
  those still in USB transfers from the previous channel.
- `--halow wlan_granular.toml` uses the block-by-block HaLow receiver.

## 11. When something goes wrong

| Symptom | Check |
|---------|-------|
| `opening the bladeRF: No devices available` | `bladeRF-cli -p`; the udev rules and the `plugdev` group (log in again); a USB 3 port; the supply |
| `unable to find library -lbladeRF` at build time | `sudo ldconfig`; `export PKG_CONFIG_PATH=/usr/local/lib/pkgconfig` |
| `libclang` not found at build time | `sudo apt install clang libclang-dev` |
| The build is killed | memory: more swap (step 3), or `cargo build --release -j2` |
| `error while loading shared libraries: libfuturesdr_plugin_rt.so` | start it with `cargo run --release -- ...`, not the binary |
| `asked for 20000000 S/s, the bladeRF gave ...` | the FPGA image is missing or old (step 4) |
| PER at every IFS, even large ones | `vcgencmd get_throttled` (heat, supply); `REPLAY_LOSSES=1`; fewer workers or `--keep-awake` |
| `real-time priority ...: Operation not permitted` | the rtprio limit (step 8) and a new login |
