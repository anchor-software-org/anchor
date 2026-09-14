#!/usr/bin/env python3
"""
Anchor performance dashboard.

Parses /tmp/anchor.log (or any path) and displays a 6-panel metrics
dashboard.  Two modes:

  --live           Tail the log file and refresh every --interval seconds.
  --file PATH      Analyse a complete log file and show static plots.

Understands both:
  • New structured  [METRICS] JSON lines (anchor::metrics target)
  • Legacy plain-text [timing], [perf], [perf-dmabuf], [udp.perf],
    [camera] stats, [input.perf] lines for backward compatibility.

Usage:
  python anchor_stats.py --live
  python anchor_stats.py --file /tmp/anchor.log
  python anchor_stats.py --file myrun.log --out results/run1
"""

import argparse
import json
import os
import re
import sys
import threading
import time
from collections import defaultdict
from dataclasses import dataclass, field
from typing import Optional

import matplotlib
import matplotlib.pyplot as plt
import matplotlib.animation as animation
import numpy as np

matplotlib.rcParams.update({
    "figure.facecolor": "#1a1a2e",
    "axes.facecolor": "#16213e",
    "axes.edgecolor": "#4a4a6a",
    "axes.labelcolor": "#e0e0e0",
    "xtick.color": "#b0b0c0",
    "ytick.color": "#b0b0c0",
    "text.color": "#e0e0e0",
    "grid.color": "#2a2a4a",
    "grid.linestyle": "--",
    "grid.alpha": 0.5,
    "lines.linewidth": 1.4,
    "font.size": 8,
})

DEFAULT_LOG = "/tmp/anchor.log"

# ── Regex patterns for legacy plain-text log lines ────────────────────────────

_RE_LINE = re.compile(r"^(\d+) \[(\w+)\] ([\w:]+): (.+)$")

_RE_TIMING = re.compile(
    r"\[timing\] avg frame=(\d+)ms fps=(\d+) drops=(\d+) total_drops=(\d+)"
)
_RE_PERF = re.compile(
    r"\[perf\] frame=(\d+) \| encode=(\d+)us enc_size=(\d+)B ifi=(\d+)us drops=(\d+)"
)
_RE_DMABUF = re.compile(
    r"\[perf-dmabuf\] frame=(\d+) \| dup=(\d+)us desc=(\d+)us map=(\d+)us "
    r"filt_in=(\d+)us filt_out=(\d+)us send=(\d+)us recv=(\d+)us \| TOTAL=(\d+)us"
)
_RE_UDP = re.compile(
    r"\[udp\.perf\] device=(\S+) frames=(\d+) packets=(\d+) "
    r"avg_packets_per_frame=([\d.]+) avg_frame_bytes=([\d.]+).*?"
    r"slow_frames=(\d+) max_send_us=(\d+)"
)
_RE_CAMERA = re.compile(
    r"\[camera\] stats: decoded=(\d+) written=(\d+) packets=(\d+) "
    r"write_errs=(\d+) pending_pkts=(\d+)"
)
_RE_INPUT = re.compile(
    r"\[input\.perf\] abs=(\d+) rel=(\d+) button=(\d+) axis=(\d+) "
    r"avg_event_us=([\d.]+) max_event_us=(\d+) slow_events=(\d+) max_abs_gap_ms=([\d.]+)"
)

# ── Data store ────────────────────────────────────────────────────────────────

@dataclass
class Series:
    ts: list = field(default_factory=list)   # unix ms timestamps
    vals: dict = field(default_factory=lambda: defaultdict(list))

    def append(self, ts_ms: int, **kwargs):
        self.ts.append(ts_ms / 1000.0)  # convert to seconds for plotting
        for k, v in kwargs.items():
            self.vals[k].append(v)

    def get(self, key, default=None):
        return self.vals.get(key, default or [])

    def tail(self, n: int):
        """Return a view of the last n samples."""
        s = Series()
        s.ts = self.ts[-n:]
        for k, v in self.vals.items():
            s.vals[k] = v[-n:]
        return s

    def __len__(self):
        return len(self.ts)


