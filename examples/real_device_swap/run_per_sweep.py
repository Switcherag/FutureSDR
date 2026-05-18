#!/usr/bin/env python3
"""PER sweep orchestrator: one flowgraph per PHY, sweep gain live.

For each PHY ∈ {zigbee, halow}:

  1. Start `phy_per --flow <toml> --gains 0,4,8,12,16,20 ...` exactly once.
     The bin loads the SDR head + tail + PHY flowgraph, applies the
     first gain, and prints `READY gain=<g>` on stdout to signal that
     it's settled on a new gain.

  2. For each gain value:
       a. Read phy_per stdout until we see `READY gain=<g>`.
       b. Wait 4 s for the SDR / flowgraph to fully warm up.
       c. Trigger the ESP TX (flash on first iteration; reset between
          iterations; auto-restart firmware can self-restart instead).
       d. Read ESP serial until the `=== done count=N` sentinel.
       e. Write `NEXT\\n` into phy_per stdin → it retunes the radio
          live (via FlowgraphController::set_gain) and emits a new
          `READY gain=<next>` line.

  3. After the last gain, write `QUIT\\n` and wait for phy_per to exit.

Every received frame phy_per saw is in `data/phy_per_<phy>.csv` with the
active `gain_db` in the first column, so PER is post-computable per
(phy, gain).

The bootstrap step sources the ESP-IDF env equivalent to:

    . <--idf-path>/export.sh
    export MMIOT_ROOT=<--tx-project>/../../mm-iot-esp32
    idf.py set-target <--target>

so you don't need to source anything in your own shell first.

TX firmware modes (`--mode`):

  reset (default) — flash once, DTR/RTS pulse between gain steps.
  flash           — reflash every gain step.
  auto-restart    — flash with -DAUTO_RESTART=1; firmware self-restarts.
"""
import argparse
import csv
import shlex
import subprocess
import sys
import threading
import time
from pathlib import Path

try:
    import serial
except ImportError:
    sys.exit("missing pyserial — install with `pip install --user pyserial`")

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parents[1]
RX_BIN = REPO_ROOT / "target" / "release" / "phy_per"
DATA_DIR = HERE / "data"

PHYS = [
    ("zigbee", "flows/zigbee_rx.toml"),
    ("halow",  "flows/wlan_ah_rx_v2.toml"),
]
GAINS = list(range(0, 21, 4))    # 0, 4, 8, 12, 16, 20

DEFAULT_TX_PROJECT = "/home/alakhdar/Projets/XiaoRadio/continuous_tx/multi_phy_desc"
DEFAULT_IDF_PATH   = "/home/alakhdar/Projets/XiaoRadio/esp-idf"
DEFAULT_TARGET     = "esp32c6"

READY_PREFIX = "READY gain="


# ── ESP-IDF environment bootstrap ──────────────────────────────────

def build_idf_env(idf_dir: Path, tx_project: Path) -> dict:
    export_sh = (idf_dir / "export.sh").resolve()
    if not export_sh.exists():
        sys.exit(f"export.sh not found at {export_sh} — pass --idf-path")
    mmiot_root = (tx_project / ".." / ".." / "mm-iot-esp32").resolve()
    cmd = (
        f". {shlex.quote(str(export_sh))} >/dev/null && "
        f"export MMIOT_ROOT={shlex.quote(str(mmiot_root))} && "
        f"env -0"
    )
    print(f"  sourcing {export_sh}")
    print(f"  MMIOT_ROOT={mmiot_root}")
    result = subprocess.run(["bash", "-c", cmd], capture_output=True)
    if result.returncode != 0:
        sys.exit(f"failed to source IDF env:\n"
                 f"{result.stderr.decode(errors='replace')}")
    env = {}
    for entry in result.stdout.split(b"\x00"):
        if not entry:
            continue
        k, _, v = entry.decode(errors="replace").partition("=")
        if k:
            env[k] = v
    return env


def idf_set_target(env: dict, tx_project: Path, target: str):
    cmd = ["idf.py", "-C", str(tx_project), "set-target", target]
    print("  $ " + " ".join(shlex.quote(c) for c in cmd))
    rc = subprocess.run(cmd, env=env)
    if rc.returncode != 0:
        sys.exit(f"idf.py set-target failed (rc={rc.returncode})")


# ── ESP TX helpers ─────────────────────────────────────────────────

def reset_esp(port: str):
    with serial.Serial(port, baudrate=115200) as ser:
        ser.dtr = False
        ser.rts = True
        time.sleep(0.15)
        ser.rts = False


