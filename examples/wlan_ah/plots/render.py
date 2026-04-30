#!/usr/bin/env python3
"""Render the STF correlation and STF detection-metric plots from the
FileSink dumps produced by `rx_debug`.

Inputs (in this directory):
    stf_corr.cf32    Complex<f32>  — running 288-sample correlation sum
                                     (rust analogue of np.convolve output)
    stf_metric.f32   f32           — streaming detection metric
                                     M[n] = |Σ_{288} corr| / Σ_{80} |x|²

Outputs:
    stf_corr.png
    stf_metric.png

The detection-metric processing mirrors the analysis notebook exactly:
    detect_window     = 6 * Ts          (=  960)
    local_maxima_win  = ±2 * Ts         (= ±320)
    threshold_dB      = 30 (on max_value_norm, which is mean-based)

Notes on units:
    rust metric (sum-of-power)  ≈  notebook metric (mean-of-power)  / N_pow
    with N_pow = Ts/2 = 80 → 10*log10(80) ≈ 19.03 dB.
    We add 19.03 dB to convert rust units → notebook-equivalent dB.
"""

from __future__ import annotations

import argparse
import os
import sys

import matplotlib.pyplot as plt
import numpy as np


# ── Constants matching rx_debug.rs ─────────────────────────────────────
FS_DEFAULT = 4e6
TU = 128
TCP = 32
TS = TU + TCP                     # 160
CORR_WIN = 2 * TS - TU // 4       # 288
POWER_WIN = TS // 2               # 80   (mean-vs-sum offset = 10*log10(80))
DETECT_WIN = 6 * TS               # 960
LOCAL_MAX_R = 2 * TS              # 320
THRESHOLD_DB = 30.0
DB_OFFSET_NB = 10.0 * np.log10(POWER_WIN)  # ≈ 19.03


def load(path: str, dtype) -> np.ndarray:
    if not os.path.exists(path):
        sys.exit(f"missing input: {path}")
    return np.fromfile(path, dtype=dtype)


def plot_stf_corr_zoom(here: str, fs: float, sel_start: int, sel_len: int) -> None:
    """Mirrors notebook cell `sel = slice(3310000, 3316000)`."""
    path = os.path.join(here, "stf_corr.cf32")
    out = os.path.join(here, "stf_corr_zoom.png")
    z = load(path, "complex64")
    n_total = z.size
    sel_start = max(0, min(sel_start, n_total - 1))
    sel_end = min(n_total, sel_start + sel_len)
    z_sel = z[sel_start:sel_end]

    # x-axis starts at 0 µs (matches the notebook: np.arange(z_sel.size)/fs*1e6)
    tax = np.arange(z_sel.size) / fs * 1e6

    fig, ax = plt.subplots(figsize=(7, 3.5))
    ax.plot(tax, z_sel.real, label="Real part")
    ax.plot(tax, z_sel.imag, label="Imaginary part")
    ax.set_title("STF correlation metric (Schmidl & Cox)")
    ax.set_xlabel("Relative time (us)")
    ax.set_ylabel("Amplitude")
    ax.legend()
    fig.tight_layout()
    fig.savefig(out, dpi=120)
    print(f"wrote {out}  (slice [{sel_start}:{sel_end}] of {n_total})")


def plot_stf_corr_full(here: str, fs: float) -> None:
    """Mirrors notebook cell '#plotting full recording'."""
    path = os.path.join(here, "stf_corr.cf32")
    out = os.path.join(here, "stf_corr_full.png")
    z = load(path, "complex64")
    tax = np.arange(z.size) / fs  # seconds

    fig, ax = plt.subplots(figsize=(12, 3.5))
    ax.plot(tax, z.real, label="Real part", linewidth=0.5)
    ax.plot(tax, z.imag, label="Imaginary part", linewidth=0.5)
    ax.set_title("STF correlation metric (Schmidl & Cox)")
    ax.set_xlabel("Relative time (s)")
    ax.set_ylabel("Amplitude")
    fig.tight_layout()
    fig.savefig(out, dpi=120)
    print(f"wrote {out}  ({z.size} samples)")