class DataStore:
    def __init__(self):
        self.timing  = Series()   # t=timing:     fps, avg_frame_ms, drops
        self.frame   = Series()   # t=frame:      encode_us, enc_size_bytes, ifi_us
        self.encode  = Series()   # t=encode:     dup_us … total_us (sub-stages)
        self.camera  = Series()   # t=camera:     decoded, written, write_errs
        self.udp     = Series()   # t=udp:        frames, bytes, slow_frames
        self.input_s = Series()   # t=input:      avg_event_us, slow_events
        self.latency = Series()   # t=frame_trace/wire: cap_us, enc_us, desktop_us, wire_us
        self.rtt     = Series()   # t=rtt:        interval_ms between consecutive phone pings

    def ingest(self, record: dict):
        t = record.get("t", "")
        ts = int(record.get("ts_ms", time.time() * 1000))
        if t == "timing":
            self.timing.append(ts,
                fps=float(record.get("fps", 0)),
                avg_frame_ms=float(record.get("avg_frame_ms", 0)),
                drops=float(record.get("drops", 0)),
                total_drops=float(record.get("total_drops", 0)),
            )
        elif t == "frame":
            self.frame.append(ts,
                encode_us=float(record.get("encode_us", 0)),
                enc_size_bytes=float(record.get("enc_size_bytes", 0)),
                ifi_us=float(record.get("ifi_us", 0)),
                drops=float(record.get("drops", 0)),
            )
        elif t == "encode":
            self.encode.append(ts,
                dup_us=float(record.get("dup_us", 0)),
                desc_us=float(record.get("desc_us", 0)),
                map_us=float(record.get("map_us", 0)),
                filt_in_us=float(record.get("filt_in_us", 0)),
                filt_out_us=float(record.get("filt_out_us", 0)),
                send_us=float(record.get("send_us", 0)),
                recv_us=float(record.get("recv_us", 0)),
                total_us=float(record.get("total_us", 0)),
            )
        elif t == "frame_trace":
            # Per-frame nanosecond trace from TraceRing::to_json_log()
            cap = float(record.get("cap_us", 0))
            enc = float(record.get("enc_us", 0))
            desktop = float(record.get("desktop_us", 0))
            wire = float(record.get("wire_us", 0))
            if cap > 0 or enc > 0 or desktop > 0:
                self.latency.append(ts,
                    cap_us=cap,
                    enc_us=enc,
                    desktop_us=desktop,
                    wire_us=wire,
                    total_us=cap + enc + wire if wire > 0 else cap + desktop,
                )
        elif t == "wire":
            # TCP write timing from drain thread
            self.latency.append(ts,
                cap_us=0.0,
                enc_us=0.0,
                desktop_us=0.0,
                wire_us=float(record.get("avg_write_us", 0)),
                total_us=float(record.get("avg_write_us", 0)),
                max_write_us=float(record.get("max_write_us", 0)),
            )
        elif t == "camera":
            self.camera.append(ts,
                decoded=float(record.get("decoded", 0)),
                written=float(record.get("written", 0)),
                packets=float(record.get("packets", 0)),
                write_errs=float(record.get("write_errs", 0)),
            )
        elif t == "udp":
            self.udp.append(ts,
                frames=float(record.get("frames", 0)),
                bytes_sent=float(record.get("bytes", 0)),
                slow_frames=float(record.get("slow_frames", 0)),
                max_send_us=float(record.get("max_send_us", 0)),
                avg_frame_bytes=float(record.get("avg_frame_bytes", 0)),
            )
        elif t == "input":
            self.input_s.append(ts,
                avg_event_us=float(record.get("avg_event_us", 0)),
                max_event_us=float(record.get("max_event_us", 0)),
                slow_events=float(record.get("slow_events", 0)),
            )
        elif t == "rtt":
            interval = float(record.get("interval_ms", 0))
            if interval > 0:
                self.rtt.append(ts, interval_ms=interval)


# ── Log parser ────────────────────────────────────────────────────────────────

