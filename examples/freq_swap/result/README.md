# Retune matrix results

Every cell is how long `set_frequency` took to hop from the row's channel to the
column's channel. Both channel plans sit on both axes, so each plot has four
quadrants: `Z→Z` and `H→H` are in-band hops, `Z→H` and `H→Z` are the cross-band
hop a dual-PHY receiver performs on every swap.

Regenerate any of these with `python3 ../plot_retune_matrix.py <csv...> --out <png>`.

## The headline

The bladeRF 2.0 micro defaults to **host-side tuning**. Moving the tuning
algorithm to the FPGA cuts the cross-band retune 4.2x and makes it immune to
contention with a running RX stream. It costs one environment variable — no
driver change, no code change:

    BLADERF_DEFAULT_TUNING_MODE=fpga

| capture | in-band | cross-band |
| --- | --- | --- |
| SoapySDR, host tuning (default) | 10.1 ms | **111.7 ms** |
| SoapySDR, `BLADERF_DEFAULT_TUNING_MODE=fpga` | 5.9 ms | **26.8 ms** |
| libbladeRF direct, host tuning, idle | 9.7 ms | 110.7 ms |
| libbladeRF direct, host tuning, streaming | 10.4 ms | 120.4 ms |
| libbladeRF direct, FPGA tuning, idle | 5.3 ms | 26.2 ms |
| libbladeRF direct, FPGA tuning, streaming | 5.3 ms | 26.2 ms |

Two conclusions:

* **SoapySDR is not the overhead.** 111.7 ms through Soapy vs 110.7 ms straight
  to libbladeRF, same tuning mode. The abstraction layer is free.
* **Host tuning contends with streaming, FPGA tuning does not.** Host mode loses
  10 ms when a reader runs concurrently; FPGA mode is unchanged.

## Files

| file | capture |
| --- | --- |
| `compare_soapy_host_vs_fpga.png` | the headline — same binary, same driver, one env var. Full 42x42. |
| `compare_soapy_vs_libbladerf.png` | Soapy vs direct libbladeRF at equal tuning mode: they agree. |
| `compare_libbladerf_all.png` | all four libbladeRF captures on one colour scale. |
| `soapy_host.png` | SoapySDR, device default. Full 42x42. |
| `soapy_fpga.png` | SoapySDR with the env var. Full 42x42. |
| `libbladerf_host_idle.png` | direct, host tuning, no stream. 11x11 preview (`--stride 4`). |
| `libbladerf_host_streaming.png` | direct, host tuning, concurrent reader. |
| `libbladerf_fpga_idle.png` | direct, FPGA tuning, no stream. |
| `libbladerf_fpga_streaming.png` | direct, FPGA tuning, concurrent reader. |

The `soapy_*` captures come from `retune_matrix` (seify/SoapySDR); the
`libbladerf_*` ones from `retune_matrix_brf`, which talks to the C library
directly and is the only one that can select the tuning mode from code.
Source CSVs are one directory up: `rm_idle.csv`, `soapy_fpga.csv`,
`brf_{idle,stream}_{default,fpga}.csv`.

## Reading these honestly

What is timed is **how long the call takes to return**, which is not how long
the radio is off the air — the RF settles well before the call returns. A call
that blocks the streaming thread costs air time; one that merely returns slowly
on its own thread may cost none. That is why the streaming captures matter more
than the idle ones, and why `retune_timing.rs` (which finds the frequency step
in captured IQ) is the tool for true settle time.

The streaming runs reported zero read failures in both modes. That is not proof
no samples were dropped: without the `SC16_Q11_META` format libbladeRF cannot
report overruns, and these bindings have the meta formats disabled.

## Quick tune — measured, not predicted

`quick_tune_probe` captures one profile per band with `bladerf_get_quick_tune`,
then recalls them with `bladerf_schedule_retune(RETUNE_NOW, freq, &profile)`.
200 alternating cross-band hops per method, with a reader streaming throughout:

| method | Host tuning | FPGA tuning |
| --- | --- | --- |
| `set_frequency` | 120.0 ms | 26.2 ms |
| `schedule_retune`, null profile | rejected (`-3`) | rejected (`-3`) |
| **quick tune** | **0.274 ms** | **0.273 ms** |

**96x faster than FPGA `set_frequency`, 440x faster than the host default.**
Quick tune is identical in both tuning modes, which makes sense: recalling a
stored profile bypasses the tuning algorithm entirely, so where that algorithm
would have run stops mattering.

The profiles come back distinct per band — `nios_profile` 0/1 and
`rffe_profile` 0/1 — and the null-profile call fails exactly as the libbladeRF
header warns for this board (`bladerf2_schedule_retune: quick_tune invalid: is
null`).

### Proof it moves the radio

0.274 ms is about one USB round trip, which is also what a no-op costs, so the
return code is not evidence. `bladerf_get_frequency` cannot settle it either:
in FPGA mode it *errors* after a recall (`_rfic_fpga_get_frequency ... An FPGA
operation reported a failure`), and in host mode it returns libbladeRF's cached
value, which `schedule_retune` does not update.

So `--verify-power` parks the radio on the *opposite* band, recalls the
profile, and measures received power — a recall that did nothing would report
the wrong band:

| | measured |
| --- | --- |
| band separation via `set_frequency` | 6.7 dB |
| quick tune Z vs `set_frequency` Z | 0.02 dB |
| quick tune H vs `set_frequency` H | 0.44 dB |

Each recall lands on its own band's power. The radio moves.

### Correction: metadata is not required

An earlier reading of the libbladeRF header took `@pre bladerf_sync_config()
must have been called with BLADERF_FORMAT_SC16_Q11_META` to mean quick tune
needs metadata streaming. **It does not.** All 200 recalls succeeded against a
plain `SC16_Q11` stream. That precondition governs *timestamped* retunes —
scheduling a hop at a future sample count — not `RETUNE_NOW`.

That removes the larger half of the work. What remains in
`../bladerfbindings/seify-bladerf` is one fix: `QuickTune` hardcodes the
bladeRF**1** arm of a C union, while this board needs the bladeRF2 arm
`{ nios_profile, rffe_profile, port, spdt }`. `quick_tune_probe` works around
it by using the raw bindgen union directly, and is the reference for the fix.

Metadata streaming only becomes interesting later, if a retune should land at
an exact sample rather than as soon as the USB packet arrives.
