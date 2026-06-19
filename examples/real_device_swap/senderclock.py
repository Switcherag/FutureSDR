#!/usr/bin/env python3
"""Monitor the *sender's* clock as seen through `zigbee_swap`.

Every frame the multizig firmware transmits carries a 19-byte stamp whose
`ts_us` field is the **sender's own monotonic clock** (µs) at transmit time,
plus `wait_us`, the inter-frame interval the sender was *programmed* to wait.
The receiver (this SDR) records `rx_t_ms` — its **local** clock — when the
decoded frame surfaces on the controller tap. Comparing the two clocks tells
us three things about the transmitter, none of which the swap-timing analysis
covers:

  * SKEW   — the sender and receiver oscillators run at slightly different
    rates. Fitting receiver-elapsed against sender-elapsed gives a constant
    slope; (slope-1) is the frequency offset in ppm.

  * WANDER / JITTER — the offset left after removing that linear skew. This is
    dominated by receiver-side arrival jitter (RX pipeline + scheduling) but
    also captures any short-term wander in the sender clock.

  * CADENCE FIDELITY — does the sender actually hit its programmed `wait_us`?
    Δ(ts_us) between consecutive frames should equal the previous frame's
    `wait_us`. Deviations are either missed RX frames (step counter skips) or
    deliberate pauses at a sweep reset / run boundary.

Input (alongside this script):
    zigbee_swap.csv   one row per received frame
                      (…,step,run,tag,wait_us,ts_us,rx_t_ms,swap_ms)

Output:
    senderclock.png   offset/skew + de-trended jitter + cadence plots
    a stats summary on stdout
"""
import csv
import statistics as stats
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np

HERE = Path(__file__).parent
CSV_PATH = HERE / "zigbee_swap.csv"
OUT_PATH = HERE / "senderclock.png"


def load_frames(path):
    """Return valid received frames (have a real TX stamp), in capture order.

    Timeout rows and parse failures carry sentinel -1 fields and are dropped.
    """
    if not path.exists():
        raise SystemExit(f"missing {path} — run zigbee_swap to produce it first")
    frames = []
    with path.open() as f:
        for r in csv.DictReader(f):
            if r["frame_event"] != "rx":
                continue
            ts = int(r["ts_us"])
            step = int(r["step"])
            if ts < 0 or step < 0:
                continue  # sentinel row (timeout / parse fail)
            frames.append(
                {
                    "ts_us": ts,
                    "rx_t_ms": float(r["rx_t_ms"]),
                    "wait_us": int(r["wait_us"]),
                    "step": step,
                    "run": int(r["run"]),
                    "phy": r["phy_active"],
                }
            )
    if len(frames) < 2:
        raise SystemExit("need at least 2 stamped frames to compare clocks")
    # ts_us is the sender's monotonic clock; keep capture order but assert it.
    frames.sort(key=lambda f: f["ts_us"])
    return frames