def _parse_line(line: str, store: DataStore):
    m = _RE_LINE.match(line.rstrip())
    if not m:
        return
    ts_ms_str, level, target, msg = m.groups()
    ts_ms = int(ts_ms_str)

    # ── Structured JSON path ──────────────────────────────────────────────────
    if msg.startswith("[METRICS] "):
        raw = msg[len("[METRICS] "):]
        try:
            record = json.loads(raw)
            if "ts_ms" not in record:
                record["ts_ms"] = ts_ms
            store.ingest(record)
        except json.JSONDecodeError:
            pass
        return

    # ── Legacy plain-text path ────────────────────────────────────────────────
    if m2 := _RE_TIMING.search(msg):
        store.ingest({"t": "timing", "ts_ms": ts_ms,
                      "avg_frame_ms": int(m2.group(1)), "fps": int(m2.group(2)),
                      "drops": int(m2.group(3)), "total_drops": int(m2.group(4))})
    elif m2 := _RE_PERF.search(msg):
        store.ingest({"t": "frame", "ts_ms": ts_ms,
                      "frame": int(m2.group(1)), "encode_us": int(m2.group(2)),
                      "enc_size_bytes": int(m2.group(3)), "ifi_us": int(m2.group(4)),
                      "drops": int(m2.group(5))})
    elif m2 := _RE_DMABUF.search(msg):
        store.ingest({"t": "encode", "ts_ms": ts_ms,
                      "frame": int(m2.group(1)), "dup_us": int(m2.group(2)),
                      "desc_us": int(m2.group(3)), "map_us": int(m2.group(4)),
                      "filt_in_us": int(m2.group(5)), "filt_out_us": int(m2.group(6)),
                      "send_us": int(m2.group(7)), "recv_us": int(m2.group(8)),
                      "total_us": int(m2.group(9))})
    elif m2 := _RE_UDP.search(msg):
        store.ingest({"t": "udp", "ts_ms": ts_ms,
                      "device_id": m2.group(1), "frames": int(m2.group(2)),
                      "packets": int(m2.group(3)),
                      "avg_pkts_per_frame": float(m2.group(4)),
                      "avg_frame_bytes": float(m2.group(5)),
                      "slow_frames": int(m2.group(6)), "max_send_us": int(m2.group(7))})
    elif m2 := _RE_CAMERA.search(msg):
        store.ingest({"t": "camera", "ts_ms": ts_ms,
                      "decoded": int(m2.group(1)), "written": int(m2.group(2)),
                      "packets": int(m2.group(3)), "write_errs": int(m2.group(4)),
                      "pending_pkts": int(m2.group(5))})
    elif m2 := _RE_INPUT.search(msg):
        store.ingest({"t": "input", "ts_ms": ts_ms,
                      "abs": int(m2.group(1)), "rel": int(m2.group(2)),
                      "button": int(m2.group(3)), "axis": int(m2.group(4)),
                      "avg_event_us": float(m2.group(5)), "max_event_us": int(m2.group(6)),
                      "slow_events": int(m2.group(7)), "max_abs_gap_ms": float(m2.group(8))})


def parse_file(path: str, store: DataStore):
    with open(path) as f:
        for line in f:
            _parse_line(line, store)


# ── Statistics ────────────────────────────────────────────────────────────────

def _pcts(vals):
    """Return (mean, std, p50, p95, p99, min, max) or all-zero if empty."""
    if not vals:
        return (0.0,) * 7
    a = np.array(vals, dtype=float)
    return (
        float(np.mean(a)), float(np.std(a)),
        float(np.percentile(a, 50)),
        float(np.percentile(a, 95)),
        float(np.percentile(a, 99)),
        float(np.min(a)), float(np.max(a)),
    )


def print_stats(store: DataStore):
    rows = []
    def row(name, vals, unit=""):
        m, s, p50, p95, p99, mn, mx = _pcts(vals)
        flag = " ⚠" if p99 > 2 * p50 and p50 > 0 else ""
        rows.append(f"  {name:<28}  n={len(vals):<5}  mean={m:>8.1f}{unit}  "
                    f"std={s:>7.1f}  p50={p50:>8.1f}  p95={p95:>8.1f}  "
                    f"p99={p99:>8.1f}  max={mx:>8.1f}{flag}")

    print("\n── Anchor Performance Summary ──────────────────────────────────")
    if store.timing.get("fps"):
        row("FPS", store.timing.get("fps"))
        row("Avg frame ms", store.timing.get("avg_frame_ms"), "ms")
    if store.frame.get("encode_us"):
        row("Encode us", store.frame.get("encode_us"), "µs")
        row("IFI us", store.frame.get("ifi_us"), "µs")
        row("Frame size", store.frame.get("enc_size_bytes"), "B")
    if store.encode.get("total_us"):
        row("Encode total_us", store.encode.get("total_us"), "µs")
        row("  ↳ map_us", store.encode.get("map_us"), "µs")
        row("  ↳ filt_out_us (GPU)", store.encode.get("filt_out_us"), "µs")
    if store.camera.get("decoded"):
        row("Camera decoded", store.camera.get("decoded"))
        row("Camera write_errs", store.camera.get("write_errs"))
    if store.udp.get("frames"):
        row("UDP frames/s", store.udp.get("frames"))
        row("UDP slow_frames", store.udp.get("slow_frames"))
        row("UDP max_send_us", store.udp.get("max_send_us"), "µs")
    if store.latency.get("total_us"):
        total_nz = [x for x in store.latency.get("total_us") if x > 0]
        if total_nz:
            row("Pipeline total_us", total_nz, "µs")
        cap_nz = [x for x in store.latency.get("cap_us", []) if x > 0]
        if cap_nz:
            row("  ↳ cap_us (vsync)", cap_nz, "µs")
        enc_nz = [x for x in store.latency.get("enc_us", []) if x > 0]
        if enc_nz:
            row("  ↳ enc_us (encode)", enc_nz, "µs")
        wire_nz = [x for x in store.latency.get("wire_us", []) if x > 0]
        if wire_nz:
            row("  ↳ wire_us (TCP)", wire_nz, "µs")
    if store.rtt.get("interval_ms"):
        vals = np.array([x for x in store.rtt.get("interval_ms") if x > 0])
        if len(vals) >= 2:
            baseline = float(np.percentile(vals, 10))
            dev = np.clip(vals - baseline, 0, None)
            row("Control channel baseline", [baseline], "ms")
            row("Control added delay p95", [float(np.percentile(dev, 95))], "ms")
            row("Control added delay p99", [float(np.percentile(dev, 99))], "ms")
    if store.input_s.get("avg_event_us"):
        row("Input avg_event_us", store.input_s.get("avg_event_us"), "µs")
        row("Input slow_events", store.input_s.get("slow_events"))
    for r in rows:
        print(r)
    print()


