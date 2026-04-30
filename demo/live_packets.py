#!/usr/bin/env python3

from __future__ import annotations

import argparse
import socket
import string
import threading
import time
from collections import deque
from dataclasses import dataclass
from typing import Optional

import matplotlib.pyplot as plt
import numpy as np
from matplotlib.animation import FuncAnimation


@dataclass
class PacketEvent:
    proto: str
    received_monotonic: float
    received_wall: float
    sequence: int
    tag: str
    uptime_us: int
    summary: str
    color: str


class PacketStore:
    def __init__(self, maxlen: int = 4096) -> None:
        self._events: deque[PacketEvent] = deque(maxlen=maxlen)
        self._lock = threading.Lock()
        self._totals = {"zigbee": 0, "halow": 0}
        self._missing = {"zigbee": 0, "halow": 0}
        self._last_sequence: dict[str, int] = {}

    def add(self, event: PacketEvent) -> None:
        with self._lock:
            previous = self._last_sequence.get(event.proto)
            if previous is not None:
                gap = event.sequence - previous - 1
                if 0 < gap < 1000:
                    self._missing[event.proto] += gap
            self._last_sequence[event.proto] = event.sequence
            self._totals[event.proto] += 1
            self._events.append(event)

    def snapshot(self) -> tuple[list[PacketEvent], dict[str, int], dict[str, int]]:
        with self._lock:
            return list(self._events), dict(self._totals), dict(self._missing)


def printable_tag(value: int) -> str:
    if 32 <= value <= 126 and chr(value) in string.printable:
        return chr(value)
    return f"0x{value:02x}"


def decode_zigbee(packet: bytes) -> Optional[PacketEvent]:
    if not packet.startswith(b"RFta") or len(packet) < 12 + 15 + 13:
        return None

    mpdu = packet[12:]
    if len(mpdu) < 28:
        return None

    mac_seq = mpdu[2]
    src_eui = mpdu[7:15]
    payload = mpdu[15:28]
    app_seq = int.from_bytes(payload[0:4], "little")
    tag = printable_tag(payload[4])
    uptime_us = int.from_bytes(payload[5:13], "little")
    source_tag = bytes(reversed(src_eui)).rstrip(b"\x00").decode("ascii", errors="replace")

    now_mono = time.monotonic()
    now_wall = time.time()
    summary = (
        f"{time.strftime('%H:%M:%S', time.localtime(now_wall))}  "
        f"Z  mac={mac_seq:03d}  app={app_seq:6d}  tag={tag}  "
        f"src={source_tag or '-':8s}  t={uptime_us / 1e6:9.3f}s"
    )
    return PacketEvent(
        proto="zigbee",
        received_monotonic=now_mono,
        received_wall=now_wall,
        sequence=app_seq,
        tag=tag,
        uptime_us=uptime_us,
        summary=summary,
        color="#0f766e",
    )


def decode_halow(packet: bytes) -> Optional[PacketEvent]:
    if len(packet) < 24 + 2 + 13:
        return None
    if packet[0:2] != b"\x40\x00":
        return None

    source_mac = ":".join(f"{byte:02x}" for byte in packet[10:16])

    payload = None
    index = 24
    while index + 2 <= len(packet):
        element_id = packet[index]
        length = packet[index + 1]
        end = index + 2 + length
        if end > len(packet):
            return None
        if element_id == 0 and length == 13:
            payload = packet[index + 2:end]
            break
        index = end

    if payload is None:
        return None

    sequence = int.from_bytes(payload[0:4], "little")
    tag = printable_tag(payload[4])
    uptime_us = int.from_bytes(payload[5:13], "little")

    now_mono = time.monotonic()
    now_wall = time.time()
    summary = (
        f"{time.strftime('%H:%M:%S', time.localtime(now_wall))}  "
        f"H  seq={sequence:6d}  tag={tag}  sa={source_mac}  "
        f"t={uptime_us / 1e6:9.3f}s"
    )
    return PacketEvent(
        proto="halow",
        received_monotonic=now_mono,
        received_wall=now_wall,
        sequence=sequence,
        tag=tag,
        uptime_us=uptime_us,
        summary=summary,
        color="#b45309",
    )


def decode_packet(packet: bytes) -> Optional[PacketEvent]:
    if packet.startswith(b"RFta"):
        return decode_zigbee(packet)
    return decode_halow(packet)


def listener(host: str, port: int, stop_event: threading.Event, store: PacketStore) -> None:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind((host, port))
        sock.settimeout(0.5)
        while not stop_event.is_set():
            try:
                packet, _ = sock.recvfrom(4096)
            except socket.timeout:
                continue
            event = decode_packet(packet)
            if event is not None:
                store.add(event)


def recent_rate(events: list[PacketEvent], proto: str, now: float, seconds: float) -> float:
    if seconds <= 0:
        return 0.0
    count = sum(1 for event in events if event.proto == proto and event.received_monotonic >= now - seconds)
    return count / seconds