def wait_sentinel(port: str, count: int, timeout_s: float, mirror: bool):
    deadline = time.monotonic() + timeout_s
    sentinel = f"=== done count={count}"
    with serial.Serial(port, baudrate=115200, timeout=2) as ser:
        while time.monotonic() < deadline:
            try:
                line = ser.readline().decode(errors="replace").rstrip()
            except serial.SerialException as e:
                print(f"  serial read error: {e}", file=sys.stderr)
                return False
            if not line:
                continue
            if mirror:
                print(f"  [TX] {line}")
            if line.startswith(sentinel):
                return True
    return False


def flash_tx(project_dir: str, count: int, port: str,
             extra_defines: list, env, auto_restart: bool):
    defs = [f"-DCOUNT={count}"]
    if auto_restart:
        defs.append("-DAUTO_RESTART=1")
    defs += list(extra_defines)
    cmd = ["idf.py", "-C", project_dir, "-p", port] + defs + ["flash"]
    print("  $ " + " ".join(shlex.quote(c) for c in cmd))
    return subprocess.run(cmd, env=env, check=False)


# ── phy_per I/O ─────────────────────────────────────────────────────

class PhyPerProc:
    """A running phy_per subprocess with line-based stdin/stdout."""

    def __init__(self, flow: str, gains: list, out_csv: Path,
                 max_wait_s: float):
        self.flow = flow
        self.gains = gains
        self.out_csv = out_csv
        cmd = [
            str(RX_BIN),
            "--flow", flow,
            "--gains", ",".join(str(g) for g in gains),
            "--out", str(out_csv),
            "--max-wait-s", str(max_wait_s),
        ]
        print("  $ " + " ".join(shlex.quote(c) for c in cmd))
        self.proc = subprocess.Popen(
            cmd, cwd=str(HERE),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True, bufsize=1,
        )
        # Pipe phy_per's stdout to our stdout in a background thread so
        # the user sees its activity, while we also pluck out the READY
        # lines via a queue.
        self._ready_event = threading.Event()
        self._last_ready_gain: float | None = None
        self._reader = threading.Thread(
            target=self._read_stdout, daemon=True)
        self._reader.start()

    def _read_stdout(self):
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            line = line.rstrip("\n")
            print(f"  [RX] {line}")
            if line.startswith(READY_PREFIX):
                try:
                    g = float(line[len(READY_PREFIX):])
                except ValueError:
                    continue
                self._last_ready_gain = g
                self._ready_event.set()

    def wait_ready(self, expected_gain: float, timeout_s: float = 30.0) -> bool:
        """Block until phy_per prints `READY gain=<expected_gain>`."""
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            if self._ready_event.is_set() and \
                    self._last_ready_gain == expected_gain:
                self._ready_event.clear()
                return True
            time.sleep(0.05)
        return False

    def send(self, cmd: str):
        assert self.proc.stdin is not None
        try:
            self.proc.stdin.write(cmd + "\n")
            self.proc.stdin.flush()
        except (BrokenPipeError, ValueError):
            pass

    def quit_and_wait(self, timeout_s: float = 30.0):
        self.send("QUIT")
        try:
            self.proc.wait(timeout=timeout_s)
        except subprocess.TimeoutExpired:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()


# ── PER post-processing ───────────────────────────────────────────

def count_per_gain(out_csv: Path) -> dict:
    """Return {gain_db: rx_count} from a phy_per CSV."""
    counts: dict = {}
    if not out_csv.exists():
        return counts
    with out_csv.open() as f:
        reader = csv.DictReader(f)
        for row in reader:
            try:
                g = float(row["gain_db"])
            except (KeyError, ValueError):
                continue
            counts[g] = counts.get(g, 0) + 1
    return counts


# ── main sweep ─────────────────────────────────────────────────────