# ── Dashboard ─────────────────────────────────────────────────────────────────

COLORS = ["#00d2ff", "#ff6b6b", "#48dbfb", "#ffd32a",
          "#0be881", "#f53b57", "#7efff5", "#ffdd59"]

ENCODE_SUBSTAGES = ["dup_us", "desc_us", "map_us", "filt_in_us", "filt_out_us",
                    "send_us", "recv_us"]
ENCODE_LABELS    = ["dup", "desc", "map", "filt_in", "filt_out(GPU)", "send", "recv"]


def _rolling(vals, window=10):
    if len(vals) < 2:
        return vals
    a = np.array(vals, dtype=float)
    w = min(window, len(a))
    kernel = np.ones(w) / w
    # 'valid' returns len(a)-w+1 elements with no edge artifacts.
    # Prepend w-1 raw values so output length always equals input length.
    valid = np.convolve(a, kernel, mode='valid')
    return np.concatenate([a[: w - 1], valid]).tolist()


class Dashboard:
    TAIL = 300  # show last N samples per panel

    # RTT health thresholds (ms)
    RTT_GOOD = 30
    RTT_WARN = 80

    def __init__(self, store: DataStore):
        self.store = store
        self.fig = plt.figure(figsize=(16, 13))
        gs = self.fig.add_gridspec(4, 2, hspace=0.45, wspace=0.35,
                                   top=0.94, bottom=0.06, left=0.07, right=0.97)
        # 3×2 grid of metric panels
        self.axes = np.array([
            [self.fig.add_subplot(gs[0, 0]), self.fig.add_subplot(gs[0, 1])],
            [self.fig.add_subplot(gs[1, 0]), self.fig.add_subplot(gs[1, 1])],
            [self.fig.add_subplot(gs[2, 0]), self.fig.add_subplot(gs[2, 1])],
        ])
        # Full-width RTT panel at the bottom
        self.ax_rtt = self.fig.add_subplot(gs[3, :])
        self.fig.suptitle("Anchor Performance Dashboard", fontsize=13, fontweight="bold",
                          color="#e0e0e0")
        for ax in self.axes.flat:
            ax.grid(True, alpha=0.3)
        self.ax_rtt.grid(True, alpha=0.3)
        self._init_panels()

    def _init_panels(self):
        titles = [
            "FPS + Frame Drops", "Encode Latency (µs)",
            "Encode Sub-stages (µs)", "UDP Transport",
            "Camera Pipeline", "Pipeline Latency (µs) — capture→encode→wire",
        ]
        for ax, title in zip(self.axes.flat, titles):
            ax.set_title(title, fontsize=9, pad=4)
        self.ax_rtt.set_title(
            "Control channel health — inter-ping interval (ms)  |  baseline = phone's ping period  |  spikes = control starved by video flood",
            fontsize=9, pad=4)

    def update(self):
        for ax in self.axes.flat:
            ax.cla()
            ax.grid(True, alpha=0.3)
        self.ax_rtt.cla()
        self.ax_rtt.grid(True, alpha=0.3)
        self._init_panels()
        s = self.store

        # ── Panel 1: FPS + drops ─────────────────────────────────────────────
        ax = self.axes[0, 0]
        if len(s.timing) > 1:
            d = s.timing.tail(self.TAIL)
            ts = d.ts
            fps = d.get("fps")
            drops = d.get("drops")
            ax.plot(ts, fps, color=COLORS[0], label="FPS")
            ax.plot(ts, _rolling(fps), color=COLORS[0], alpha=0.4, linestyle="--",
                    label="FPS (rolling)")
            ax2 = ax.twinx()
            ax2.bar(ts, drops, color=COLORS[1], alpha=0.5, width=0.8, label="drops")
            ax2.set_ylabel("Frame drops", color=COLORS[1], fontsize=7)
            ax2.tick_params(axis='y', colors=COLORS[1], labelsize=7)
            ax.set_ylabel("FPS", color=COLORS[0], fontsize=7)
            ax.legend(loc="upper left", fontsize=7)
            _add_pct_text(ax, fps, "fps")

        # ── Panel 2: Encode latency + RTT overlay ───────────────────────────
        ax = self.axes[0, 1]
        enc_vals = s.frame.get("encode_us") or s.encode.get("total_us") or []
        ts_enc   = s.frame.ts or s.encode.ts
        if len(enc_vals) > 1:
            tail_n = min(self.TAIL, len(enc_vals))
            enc_t  = ts_enc[-tail_n:]
            enc_v  = enc_vals[-tail_n:]
            ax.plot(enc_t, enc_v, color=COLORS[2], alpha=0.6, label="encode_us")
            ax.plot(enc_t, _rolling(enc_v), color=COLORS[2], linewidth=2,
                    label="rolling avg")
            p95 = float(np.percentile(enc_vals, 95))
            p99 = float(np.percentile(enc_vals, 99))
            ax.axhline(p95, color=COLORS[3], linestyle=":", linewidth=1,
                       label=f"p95={p95:.0f}")
            ax.axhline(p99, color=COLORS[5], linestyle=":", linewidth=1,
                       label=f"p99={p99:.0f}")
            ax.set_ylabel("µs", fontsize=7)
            ax.legend(loc="upper right", fontsize=7)
            _add_pct_text(ax, enc_vals, "µs")

        # ── Panel 3: Encode sub-stages stacked area ──────────────────────────
        ax = self.axes[1, 0]
        if len(s.encode) > 1:
            d = s.encode.tail(self.TAIL)
            ts3 = d.ts
            stages = [d.get(k) for k in ENCODE_SUBSTAGES]
            # Only include non-zero stages
            active = [(lb, v) for lb, v in zip(ENCODE_LABELS, stages) if any(x > 0 for x in v)]
            if active:
                labels, arrs = zip(*active)
                arrs = [np.array(a, dtype=float) for a in arrs]
                ax.stackplot(ts3, *arrs, labels=labels,
                             colors=COLORS[:len(arrs)], alpha=0.7)
                ax.set_ylabel("µs", fontsize=7)
                ax.legend(loc="upper right", fontsize=7, ncol=2)

        # ── Panel 4: UDP transport ────────────────────────────────────────────
        ax = self.axes[1, 1]
        if len(s.udp) > 1:
            d = s.udp.tail(self.TAIL)
            ts4 = d.ts
            frames = d.get("frames")
            slow   = d.get("slow_frames")
            bw     = [b / 1024 / 1024 for b in d.get("bytes_sent")]
            ax.plot(ts4, frames, color=COLORS[0], label="frames/window")
            ax2 = ax.twinx()
            ax2.plot(ts4, bw, color=COLORS[4], alpha=0.7, linestyle="--",
                     label="MB/window")
            ax2.set_ylabel("MB/window", color=COLORS[4], fontsize=7)
            ax2.tick_params(axis='y', colors=COLORS[4], labelsize=7)
            ax.bar(ts4, slow, color=COLORS[5], alpha=0.5, width=0.8,
                   label="slow frames")
            ax.set_ylabel("Frames", color=COLORS[0], fontsize=7)
            ax.legend(loc="upper left", fontsize=7)

        # ── Panel 5: Camera pipeline ──────────────────────────────────────────
        ax = self.axes[2, 0]
        if len(s.camera) > 1:
            d = s.camera.tail(self.TAIL)
            ts5  = d.ts
            dec  = d.get("decoded")
            writ = d.get("written")
            errs = d.get("write_errs")
            ax.plot(ts5, dec,  color=COLORS[0], label="decoded")
            ax.plot(ts5, writ, color=COLORS[4], linestyle="--", label="written")
            ax2 = ax.twinx()
            ax2.bar(ts5, errs, color=COLORS[5], alpha=0.6, width=0.8,
                    label="write_errs")
            ax2.set_ylabel("Write errors", color=COLORS[5], fontsize=7)
            ax2.tick_params(axis='y', colors=COLORS[5], labelsize=7)
            ax.set_ylabel("Frames", fontsize=7)
            ax.legend(loc="upper left", fontsize=7)

        # ── Panel 6: Pipeline latency (cap + enc + wire) ─────────────────────
        ax = self.axes[2, 1]
        if len(s.latency) > 1:
            d = s.latency.tail(self.TAIL)
            ts6   = d.ts
            cap   = d.get("cap_us")
            enc   = d.get("enc_us")
            wire  = d.get("wire_us")
            total = d.get("total_us")

            # Stacked area for breakdown; total as bold line on top
            active_stages, active_labels, active_colors = [], [], []
            for arr, lbl, col in [
                (cap,  "cap (vsync)",  COLORS[3]),
                (enc,  "encode",       COLORS[2]),
                (wire, "wire (TCP)",   COLORS[4]),
            ]:
                if arr and any(x > 0 for x in arr):
                    active_stages.append(np.array(arr, dtype=float))
                    active_labels.append(lbl)
                    active_colors.append(col)

            if active_stages:
                ax.stackplot(ts6, *active_stages, labels=active_labels,
                             colors=active_colors, alpha=0.55)

            if total and any(x > 0 for x in total):
                ax.plot(ts6, total, color="#ffffff", linewidth=1.4,
                        label="total", zorder=5)
                ax.plot(ts6, _rolling(total), color="#ffffff", linewidth=2,
                        alpha=0.35, linestyle="--")
                _add_pct_text(ax, [x for x in total if x > 0], "µs")

            ax.set_ylabel("µs", fontsize=7)
            ax.legend(loc="upper right", fontsize=7)
        elif len(s.input_s) > 1:
            # Fall back to input latency if no pipeline data yet
            d = s.input_s.tail(self.TAIL)
            ts6 = d.ts
            avg = d.get("avg_event_us")
            ax.plot(ts6, avg, color=COLORS[0], label="input avg_event_us")
            ax.set_ylabel("µs", fontsize=7)
            ax.legend(loc="upper left", fontsize=7)
            ax.set_title("Input Latency (µs) — no pipeline data yet", fontsize=9, pad=4)

        # ── Control channel delay panel (full-width bottom) ─────────────────
        # The phone pings every ~T ms. We measure inter-ping arrival time on
        # the desktop. Baseline = p10 of all intervals ≈ T (no-congestion period).
        # Deviation = interval - baseline = extra delay imposed by video flooding.
        # 0 ms = control channel is healthy; spikes = link is saturated.
        ax = self.ax_rtt
        if len(s.rtt) > 1:
            d    = s.rtt.tail(self.TAIL)
            ts_r = d.ts
            vals = np.array(d.get("interval_ms"), dtype=float)
            nz   = vals[vals > 0]

            if len(nz) >= 2:
                # Baseline: p10 of ALL samples so far (not just the tail window).
                all_vals = np.array([x for x in s.rtt.get("interval_ms") if x > 0],
                                    dtype=float)
                baseline = float(np.percentile(all_vals, 10))

                dev  = np.clip(vals - baseline, 0, None)  # added delay, floor at 0
                roll = np.array(_rolling(dev.tolist()), dtype=float)

                # Fixed health bands in ms of added delay
                warn_ms, bad_ms = 100, 400
                ax.axhspan(0,        warn_ms,      alpha=0.10, color="#00ff88", zorder=0)
                ax.axhspan(warn_ms,  bad_ms,       alpha=0.10, color="#ffdd44", zorder=0)
                ax.axhspan(bad_ms,   bad_ms * 10,  alpha=0.10, color="#ff4444", zorder=0)

                ax.plot(ts_r, dev,  color=COLORS[4], linewidth=1.0, alpha=0.55,
                        label="added delay")
                ax.plot(ts_r, roll, color=COLORS[4], linewidth=2.2,
                        label="rolling avg")

                p95_dev = float(np.percentile(dev[dev >= 0], 95)) if len(dev) else 0
                p99_dev = float(np.percentile(dev[dev >= 0], 99)) if len(dev) else 0
                if p95_dev > 0:
                    ax.axhline(p95_dev, color="#ffdd44", linestyle=":", linewidth=1,
                               label=f"p95={p95_dev:.0f}ms")
                if p99_dev > 0:
                    ax.axhline(p99_dev, color="#ff6666", linestyle=":", linewidth=1.2,
                               label=f"p99={p99_dev:.0f}ms")

                spike_idx = int(np.argmax(dev))
                spike_val = float(dev[spike_idx])
                if spike_val > warn_ms:
                    ax.annotate(f"↑{spike_val:.0f}ms",
                                xy=(ts_r[spike_idx], spike_val),
                                xytext=(0, 6), textcoords="offset points",
                                fontsize=7, color="#ff8888", ha="center")

                # Zone labels
                for y, label, col in [(warn_ms * 0.4, "healthy", "#00ff88"),
                                      (warn_ms + (bad_ms - warn_ms) * 0.4, "degraded", "#ffdd44"),
                                      (bad_ms * 1.3, "flooded", "#ff6666")]:
                    ax.text(0.002, y, label, transform=ax.get_yaxis_transform(),
                            fontsize=6.5, color=col, va="center")

                ax.set_title(
                    f"Control channel added delay (ms)  —  baseline ping period: {baseline:.0f}ms"
                    f"  |  0 ms = healthy  |  spikes = link saturated by video",
                    fontsize=9, pad=4)

            ax.set_ylabel("added delay (ms)", fontsize=8)
            ax.set_xlabel("time (s)", fontsize=7)
            ax.legend(loc="upper left", fontsize=7, ncol=5)
        else:
            ax.text(0.5, 0.5, "No data yet — waiting for phone pings",
                    transform=ax.transAxes, ha="center", va="center",
                    fontsize=10, color="#888888")

        self.fig.canvas.draw_idle()

    def save(self, prefix: str):
        png = f"{prefix}.png"
        self.fig.savefig(png, dpi=150, bbox_inches="tight",
                         facecolor=self.fig.get_facecolor())
        print(f"Saved plot → {png}")
        _save_csv(self.store, prefix)
        _save_report(self.store, prefix)