def plot_stf_metric(here: str, max_blocks: int) -> None:
    path = os.path.join(here, "stf_metric.f32")
    out = os.path.join(here, "stf_metric.png")
    m = load(path, "float32")  # streaming M[n] in rust (sum-based) units

    n = m.size
    # ── Block argmax + local-max test (mirrors the notebook) ──────────
    # The metric stream has been padded with (CORR_WIN-1)+(POWER_WIN-1)
    # zeros at start by the moving-average warmup; that's fine — those
    # samples will be zero and won't trigger detection.
    n_blocks = n // DETECT_WIN
    if n_blocks == 0:
        sys.exit("not enough samples in stf_metric.f32 to form a block")

    # reshape into (n_blocks, DETECT_WIN); leftover tail is dropped
    blocks = m[: n_blocks * DETECT_WIN].reshape(n_blocks, DETECT_WIN)
    block_argmax = np.argmax(blocks, axis=1)
    block_max = blocks[np.arange(n_blocks), block_argmax]

    # local-max check over ±LOCAL_MAX_R around the argmax
    abs_argmax = block_argmax + np.arange(n_blocks) * DETECT_WIN
    is_local_max = np.zeros(n_blocks, bool)
    for j in range(n_blocks):
        a = abs_argmax[j]
        lo = max(0, a - LOCAL_MAX_R)
        hi = min(n, a + LOCAL_MAX_R)
        is_local_max[j] = block_max[j] >= np.max(m[lo:hi])

    # convert rust-units to notebook-equivalent dB
    with np.errstate(divide="ignore"):
        db_rust = 10.0 * np.log10(block_max)
    db_nb = db_rust + DB_OFFSET_NB
    db_nb = np.where(is_local_max, db_nb, np.nan)  # mask non-local-max

    n_show = n_blocks if max_blocks <= 0 else min(n_blocks, max_blocks)
    detected = np.sum(np.where(np.isfinite(db_nb), db_nb, -np.inf) >= THRESHOLD_DB)

    fig, ax = plt.subplots(figsize=(12, 4))
    ax.plot(np.arange(n_show), db_nb[:n_show], ".", markersize=3)
    ax.axhline(THRESHOLD_DB, color="grey", linewidth=1, linestyle="--",
               label=f"{THRESHOLD_DB:.0f} dB threshold")
    ax.set_title("STF detection metric")
    ax.set_xlabel("Detection window number")
    ax.set_ylabel("Metric (dB, notebook-equivalent)")
    ax.legend()
    ax.grid(True, alpha=0.3)
    fig.tight_layout()
    fig.savefig(out, dpi=120)
    print(f"wrote {out}  ({n_blocks} blocks, {detected} above {THRESHOLD_DB:.0f} dB; first {n_show} shown)")


# ── First-PPDU plots (the contents of decode_ppdu's `plots=True` path) ─

# Active subcarrier offsets from DC (matches lib.rs / notebook): -28..-1, +1..+28.
ACTIVE_OFFSETS = np.array([k for k in range(-28, 29) if k != 0])
PILOT_OFFSETS = np.array([-21, -7, 7, 21])
DATA_OFFSETS = np.array([k for k in ACTIVE_OFFSETS if k not in PILOT_OFFSETS])
SIG_DATA_OFFSETS = np.array([k for k in range(-26, 27) if k != 0 and k not in PILOT_OFFSETS])


def _const_plot(ax, z, title, lim=1.8, color="C0", ref=None):
    ax.plot(z.real, z.imag, ".", color=color, markersize=3)
    if ref is not None:
        ax.plot(ref.real, ref.imag, ".", color="red", markersize=3)
    ax.set_aspect("equal")
    ax.set_xlim(-lim, lim)
    ax.set_ylim(-lim, lim)
    ax.set_xticks([])
    ax.set_yticks([])
    ax.text(0, 1.45, title, ha="center")


def plot_first_ppdu_channel(here: str, fs: float) -> None:
    path = os.path.join(here, "h_est.cf32")
    out = os.path.join(here, "first_ppdu_channel.png")
    if not os.path.exists(path):
        print(f"skip {out}: missing {path} (no PPDU was captured)")
        return
    h = np.fromfile(path, dtype="complex64")
    if h.size != ACTIVE_OFFSETS.size:
        print(f"warn: h_est has {h.size} elements, expected {ACTIVE_OFFSETS.size}")
    f_mhz = ACTIVE_OFFSETS * (fs / FFT_SIZE_INFERRED) * 1e-6 if False else \
            ACTIVE_OFFSETS * 31.25e-3  # 31.25 kHz spacing → MHz axis
    fig, axs = plt.subplots(2, 1, sharex=True, figsize=(8, 4))
    axs[0].plot(f_mhz[: h.size], np.abs(h), ".-")
    axs[0].set_ylabel("Amplitude")
    axs[1].plot(f_mhz[: h.size], np.rad2deg(np.angle(h)), ".-")
    axs[1].set_ylabel("Phase (deg)")
    axs[1].set_xlabel("Frequency offset (MHz)")
    axs[0].set_title("First PPDU — channel estimate (h_est)")
    fig.tight_layout()
    fig.savefig(out, dpi=120)
    print(f"wrote {out}")