def main():
    p = argparse.ArgumentParser()
    p.add_argument("--port", default="/dev/ttyUSB0")
    p.add_argument("--count", type=int, default=1000,
                   help="frames per gain step (TX -DCOUNT)")
    p.add_argument("--mode", choices=("reset", "flash", "auto-restart"),
                   default="reset")
    p.add_argument("--wait-ms", type=int, default=None,
                   help="-DWAIT_MS=<n> for idf.py")
    p.add_argument("--country-code", default=None,
                   help="-DCOUNTRY_CODE=<v> for idf.py (e.g. US)")
    p.add_argument("--tx-project", default=DEFAULT_TX_PROJECT)
    p.add_argument("--idf-path", default=DEFAULT_IDF_PATH)
    p.add_argument("--target", default=DEFAULT_TARGET)
    p.add_argument("--skip-target", action="store_true")
    p.add_argument("--tx-define", action="append", default=[])
    p.add_argument("--max-wait-s", type=float, default=180.0,
                   help="sentinel wait timeout per gain step")
    p.add_argument("--phy-per-max-s", type=float, default=3600.0,
                   help="hard cap on the whole phy_per run")
    p.add_argument("--mirror-tx", action="store_true")
    p.add_argument("--gains", type=str, default=None,
                   help="comma-separated gains, e.g. 0,8,16")
    p.add_argument("--phys", type=str, default=None,
                   help="comma-separated PHY names (zigbee,halow)")
    args = p.parse_args()

    if not RX_BIN.exists():
        sys.exit(f"RX binary not found at {RX_BIN}. Build it first.")
    tx_project = Path(args.tx_project).resolve()
    if not tx_project.is_dir():
        sys.exit(f"TX project dir not found: {tx_project}")

    print("=== bootstrapping ESP-IDF env ===")
    idf_env = build_idf_env(Path(args.idf_path), tx_project)
    if not args.skip_target:
        idf_set_target(idf_env, tx_project, args.target)

    DATA_DIR.mkdir(exist_ok=True)
    summary_path = DATA_DIR / "per_sweep_summary.csv"

    gains = [int(g) for g in args.gains.split(",")] if args.gains else GAINS
    if args.phys:
        wanted = {n.strip() for n in args.phys.split(",")}
        phys = [(n, f) for n, f in PHYS if n in wanted]
    else:
        phys = PHYS

    tx_defines = list(args.tx_define)
    if args.wait_ms is not None:
        tx_defines.append(f"-DWAIT_MS={args.wait_ms}")
    if args.country_code is not None:
        tx_defines.append(f"-DCOUNTRY_CODE={args.country_code}")

    auto_restart = (args.mode == "auto-restart")
    already_flashed = False

    with summary_path.open("w") as fsum:
        wsum = csv.writer(fsum)
        wsum.writerow(["phy", "gain_db", "expected", "received", "per",
                       "out_csv"])
        fsum.flush()

        for phy_name, flow in phys:
            out_csv = DATA_DIR / f"phy_per_{phy_name}.csv"
            if out_csv.exists():
                out_csv.unlink()
            print(f"\n=== PHY={phy_name}  gains={gains}  count={args.count} ===")

            rx = PhyPerProc(flow, gains, out_csv, args.phy_per_max_s)

            # Wait for initial READY gain=<first>
            if not rx.wait_ready(gains[0], timeout_s=30.0):
                print(f"  phy_per never reported READY gain={gains[0]} — aborting PHY")
                rx.quit_and_wait()
                continue

            for gi, gain in enumerate(gains):
                print(f"\n  -- gain={gain} dB --")
                # Wait 4 s for the SDR + flowgraph to settle on this gain.
                print("  waiting 4 s for flowgraph init...")
                time.sleep(4.0)

                # Trigger the TX cycle.
                if args.mode == "flash" or not already_flashed:
                    rc = flash_tx(str(tx_project), args.count, args.port,
                                  tx_defines, idf_env, auto_restart)
                    if rc.returncode != 0:
                        print("  idf.py flash failed — aborting this run")
                        break
                    already_flashed = True
                elif args.mode == "auto-restart":
                    print("  (auto-restart) waiting for next TX cycle...")
                else:
                    try:
                        reset_esp(args.port)
                    except serial.SerialException as e:
                        print(f"  serial reset failed: {e}")
                        break

                # Wait for the TX sentinel = end of this gain's cycle.
                ok = wait_sentinel(args.port, args.count,
                                   timeout_s=args.max_wait_s,
                                   mirror=args.mirror_tx)
                if not ok:
                    print(f"  WARN: sentinel not seen within {args.max_wait_s}s")

                # Advance phy_per to the next gain (or finish on last).
                if gi + 1 < len(gains):
                    next_gain = gains[gi + 1]
                    rx.send("NEXT")
                    if not rx.wait_ready(next_gain, timeout_s=10.0):
                        print(f"  WARN: phy_per never confirmed gain={next_gain}")

            rx.quit_and_wait()

            # Tally per-gain counts for the summary CSV.
            per_gain = count_per_gain(out_csv)
            for gain in gains:
                received = per_gain.get(float(gain), 0)
                per = 1.0 - (received / args.count) if args.count > 0 else 0.0
                print(f"  → gain={gain} dB  received={received}/{args.count}  PER={per:.4f}")
                wsum.writerow([phy_name, gain, args.count, received,
                               f"{per:.6f}", str(out_csv)])
            fsum.flush()

    print(f"\nsweep complete → {summary_path}")


if __name__ == "__main__":
    main()