def render_dashboard(store: PacketStore, host: str, port: int, window_seconds: int, recent_lines: int) -> None:
    plt.style.use("seaborn-v0_8-whitegrid")
    fig = plt.figure(figsize=(14, 8), constrained_layout=True)
    grid = fig.add_gridspec(2, 2, width_ratios=[2.2, 1.5], height_ratios=[1.4, 1.0])
    ax_rate = fig.add_subplot(grid[0, 0])
    ax_arrivals = fig.add_subplot(grid[1, 0], sharex=ax_rate)
    ax_text = fig.add_subplot(grid[:, 1])

    stop_event = threading.Event()
    worker = threading.Thread(target=listener, args=(host, port, stop_event, store), daemon=True)
    worker.start()

    def on_close(_event: object) -> None:
        stop_event.set()

    fig.canvas.mpl_connect("close_event", on_close)

    def update(_frame: int) -> None:
        events, totals, missing = store.snapshot()
        now = time.monotonic()
        visible = [event for event in events if event.received_monotonic >= now - window_seconds]

        ax_rate.clear()
        ax_arrivals.clear()
        ax_text.clear()

        bins = np.arange(-window_seconds, 1, 1)
        for proto, color, label in (("zigbee", "#0f766e", "Zigbee"), ("halow", "#b45309", "HaLow")):
            times = [event.received_monotonic - now for event in visible if event.proto == proto]
            if times:
                hist, edges = np.histogram(times, bins=bins)
            else:
                hist = np.zeros(len(bins) - 1)
                edges = bins
            ax_rate.step(edges[:-1], hist, where="post", color=color, linewidth=2.0, label=label)

        ax_rate.set_title("Packet rate over rolling window")
        ax_rate.set_ylabel("packets / second")
        ax_rate.set_xlim(-window_seconds, 0)
        ax_rate.legend(loc="upper left")

        zigbee_events = [event for event in visible if event.proto == "zigbee"]
        halow_events = [event for event in visible if event.proto == "halow"]
        if zigbee_events:
            ax_arrivals.scatter(
                [event.received_monotonic - now for event in zigbee_events],
                [1.0] * len(zigbee_events),
                color="#0f766e",
                s=28,
                label="Zigbee",
            )
        if halow_events:
            ax_arrivals.scatter(
                [event.received_monotonic - now for event in halow_events],
                [0.0] * len(halow_events),
                color="#b45309",
                s=28,
                label="HaLow",
            )

        ax_arrivals.set_title("Packet arrivals")
        ax_arrivals.set_xlabel("seconds ago")
        ax_arrivals.set_yticks([0.0, 1.0], labels=["HaLow", "Zigbee"])
        ax_arrivals.set_xlim(-window_seconds, 0)
        ax_arrivals.set_ylim(-0.6, 1.6)
        ax_arrivals.legend(loc="upper left")

        latest = {proto: None for proto in ("zigbee", "halow")}
        for event in reversed(events):
            if latest[event.proto] is None:
                latest[event.proto] = event
            if all(latest.values()):
                break

        ax_text.axis("off")
        lines = [
            f"Listening on UDP {host}:{port}",
            f"Window: {window_seconds}s",
            "",
        ]
        for proto, label in (("zigbee", "Zigbee"), ("halow", "HaLow")):
            event = latest[proto]
            age = now - event.received_monotonic if event is not None else float("nan")
            rate_10s = recent_rate(events, proto, now, 10.0)
            lines.append(
                f"{label:7s} total={totals[proto]:5d}  missing~={missing[proto]:4d}  "
                f"rate10={rate_10s:4.2f}/s  last_age={age:5.1f}s"
                if event is not None
                else f"{label:7s} waiting for packets"
            )

        lines.append("")
        lines.append("Recent packets")
        lines.append("------------")
        for event in reversed(events[-recent_lines:]):
            lines.append(event.summary)

        ax_text.text(
            0.0,
            1.0,
            "\n".join(lines),
            va="top",
            ha="left",
            family="monospace",
            fontsize=10.5,
        )

    animation = FuncAnimation(fig, update, interval=250, cache_frame_data=False)
    fig._animation = animation
    plt.show()
    stop_event.set()
    worker.join(timeout=1.0)


def self_test() -> None:
    zigbee = bytes([
        82, 70, 116, 97, 3, 0, 1, 0, 195, 0, 0, 0,
        65, 200, 52, 255, 255, 255, 255, 0, 0, 69, 69, 66, 71, 73, 90,
        52, 27, 0, 0, 90, 68, 29, 87, 133, 1, 0, 0, 0,
    ])
    halow = bytes([
        64, 0, 0, 0, 255, 255, 255, 255, 255, 255, 168, 221, 159, 77, 197, 181,
        255, 255, 255, 255, 255, 255, 0, 0, 0, 13, 62, 27, 0, 0, 72,
        163, 101, 239, 133, 1, 0, 0, 0, 217, 15, 158, 0, 64, 0, 0, 8, 8, 2,
        32, 0, 253, 0, 250, 1, 0,
    ])
    for payload in (zigbee, halow):
        event = decode_packet(payload)
        if event is None:
            raise SystemExit("self-test failed: parser returned no event")
        print(event.summary)


def main() -> None:
    parser = argparse.ArgumentParser(description="Live Zigbee / HaLow packet monitor for the demo bundle")
    parser.add_argument("--host", default="127.0.0.1", help="UDP bind host")
    parser.add_argument("--port", type=int, default=55555, help="UDP port to monitor")
    parser.add_argument("--window", type=int, default=60, help="Rolling rate window in seconds")
    parser.add_argument("--recent", type=int, default=18, help="Number of recent packets shown in the text pane")
    parser.add_argument("--self-test", action="store_true", help="Decode embedded sample packets and exit")
    args = parser.parse_args()

    if args.self_test:
        self_test()
        return

    render_dashboard(PacketStore(), args.host, args.port, args.window, args.recent)


if __name__ == "__main__":
    main()