def _add_pct_text(ax, vals, unit):
    if not vals:
        return
    m, s, p50, p95, p99, mn, mx = _pcts(vals)
    txt = f"p50={p50:.0f}{unit}  p95={p95:.0f}  p99={p99:.0f}"
    ax.text(0.01, 0.97, txt, transform=ax.transAxes, fontsize=6.5,
            verticalalignment='top', color="#b0ffb0",
            bbox={"facecolor": "#1a1a2e", "alpha": 0.6, "edgecolor": "none", "pad": 2})


def _save_csv(store: DataStore, prefix: str):
    rows = []
    for i, ts in enumerate(store.timing.ts):
        rows.append({
            "ts": ts,
            "fps": store.timing.get("fps")[i] if i < len(store.timing.get("fps", [])) else "",
            "avg_frame_ms": store.timing.get("avg_frame_ms")[i] if i < len(store.timing.get("avg_frame_ms", [])) else "",
            "drops": store.timing.get("drops")[i] if i < len(store.timing.get("drops", [])) else "",
        })
    if rows:
        import csv
        path = f"{prefix}_timing.csv"
        with open(path, "w", newline="") as f:
            w = csv.DictWriter(f, fieldnames=rows[0].keys())
            w.writeheader()
            w.writerows(rows)
        print(f"Saved CSV  → {path}")

    enc_vals = store.frame.get("encode_us") or store.encode.get("total_us") or []
    if enc_vals:
        import csv
        path = f"{prefix}_encode.csv"
        ts_list = store.frame.ts or store.encode.ts
        with open(path, "w", newline="") as f:
            w = csv.writer(f)
            w.writerow(["ts", "encode_us"])
            w.writerows(zip(ts_list, enc_vals))
        print(f"Saved CSV  → {path}")

    rtt_vals = store.rtt.get("interval_ms") or []
    if rtt_vals:
        import csv
        path = f"{prefix}_rtt.csv"
        with open(path, "w", newline="") as f:
            w = csv.writer(f)
            w.writerow(["ts", "interval_ms"])
            w.writerows(zip(store.rtt.ts, rtt_vals))
        print(f"Saved CSV  → {path}")


