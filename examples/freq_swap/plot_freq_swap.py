#!/usr/bin/env python3
"""Plot the spectrogram of an RX capture produced by `freq_swap`.

Usage:
    python plot_freq_swap.py [iq_file] [meta_file]

Defaults to ./freq_swap.cf32 and ./freq_swap.meta.json.
"""

import json
import sys
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
from matplotlib.colors import Normalize

NFFT = 2048
NOVERLAP = NFFT // 2


def load_meta(path: Path) -> dict:
    if path.exists():
        with path.open() as f:
            return json.load(f)
    print(f"WARN: {path} not found, using defaults", file=sys.stderr)
    return {
        "sample_rate_hz": 4e6,
        "rx_start_freq_hz": 830e6,
        "tx_start_freq_hz": 828.5e6,
        "tx_target_freq_hz": 831.5e6,
        "rx_target_freq_hz": 832e6,
        "tx_retune_t_s": None,
        "rx_retune_t_s": None,
    }


def load_iq(path: Path) -> np.ndarray:
    raw = np.fromfile(path, dtype=np.float32)
    if raw.size % 2:
        raw = raw[:-1]
    return raw.view(np.complex64)


def stft(iq: np.ndarray, fs: float, nfft: int, noverlap: int):
    """Explicit windowed STFT for complex IQ.

    Returns (P_db, t_centers, f_shifted) with f sorted from -fs/2 to +fs/2
    and P_db arranged so row 0 is the lowest frequency.
    """
    step = nfft - noverlap
    n_frames = 1 + (iq.size - nfft) // step
    if n_frames <= 0:
        raise ValueError(f"capture too short for NFFT={nfft}")
    win = np.hanning(nfft).astype(np.float32)
    win_norm = (win * win).sum() * fs  # PSD normalization

    # Build frame matrix with stride tricks
    shape = (n_frames, nfft)
    strides = (iq.strides[0] * step, iq.strides[0])
    frames = np.lib.stride_tricks.as_strided(iq, shape=shape, strides=strides)
    frames = frames * win  # broadcast along axis 1

    spec = np.fft.fftshift(np.fft.fft(frames, n=nfft, axis=1), axes=1)
    psd = (np.abs(spec) ** 2) / win_norm
    P_db = 10.0 * np.log10(psd.T + 1e-20)  # shape (nfft, n_frames)

    f = np.fft.fftshift(np.fft.fftfreq(nfft, d=1.0 / fs))  # monotonic [-fs/2, fs/2)
    t = (np.arange(n_frames) * step + nfft / 2) / fs
    return P_db, t, f


def draw_spec(ax, iq, fs, nfft, noverlap, cmap):
    P_db, t, f = stft(iq, fs, nfft, noverlap)
    vmax = np.percentile(P_db, 99.5)
    vmin = vmax - 70
    im = ax.imshow(
        P_db,
        extent=(t[0], t[-1], f[0] / 1e6, f[-1] / 1e6),
        aspect="auto",
        origin="lower",
        cmap=cmap,
        norm=Normalize(vmin=vmin, vmax=vmax),
        interpolation="nearest",
    )
    return im, t, f