# Used only inside the channel-estimate plot above; kept for clarity.
FFT_SIZE_INFERRED = 128  # at 4 MSps; rx_debug bumps this to 128


def _equalize_stf(stf_td: np.ndarray, h_est: np.ndarray) -> list[np.ndarray]:
    """Demodulate the time-domain STF samples and equalise.

    Mirrors the notebook:
      cp_offset = Tcp/2; cp_corr = -Tcp/2
      stf = fftshift(fft(x[cp_offset + j*Ts:][:Tu])) · exp(j 2π cp_corr fftfreq(Tu))
      stf = stf[-26+64 : 27+64] · conj(stf_syms)
      stf_eq = stf[stf_syms != 0] / h_est[-26+64:27+64][stf_syms != 0]
    Returns one ndarray per available STF symbol (0, 1, or 2).
    """
    # 802.11ah constants @ 4 MSps
    Tu, Tcp, Ts = 128, 32, 160
    if stf_td.size == 0:
        return []
    # Notebook stf_syms — non-zero every 4 subcarriers (with the gap at DC)
    stf_syms = np.array([
        0, 0, 1+1j, 0, 0, 0, -1-1j, 0, 0, 0, 1+1j, 0, 0, 0, -1-1j,
        0, 0, 0, -1-1j, 0, 0, 0, 1+1j, 0, 0, 0, 0, 0, 0, 0, -1-1j,
        0, 0, 0, -1-1j, 0, 0, 0, 1+1j, 0, 0, 0, 1+1j, 0, 0, 0, 1+1j,
        0, 0, 0, 1+1j, 0, 0
    ], dtype=complex) / np.sqrt(2)
    nz = stf_syms != 0

    # h_est is over the 56 active subcarriers (-28..-1, +1..+28).
    # We need it at offsets -26..+26 — that's a 53-element slice centred on it,
    # i.e. drop offsets -28, -27 (indices 0,1) and +27, +28 (indices 54,55),
    # and re-insert a zero at the DC position (offset 0) for a 53-element array.
    h_minus = h_est[2:28]   # offsets -26 .. -1 (26 values)
    h_plus = h_est[28:54]   # offsets +1 .. +26 (26 values)
    h_53 = np.concatenate([h_minus, [0.0 + 0j], h_plus])

    # cp_offset = Tcp/2 = 16; cp_corr = -Tcp/2 = -16.
    cp_offset = Tcp // 2
    cp_corr = -Tcp // 2
    syms_out = []
    # We have stf_td of length up to 2·Tu. Extract symbol(s) we can demodulate
    # given that the time origin of stf_td corresponds to the *start* of STF
    # sym1 (= start_idx in the notebook). cp_offset shifts the FFT window into
    # the symbol body to be tolerant to STO.
    n = stf_td.size
    for j in range(2):
        start = j * Ts + cp_offset
        if start + Tu > n:
            break
        block = stf_td[start:start + Tu]
        spec = np.fft.fftshift(np.fft.fft(block)) \
               * np.exp(1j * 2 * np.pi * cp_corr * np.fft.fftshift(np.fft.fftfreq(Tu)))
        # spec is length Tu = 128; subcarriers -26..+26 are at indices [-26+64 : +27+64] = [38:91]
        sub = spec[38:91] * np.conjugate(stf_syms)
        # Filter to non-zero stf subcarriers (12 of them) and equalise.
        with np.errstate(divide="ignore", invalid="ignore"):
            eq = sub[nz] / h_53[nz]
            # apply sqrt(12/56) scaling like the notebook does
            eq = eq * np.sqrt(12 / 56)
        syms_out.append(eq)
    return syms_out