def _save_report(store: DataStore, prefix: str):
    path = f"{prefix}_report.md"
    lines = ["# Anchor Performance Report\n"]

    def section(name, series, keys):
        if not series.ts:
            return
        lines.append(f"\n## {name}\n")
        lines.append("| Metric | N | Mean | Std | p50 | p95 | p99 | Max |")
        lines.append("|--------|---|------|-----|-----|-----|-----|-----|")
        for k in keys:
            vals = series.get(k)
            if not vals:
                continue
            m, s, p50, p95, p99, mn, mx = _pcts(vals)
            flag = " ⚠" if p99 > 2 * p50 and p50 > 0 else ""
            lines.append(
                f"| {k}{flag} | {len(vals)} | {m:.1f} | {s:.1f} | "
                f"{p50:.1f} | {p95:.1f} | {p99:.1f} | {mx:.1f} |"
            )

    section("Frame Pipeline", store.timing, ["fps", "avg_frame_ms", "drops"])
    section("Encode Latency", store.frame, ["encode_us", "ifi_us", "enc_size_bytes"])
    section("Encode Sub-stages", store.encode,
            ["total_us", "map_us", "filt_in_us", "filt_out_us", "send_us", "recv_us"])
    section("UDP Transport", store.udp, ["frames", "bytes_sent", "slow_frames", "max_send_us"])
    section("Camera Pipeline", store.camera, ["decoded", "written", "write_errs"])
    section("Input Events", store.input_s, ["avg_event_us", "max_event_us", "slow_events"])

    with open(path, "w") as f:
        f.write("\n".join(lines) + "\n")
    print(f"Saved report→ {path}")