def main():
    iq_path = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("freq_swap.cf32")
    meta_path = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("freq_swap.meta.json")

    meta = load_meta(meta_path)
    fs = float(meta["sample_rate_hz"])
    rx_start = float(meta["rx_start_freq_hz"])
    tx_start = float(meta["tx_start_freq_hz"])
    tx_target = float(meta["tx_target_freq_hz"])
    rx_target = float(meta["rx_target_freq_hz"])
    tx_t = meta.get("tx_retune_t_s")
    rx_t = meta.get("rx_retune_t_s")

    iq = load_iq(iq_path)
    if iq.size == 0:
        sys.exit(f"empty IQ file: {iq_path}")

    duration = iq.size / fs
    print(f"Loaded {iq.size} samples ({duration:.3f} s) @ {fs/1e6:.3f} MSPS")

    # IQ sanity: if Q is ~zero or strictly correlated with I, the file is real,
    # which would explain a perfectly mirror-symmetric spectrum.
    rms_i = float(np.sqrt(np.mean(iq.real ** 2)))
    rms_q = float(np.sqrt(np.mean(iq.imag ** 2)))
    pearson = float(np.corrcoef(iq.real, iq.imag)[0, 1])
    print(f"IQ check  | rms(I)={rms_i:.4f}  rms(Q)={rms_q:.4f}  corr(I,Q)={pearson:+.4f}")
    if rms_q < 1e-6 or abs(pearson) > 0.99:
        print("  ⚠ Q channel is degenerate — input is effectively real, "
              "spectrum will be mirror-symmetric.")

    fig, axes = plt.subplots(
        2, 1, figsize=(12, 9), sharex=True,
        gridspec_kw={"height_ratios": [3, 1]},
    )
    ax_spec, ax_pow = axes

    im, _t, _f = draw_spec(ax_spec, iq, fs, NFFT, NOVERLAP, "viridis")
    ax_spec.set_ylabel("Baseband frequency [MHz]")
    ax_spec.set_title(
        f"RX spectrogram — RX {rx_start/1e6:.3f}→{rx_target/1e6:.3f} MHz, "
        f"TX {tx_start/1e6:.3f}→{tx_target/1e6:.3f} MHz"
    )
    cbar = plt.colorbar(im, ax=ax_spec, pad=0.01)
    cbar.set_label("PSD [dB/Hz]")

    bb_seg1 = (tx_start - rx_start) / 1e6
    bb_seg2 = (tx_target - rx_start) / 1e6
    bb_seg3 = (tx_target - rx_target) / 1e6

    if tx_t is not None:
        ax_spec.axvline(tx_t, color="red", lw=1.2, ls="--", alpha=0.8,
                        label=f"TX retune @ {tx_t:.3f}s")
        ax_spec.annotate(
            f"TX → {tx_target/1e6:.3f} MHz\n({bb_seg2:+.2f} MHz BB)",
            xy=(tx_t, bb_seg2), xytext=(tx_t + 0.02, bb_seg2),
            color="red", fontsize=9,
            arrowprops=dict(arrowstyle="->", color="red"),
        )
    if rx_t is not None:
        ax_spec.axvline(rx_t, color="orange", lw=1.2, ls="--", alpha=0.8,
                        label=f"RX retune @ {rx_t:.3f}s")
        ax_spec.annotate(
            f"RX → {rx_target/1e6:.3f} MHz\n(TX now @ {bb_seg3:+.2f} MHz BB)",
            xy=(rx_t, bb_seg3), xytext=(rx_t + 0.02, bb_seg3),
            color="orange", fontsize=9,
            arrowprops=dict(arrowstyle="->", color="orange"),
        )

    for y in (bb_seg1, bb_seg2, bb_seg3):
        ax_spec.axhline(y, color="white", lw=0.5, ls=":", alpha=0.4)

    ax_spec.legend(loc="upper right")

    win = max(1, int(fs * 0.001))
    n_win = iq.size // win
    pwr = (np.abs(iq[: n_win * win]).reshape(n_win, win) ** 2).mean(axis=1)
    pwr_db = 10 * np.log10(pwr + 1e-20)
    t_pwr = np.arange(n_win) * (win / fs)

    ax_pow.plot(t_pwr, pwr_db, lw=0.6, color="steelblue")
    ax_pow.set_ylabel("Avg power [dB] / 1 ms")
    ax_pow.set_xlabel("Time [s]")
    ax_pow.grid(True, alpha=0.3)
    if tx_t is not None:
        ax_pow.axvline(tx_t, color="red", lw=1.0, ls="--", alpha=0.8)
    if rx_t is not None:
        ax_pow.axvline(rx_t, color="orange", lw=1.0, ls="--", alpha=0.8)

    plt.tight_layout()
    out_png = iq_path.with_suffix(".png")
    plt.savefig(out_png, dpi=140)
    print(f"Saved figure → {out_png}")

    if tx_t is not None:
        win_s = 0.02  # 20 ms zoom
        i0 = max(0, int((tx_t - win_s) * fs))
        i1 = min(iq.size, int((tx_t + win_s) * fs))
        iq_z = iq[i0:i1]
        if iq_z.size >= NFFT:
            fig2, ax2 = plt.subplots(figsize=(12, 5))
            zoom_nfft = min(NFFT, 1 << int(np.log2(iq_z.size // 4)))
            zoom_olap = zoom_nfft // 2
            P_db, t, f = stft(iq_z, fs, zoom_nfft, zoom_olap)
            t_abs = t + i0 / fs
            vmax = np.percentile(P_db, 99.5)
            vmin = vmax - 70
            ax2.imshow(
                P_db,
                extent=(t_abs[0], t_abs[-1], f[0] / 1e6, f[-1] / 1e6),
                aspect="auto", origin="lower", cmap="magma",
                norm=Normalize(vmin=vmin, vmax=vmax),
                interpolation="nearest",
            )
            ax2.axvline(tx_t, color="cyan", lw=1.2, ls="--", alpha=0.9,
                        label="TX retune callback")
            ax2.set_xlabel("Time [s]")
            ax2.set_ylabel("Baseband freq [MHz]")
            ax2.set_title(f"Zoom ±{win_s*1000:.0f} ms around TX retune (NFFT={zoom_nfft})")
            ax2.legend(loc="upper right")
            plt.tight_layout()
            out_png2 = iq_path.with_name(iq_path.stem + "_zoom.png")
            plt.savefig(out_png2, dpi=140)
            print(f"Saved zoom    → {out_png2}")

    plt.show()


if __name__ == "__main__":
    main()