def plot_first_ppdu_constellations(here: str) -> None:
    """Mirrors decode_ppdu's `plots=True` constellation grid.

    Panels:  STF sym1 | STF sym2
             LTF1 sym1 | LTF1 sym2
             SIG sym1  | SIG sym2
             data syms | (empty)
    """
    sig1_p = os.path.join(here, "sig1.cf32")
    sig2_p = os.path.join(here, "sig2.cf32")
    data_p = os.path.join(here, "data_syms.cf32")
    ltf1_p = os.path.join(here, "ltf1_eq.cf32")
    stf_p = os.path.join(here, "stf_td.cf32")
    h_p = os.path.join(here, "h_est.cf32")

    if not all(os.path.exists(p) for p in [sig1_p, sig2_p, data_p]):
        print("skip first-PPDU constellation plot: missing SIG/data dumps (no PPDU decoded)")
        return
    out = os.path.join(here, "first_ppdu_constellations.png")

    sig1 = np.fromfile(sig1_p, dtype="complex64")
    sig2 = np.fromfile(sig2_p, dtype="complex64")
    data = np.fromfile(data_p, dtype="complex64")
    ltf1 = np.fromfile(ltf1_p, dtype="complex64") if os.path.exists(ltf1_p) else None
    stf_td = np.fromfile(stf_p, dtype="complex64") if os.path.exists(stf_p) else np.array([], dtype="complex64")
    h_est = np.fromfile(h_p, dtype="complex64") if os.path.exists(h_p) else None

    # FFT the STF time-domain samples and equalise via h_est.
    stf_eq = _equalize_stf(stf_td, h_est) if h_est is not None else []

    # LTF1 dump is [sym1 (56) , sym2 (56)] — split.
    if ltf1 is not None and ltf1.size == 2 * 56:
        ltf_eq = [ltf1[:56], ltf1[56:]]
    else:
        ltf_eq = []

    fig, axs = plt.subplots(4, 2, figsize=(7, 12))

    # Row 0: STF
    if len(stf_eq) >= 1:
        _const_plot(axs[0, 0], stf_eq[0], "STF (1st symbol)", color="C2",
                    ref=np.array([1.0]))  # pilot reference
    else:
        axs[0, 0].axis("off"); axs[0, 0].text(0.5, 0.5, "STF sym1\n(unavailable)",
                                              ha="center", va="center")
    if len(stf_eq) >= 2:
        _const_plot(axs[0, 1], stf_eq[1], "STF (2nd symbol)", color="C2",
                    ref=np.array([1.0]))
    else:
        axs[0, 1].axis("off"); axs[0, 1].text(0.5, 0.5, "STF sym2\n(unavailable)",
                                              ha="center", va="center")
    # Row 1: LTF1
    if len(ltf_eq) == 2:
        _const_plot(axs[1, 0], ltf_eq[0], "LTF1 (1st symbol)", color="C2",
                    ref=np.array([1.0]))
        _const_plot(axs[1, 1], ltf_eq[1], "LTF1 (2nd symbol)", color="C2",
                    ref=np.array([1.0]))
    else:
        for ax in (axs[1, 0], axs[1, 1]):
            ax.axis("off")
            ax.text(0.5, 0.5, "LTF1\n(unavailable)", ha="center", va="center")

    # Row 2: SIG (BPSK rotated — points on imaginary axis)
    bpsk_rot = np.array([-1j, 1j])
    _const_plot(axs[2, 0], sig1, "SIG (1st symbol)", color="C0", ref=bpsk_rot)
    _const_plot(axs[2, 1], sig2, "SIG (2nd symbol)", color="C0", ref=bpsk_rot)

    # Row 3: data symbols (left), empty (right)
    _const_plot(axs[3, 0], data, "data symbols (eq.)", color="C0")
    axs[3, 1].axis("off")

    fig.suptitle("First PPDU — equalised constellations", y=0.995)
    fig.tight_layout()
    fig.savefig(out, dpi=120)
    print(f"wrote {out}  (STF panels: {len(stf_eq)}, LTF1 panels: {len(ltf_eq)}, "
          f"SIG1 {sig1.size}, SIG2 {sig2.size}, data {data.size} = {data.size // 52} syms)")


def plot_sync_long_corr(here: str) -> None:
    """SyncLong's cross-correlation magnitude across the 2·Ts search window.
    Real LTF should produce two peaks separated by FFT_SIZE = 128 samples.
    """
    path = os.path.join(here, "sync_long_corr_mag.f32")
    out = os.path.join(here, "sync_long_corr_mag.png")
    if not os.path.exists(path):
        return
    m = np.fromfile(path, dtype="float32")
    if m.size == 0:
        return
    # find top-2 peaks (same logic as the Correlator)
    order = np.argsort(m)[::-1]
    p1, p2 = sorted(order[:2])
    fig, ax = plt.subplots(figsize=(12, 3.5))
    ax.plot(m, linewidth=0.6)
    ax.axvline(p1, color="C1", linewidth=0.8, alpha=0.5,
               label=f"peak1 @ {p1}")
    ax.axvline(p2, color="C2", linewidth=0.8, alpha=0.5,
               label=f"peak2 @ {p2} (gap={p2 - p1}, expected 128)")
    ax.set_title("SyncLong correlation magnitude (first PPDU)")
    ax.set_xlabel("Sample (within 2·Ts search window)")
    ax.set_ylabel("|corr|")
    ax.legend()
    fig.tight_layout()
    fig.savefig(out, dpi=120)
    print(f"wrote {out}  (top-2 peaks at {p1}, {p2}; gap={p2 - p1}, expected 128)")