# ── Live tail reader ──────────────────────────────────────────────────────────

class LogTailer:
    def __init__(self, path: str, store: DataStore):
        self.path  = path
        self.store = store
        self._lock = threading.Lock()
        self._fh: Optional[object] = None
        self._stop = threading.Event()

    def _open(self):
        while not self._stop.is_set():
            try:
                self._fh = open(self.path)
                self._fh.seek(0, 2)   # seek to end for live mode
                return True
            except FileNotFoundError:
                print(f"Waiting for {self.path}…", end="\r")
                time.sleep(1)
        return False

    def start(self):
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def _run(self):
        if not self._open():
            return
        while not self._stop.is_set():
            line = self._fh.readline()
            if line:
                with self._lock:
                    _parse_line(line, self.store)
            else:
                time.sleep(0.1)

    def stop(self):
        self._stop.set()


# ── Main ──────────────────────────────────────────────────────────────────────

def main():
    ap = argparse.ArgumentParser(description="Anchor performance dashboard")
    ap.add_argument("--live",  action="store_true",
                    help="Tail the log file and update dashboard in real time")
    ap.add_argument("--file",  default=DEFAULT_LOG, metavar="PATH",
                    help=f"Log file path (default: {DEFAULT_LOG})")
    ap.add_argument("--interval", type=float, default=2.0,
                    help="Live refresh interval in seconds (default: 2)")
    ap.add_argument("--out", default="anchor_stats", metavar="PREFIX",
                    help="Output file prefix for PNG/CSV/report (default: anchor_stats)")
    args = ap.parse_args()

    store = DataStore()

    if args.live:
        print(f"Live mode — tailing {args.file}  (Ctrl+C to quit and save)")
        tailer = LogTailer(args.file, store)
        tailer.start()

        dash = Dashboard(store)

        def _update(_frame):
            dash.update()

        ani = animation.FuncAnimation(
            dash.fig, _update,
            interval=int(args.interval * 1000),
            cache_frame_data=False,
        )

        def _on_close(_event):
            tailer.stop()
            print_stats(store)
            dash.save(args.out)

        dash.fig.canvas.mpl_connect("close_event", _on_close)

        try:
            plt.show()
        except KeyboardInterrupt:
            pass
        finally:
            tailer.stop()
            print_stats(store)
            dash.save(args.out)

    else:
        if not os.path.exists(args.file):
            sys.exit(f"File not found: {args.file}")
        print(f"Parsing {args.file}…")
        parse_file(args.file, store)
        total = (len(store.timing) + len(store.frame) + len(store.encode)
                 + len(store.camera) + len(store.udp) + len(store.input_s))
        print(f"Loaded {total} metric samples.")
        if total == 0:
            print("No metrics found — is the log from a recent session with structured logging?")

        print_stats(store)
        dash = Dashboard(store)
        dash.update()

        def _on_close(_event):
            dash.save(args.out)

        dash.fig.canvas.mpl_connect("close_event", _on_close)
        plt.show()
        dash.save(args.out)


if __name__ == "__main__":
    main()