def main():
    frames = load_frames(CSV_PATH)
    n = len(frames)

    ts_us = np.array([f["ts_us"] for f in frames], dtype=np.float64)
    rx_ms = np.array([f["rx_t_ms"] for f in frames], dtype=np.float64)
    wait_us = np.array([f["wait_us"] for f in frames], dtype=np.float64)
    step = np.array([f["step"] for f in frames], dtype=np.int64)
    run = np.array([f["run"] for f in frames], dtype=np.int64)

    # Both clocks measured as elapsed-from-first-frame, in ms.
    sender_ms = (ts_us - ts_us[0]) / 1000.0
    recv_ms = rx_ms - rx_ms[0]
    offset_ms = recv_ms - sender_ms  # receiver clock minus sender clock

    # Linear skew: fit offset against the sender timeline. slope is ms of
    # offset accrued per ms of sender time -> ppm frequency error.
    slope, intercept = np.polyfit(sender_ms, offset_ms, 1)
    skew_ppm = slope * 1e6
    fit = slope * sender_ms + intercept
    resid_ms = offset_ms - fit  # de-skewed wander + arrival jitter
    total_drift = offset_ms[-1] - offset_ms[0]

    # Cadence: gap between consecutive frames vs the programmed wait. The
    # sender applies frame i-1's `wait_us` before transmitting frame i, so
    # d_send[i] should equal wait_us[i-1].
    d_send = np.diff(ts_us)                 # actual sender interval (µs)
    d_recv = np.diff(rx_ms) * 1000.0        # receiver-measured interval (µs)
    wait_prev = wait_us[:-1]                # programmed interval for that gap
    step_gap = np.diff(step)                # 1 when no frame was missed
    run_change = np.diff(run) != 0
    missed = (step_gap != 1) & ~run_change
    normal = (step_gap == 1) & ~run_change
    cad_err = d_send[normal] - wait_prev[normal]  # sender timing error (µs)

    # ---- stats summary -----------------------------------------------------
    span_s = sender_ms[-1] / 1000.0
    print(f"\nSender-clock monitor — {n} stamped frames over {span_s:.2f} s "
          f"(runs {sorted(set(run.tolist()))})\n")
    print(f"  skew vs receiver : {skew_ppm:+.2f} ppm "
          f"(slope {slope:+.3e} ms/ms; positive = receiver fast / sender SLOW)")
    print(f"  total drift      : {total_drift:+.3f} ms over the capture")
    print(f"  residual jitter  : std {stats.pstdev(resid_ms.tolist()):.4f} ms, "
          f"p95 |{np.percentile(np.abs(resid_ms), 95):.4f}| ms, "
          f"max |{np.max(np.abs(resid_ms)):.4f}| ms")
    if cad_err.size:
        print(f"  cadence error    : median {np.median(cad_err):+.1f} µs, "
              f"p95 |{np.percentile(np.abs(cad_err), 95):.1f}| µs "
              f"(normal intervals only, n={cad_err.size})")
    print(f"  missed frames    : {int(missed.sum())} step-gaps, "
          f"{int(run_change.sum())} run boundaries\n")

    # ---- plots -------------------------------------------------------------
    fig, (ax1, ax2, ax3) = plt.subplots(3, 1, figsize=(11, 13))

    # Panel 1: clock offset over the sender timeline + linear-skew fit.
    ax1.plot(sender_ms / 1000.0, offset_ms, ".", ms=3, color="tab:blue",
             alpha=0.6, label="offset (recv − sender)")
    ax1.plot(sender_ms / 1000.0, fit, "-", color="k", lw=1.2,
             label=f"linear skew {skew_ppm:+.1f} ppm")
    ax1.set_title("Sender vs receiver clock offset — accumulated skew")
    ax1.set_xlabel("sender clock (s, elapsed)")
    ax1.set_ylabel("offset recv − sender (ms)")
    ax1.annotate(f"total drift {total_drift:+.3f} ms",
                 xy=(0.99, 0.04), xycoords="axes fraction", ha="right",
                 fontsize=9, color="dimgrey")
    ax1.legend(fontsize=9)
    ax1.grid(True, alpha=0.3)

    # Panel 2: residual after removing the linear skew (wander + jitter),
    # coloured per run so a clock event at a run boundary stands out.
    cmap = plt.get_cmap("tab10")
    for j, rid in enumerate(sorted(set(run.tolist()))):
        m = run == rid
        ax2.plot(sender_ms[m] / 1000.0, resid_ms[m], ".", ms=3,
                 color=cmap(j), alpha=0.6, label=f"run {rid}")
    ax2.axhline(0.0, color="k", lw=0.8)
    sd = stats.pstdev(resid_ms.tolist())
    ax2.axhline(sd, color="grey", ls="--", lw=0.8, label=f"±1σ = {sd:.3f} ms")
    ax2.axhline(-sd, color="grey", ls="--", lw=0.8)
    ax2.set_title("De-skewed residual — sender clock wander + receiver arrival jitter")
    ax2.set_xlabel("sender clock (s, elapsed)")
    ax2.set_ylabel("residual (ms)")
    ax2.legend(fontsize=8, ncol=2)
    ax2.grid(True, alpha=0.3)

    # Panel 3: cadence fidelity — actual sender interval vs programmed wait.
    # Points on the diagonal mean the sender honoured its schedule exactly.
    ax3.plot([wait_us.min(), wait_us.max()], [wait_us.min(), wait_us.max()],
             "-", color="k", lw=1, label="Δts = programmed (ideal)")
    ax3.plot(wait_prev[normal], d_send[normal], ".", ms=4, color="tab:green",
             alpha=0.5, label=f"normal (n={int(normal.sum())})")
    if missed.any():
        ax3.plot(wait_prev[missed], d_send[missed], "x", ms=7,
                 color="tab:red", label=f"missed frame (n={int(missed.sum())})")
    if run_change.any():
        ax3.plot(wait_prev[run_change], d_send[run_change], "s", ms=7,
                 mfc="none", color="tab:orange",
                 label=f"run boundary (n={int(run_change.sum())})")
    ax3.set_xscale("log")
    ax3.set_yscale("log")
    ax3.set_title("Cadence fidelity — actual sender interval vs programmed wait")
    ax3.set_xlabel("programmed wait_us (µs, log)")
    ax3.set_ylabel("actual Δ sender clock (µs, log)")
    ax3.legend(fontsize=8)
    ax3.grid(True, which="both", alpha=0.3)

    fig.tight_layout()
    fig.savefig(OUT_PATH, dpi=130)
    print(f"wrote {OUT_PATH}")
    plt.show()


if __name__ == "__main__":
    main()