def plot_ltf_td(here: str) -> None:
    """The 2·Tu time-domain LTF samples SyncLong picks. FFT them and look at
    the magnitude spectrum: should be flat over the active subcarriers."""
    path = os.path.join(here, "ltf_td.cf32")
    out_t = os.path.join(here, "ltf_td_time.png")
    out_f = os.path.join(here, "ltf_td_spectrum.png")
    if not os.path.exists(path):
        return
    z = np.fromfile(path, dtype="complex64")
    if z.size < 256:
        print(f"warn: ltf_td has only {z.size} samples, expected 256")
        return
    Tu = 128
    # time-domain magnitude
    fig, ax = plt.subplots(figsize=(10, 3.5))
    ax.plot(np.abs(z), linewidth=0.7)
    ax.axvline(Tu, color="grey", linestyle="--", linewidth=0.6,
               label=f"sym1 / sym2 boundary (Tu = {Tu})")
    ax.set_title("LTF time-domain magnitude")
    ax.set_xlabel("Sample")
    ax.set_ylabel("|x|")
    ax.legend()
    fig.tight_layout()
    fig.savefig(out_t, dpi=120)
    print(f"wrote {out_t}")

    # FFT each symbol → magnitude spectrum
    s1 = np.fft.fftshift(np.fft.fft(z[:Tu]))
    s2 = np.fft.fftshift(np.fft.fft(z[Tu:2*Tu]))
    f = np.arange(-Tu//2, Tu//2)
    fig, axs = plt.subplots(2, 1, sharex=True, figsize=(10, 5))
    axs[0].plot(f, np.abs(s1), ".-")
    axs[0].set_ylabel("|FFT(LTF1 sym1)|")
    axs[1].plot(f, np.abs(s2), ".-")
    axs[1].set_ylabel("|FFT(LTF1 sym2)|")
    axs[1].set_xlabel("Subcarrier offset from DC")
    for ax in axs:
        ax.axvspan(-28, 28, color="grey", alpha=0.1, label="active subcarriers")
        ax.legend(loc="upper right")
    fig.suptitle("LTF spectrum (pre-equalisation)")
    fig.tight_layout()
    fig.savefig(out_f, dpi=120)
    print(f"wrote {out_f}")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--dir", default=None,
                   help="directory containing dump files to render "
                        "(default: this script's directory)")
    p.add_argument("--fs", type=float, default=FS_DEFAULT,
                   help="sample rate in Hz (default 4e6)")
    p.add_argument("--corr-start", type=int, default=3_310_000,
                   help="first sample of stf_corr slice (default 3,310,000, "
                        "matches notebook's sel = slice(3310000, 3316000))")
    p.add_argument("--corr-len", type=int, default=6_000,
                   help="length of stf_corr slice (default 6000)")
    p.add_argument("--max-blocks", type=int, default=0,
                   help="number of detection-window points to plot (0 = all)")
    args = p.parse_args()

    here = os.path.abspath(args.dir) if args.dir else os.path.dirname(os.path.abspath(__file__))

    plot_stf_corr_zoom(here, args.fs, args.corr_start, args.corr_len)
    plot_stf_corr_full(here, args.fs)
    plot_stf_metric(here, args.max_blocks)
    plot_first_ppdu_channel(here, args.fs)
    plot_first_ppdu_constellations(here)
    plot_sync_long_corr(here)
    plot_ltf_td(here)
    plt.show()


if __name__ == "__main__":
    main()
*** Add File: /home/alakhdar/Projets/DynLib10.0/FutureSDR/examples/wlan_ah/plotv2/render.py
#!/usr/bin/env python3

from pathlib import Path
import runpy
import sys


def main() -> None:
    here = Path(__file__).resolve().parent
    target = here.parent / "plots" / "render.py"
    if "--dir" not in sys.argv:
        sys.argv.extend(["--dir", str(here)])
    runpy.run_path(str(target), run_name="__main__")


if __name__ == "__main__":
    main()
