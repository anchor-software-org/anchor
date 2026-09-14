#!/usr/bin/env python3
"""
Anchor performance analyzer — parses anchor.log, produces stats + plots + markdown report.

Usage:
    python3 scripts/analyze_perf.py [/path/to/anchor.log]
    python3 scripts/analyze_perf.py --live [/path/to/anchor.log]

Outputs:
    /tmp/anchor_perf_<timestamp>.png   — performance plots
    /tmp/anchor_perf_<timestamp>.md    — markdown report with stats + image
"""

import re
import sys
import os
import argparse
import statistics
from datetime import datetime

# ── Parse ────────────────────────────────────────────────────────────────

def find_last_run(lines):
    last = 0
    for i, line in enumerate(lines):
        if "Settings:" in line and "bitrate=" in line:
            last = i
    return last

def parse_settings(lines):
    for line in lines:
        # New format with all fields
        m = re.search(
            r'Settings: bitrate=(\d+)bps fps=(\d+) gop=(\d+) max_b=(\d+) '
            r'rc=(\w+) low_power=(\w+) preset=(\w+) tune=(\w+) '
            r'hw_pool=(\d+) frame_buf=(\d+) stats_interval=(\d+)', line)
        if m:
            return {
                'bitrate_bps': int(m.group(1)),
                'fps': int(m.group(2)),
                'gop': int(m.group(3)),
                'max_b_frames': int(m.group(4)),
                'rate_control': m.group(5),
                'low_power': m.group(6) == 'true',
                'x264_preset': m.group(7),
                'x264_tune': m.group(8),
                'hw_pool_size': int(m.group(9)),
                'frame_buffer_size': int(m.group(10)),
                'stats_interval': int(m.group(11)),
            }
        # Old format fallback
        m = re.search(r'Settings: bitrate=(\d+)bps fps=(\d+) gop=(\d+) rc=(\w+) low_power=(\w+)', line)
        if m:
            return {
                'bitrate_bps': int(m.group(1)),
                'fps': int(m.group(2)),
                'gop': int(m.group(3)),
                'rate_control': m.group(4),
                'low_power': m.group(5) == 'true',
            }
    return {}

def parse_dmabuf_perf(lines):
    frames = []
    for line in lines:
        m = re.search(
            r'\[perf-dmabuf\] frame=(\d+) \| '
            r'dup=(\d+)us desc=(\d+)us map=(\d+)us '
            r'filt_in=(\d+)us filt_out=(\d+)us '
            r'send=(\d+)us recv=(\d+)us \| '
            r'TOTAL=(\d+)us packets=(\d+)', line)
        if m:
            frames.append({
                'frame': int(m.group(1)),
                'dup': int(m.group(2)),
                'desc': int(m.group(3)),
                'map': int(m.group(4)),
                'filt_in': int(m.group(5)),
                'filt_out': int(m.group(6)),
                'send': int(m.group(7)),
                'recv': int(m.group(8)),
                'total': int(m.group(9)),
                'packets': int(m.group(10)),
            })
    return frames

def parse_frame_perf(lines):
    frames = []
    for line in lines:
        # New format with ifi, drops, keyframe
        m = re.search(
            r'\[perf\] frame=(\d+) \| '
            r'encode=(\d+)us enc_size=(\d+)B ifi=(\d+)us drops=(\d+) keyframe=(\w+)', line)
        if m:
            frames.append({
                'frame': int(m.group(1)),
                'encode_us': int(m.group(2)),
                'enc_size': int(m.group(3)),
                'ifi_us': int(m.group(4)),
                'drops': int(m.group(5)),
                'keyframe': m.group(6) == 'true',
            })
            continue
        # Old format fallback
        m = re.search(
            r'\[perf\] frame=(\d+) \| '
            r'create=(\d+)us dispatch=(\d+)us map=(\d+)us '
            r'preview=(\d+)us encode=(\d+)us \| '
            r'enc_size=(\d+)B', line)
        if m:
            frames.append({
                'frame': int(m.group(1)),
                'encode_us': int(m.group(6)),
                'enc_size': int(m.group(7)),
                'ifi_us': 0,
                'drops': 0,
                'keyframe': False,
            })
    return frames

def parse_timing(lines):
    entries = []
    for line in lines:
        m = re.search(r'\[timing\] avg frame=(\d+)ms fps=(\d+)', line)
        if m:
            entries.append({'avg_frame_ms': int(m.group(1)), 'fps': int(m.group(2))})
    return entries

# ── Stats helpers ────────────────────────────────────────────────────────

def stat_line(values, unit="us"):
    if not values:
        return "no data"
    v = sorted(values)
    n = len(v)
    return (f"min={v[0]}{unit} max={v[-1]}{unit} avg={statistics.mean(v):.0f}{unit} "
            f"p50={v[n//2]}{unit} p90={v[int(n*0.9)]}{unit} p99={v[int(n*0.99)]}{unit}")

# ── Plot ─────────────────────────────────────────────────────────────────

def plot(settings, dmabuf_frames, frame_perf, timing, out_path):
    try:
        import matplotlib.pyplot as plt
        import matplotlib
        matplotlib.use('Agg')
    except ImportError:
        print("[!] matplotlib not installed — pip3 install matplotlib")
        return False

    fig, axes = plt.subplots(3, 3, figsize=(18, 13))
    bitrate_mbps = settings.get('bitrate_bps', 0) / 1e6
    fig.suptitle(
        f"Anchor Encode Performance\n"
        f"bitrate={bitrate_mbps:.0f}Mbps  fps={settings.get('fps', '?')}  "
        f"gop={settings.get('gop', '?')}  rc={settings.get('rate_control', '?')}  "
        f"low_power={settings.get('low_power', '?')}",
        fontsize=11)

    # 1. DMA-BUF encode time distribution
    if dmabuf_frames:
        totals = [f['total'] for f in dmabuf_frames]
        ax = axes[0][0]
        ax.hist(totals, bins=50, color='#42A5F5', edgecolor='#1565C0')
        ax.set_xlabel('Encode time (us)')
        ax.set_ylabel('Count')
        ax.set_title(f'1. DMA-BUF Encode Time (avg={statistics.mean(totals):.0f}us)')
        ax.axvline(statistics.mean(totals), color='red', linestyle='--', label='avg')
        ax.legend()

    # 2. Inter-frame interval (jitter)
    ifi_data = [f['ifi_us'] / 1000 for f in frame_perf if f.get('ifi_us', 0) > 0]
    if ifi_data:
        ax = axes[0][1]
        ax.hist(ifi_data, bins=50, color='#66BB6A', edgecolor='#2E7D32')
        ax.set_xlabel('Inter-frame interval (ms)')
        ax.set_ylabel('Count')
        target_ms = 1000 / max(settings.get('fps', 60), 1)
        ax.axvline(target_ms, color='red', linestyle='--', label=f'target={target_ms:.1f}ms')
        ax.set_title(f'2. Frame Jitter (avg={statistics.mean(ifi_data):.1f}ms)')
        ax.legend()

    # 3. Frame size over time with keyframe markers
    if frame_perf:
        ax = axes[0][2]
        sizes_kb = [f['enc_size'] / 1024 for f in frame_perf]
        keyframe_idx = [i for i, f in enumerate(frame_perf) if f.get('keyframe')]
        keyframe_sizes = [frame_perf[i]['enc_size'] / 1024 for i in keyframe_idx]
        ax.plot(sizes_kb, color='#26A69A', linewidth=0.5, label='P-frame')
        if keyframe_idx:
            ax.scatter(keyframe_idx, keyframe_sizes, color='#EF5350', s=15, zorder=5, label='IDR')
        ax.set_xlabel('Sample')
        ax.set_ylabel('Frame size (KB)')
        ax.set_title(f'3. Frame Size (avg={statistics.mean(sizes_kb):.1f}KB)')
        ax.axhline(statistics.mean(sizes_kb), color='red', linestyle='--', alpha=0.3)
        ax.legend(fontsize=8)

    # 4. FPS over time
    if timing:
        ax = axes[1][0]
        fps_vals = [t['fps'] for t in timing]
        ax.plot(fps_vals, color='#42A5F5', linewidth=1)
        ax.set_xlabel('Second')
        ax.set_ylabel('FPS')
        ax.set_title(f'4. Framerate (avg={statistics.mean(fps_vals):.0f} fps)')
        ax.set_ylim(0, max(fps_vals) + 10)
        ax.axhline(statistics.mean(fps_vals), color='red', linestyle='--', alpha=0.5)

    # 5. DMA-BUF pipeline breakdown (stacked bar)
    if dmabuf_frames:
        ax = axes[1][1]
        stages = ['dup', 'desc', 'map', 'filt_in', 'filt_out', 'send', 'recv']
        colors = ['#EF5350', '#FF7043', '#FFA726', '#66BB6A', '#42A5F5', '#5C6BC0', '#AB47BC']
        bottom = [0] * len(dmabuf_frames)
        for stage, color in zip(stages, colors):
            vals = [f[stage] for f in dmabuf_frames]
            ax.bar(range(len(dmabuf_frames)), vals, bottom=bottom, color=color, label=stage, width=1.0)
            bottom = [b + v for b, v in zip(bottom, vals)]
        ax.set_xlabel('Sample')
        ax.set_ylabel('Time (us)')
        ax.set_title('5. Pipeline Breakdown')
        ax.legend(fontsize=7, loc='upper right')

    # 6. Frame drops over time
    if frame_perf:
        ax = axes[1][2]
        drops = [f.get('drops', 0) for f in frame_perf]
        if any(d > 0 for d in drops):
            ax.bar(range(len(drops)), drops, color='#EF5350', width=1.0)
            ax.set_title(f'6. Frame Drops (total={sum(drops)})')
        else:
            ax.text(0.5, 0.5, 'No drops', ha='center', va='center', transform=ax.transAxes,
                    fontsize=14, color='#66BB6A')
            ax.set_title('6. Frame Drops (0)')
        ax.set_xlabel('Sample')
        ax.set_ylabel('Drops')

    # 7. Encode time over time (stability)
    if frame_perf:
        ax = axes[2][0]
        enc_times = [f['encode_us'] for f in frame_perf]
        ax.plot(enc_times, color='#5C6BC0', linewidth=0.5)
        ax.set_xlabel('Sample')
        ax.set_ylabel('Encode time (us)')
        ax.set_title(f'7. Encode Time Stability (avg={statistics.mean(enc_times):.0f}us)')
        ax.axhline(statistics.mean(enc_times), color='red', linestyle='--', alpha=0.5)

    # Hide unused subplots
    axes[2][1].set_visible(False)
    axes[2][2].set_visible(False)

    plt.tight_layout()
    plt.savefig(out_path, dpi=150)
    print(f"[+] Plot: {out_path}")
    return True

# ── Markdown report ──────────────────────────────────────────────────────

def write_report(settings, dmabuf_frames, frame_perf, timing, img_path, md_path):
    lines = []
    ts = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    lines.append(f"# Anchor Performance Report — {ts}\n")

    # Settings
    lines.append("## Settings\n")
    if settings:
        lines.append("| Parameter | Value |")
        lines.append("|-----------|-------|")
        display_order = [
            ('bitrate_bps', lambda v: f"{v/1e6:.0f} Mbps"),
            ('fps', str), ('gop', str), ('max_b_frames', str),
            ('rate_control', str), ('low_power', str),
            ('x264_preset', str), ('x264_tune', str),
            ('hw_pool_size', str), ('frame_buffer_size', str),
            ('stats_interval', str),
        ]
        for key, fmt in display_order:
            if key in settings:
                lines.append(f"| {key} | {fmt(settings[key])} |")
        lines.append("")
    else:
        lines.append("*Settings not found in log*\n")

    # DMA-BUF
    lines.append("## DMA-BUF Encode\n")
    if dmabuf_frames:
        lines.append(f"**{len(dmabuf_frames)} samples**\n")
        lines.append("| Stage | Stats |")
        lines.append("|-------|-------|")
        lines.append(f"| total | {stat_line([f['total'] for f in dmabuf_frames])} |")
        lines.append(f"| map (import) | {stat_line([f['map'] for f in dmabuf_frames])} |")
        lines.append(f"| filt_out (scale) | {stat_line([f['filt_out'] for f in dmabuf_frames])} |")
        lines.append(f"| send (submit) | {stat_line([f['send'] for f in dmabuf_frames])} |")
        lines.append(f"| recv (drain) | {stat_line([f['recv'] for f in dmabuf_frames])} |")
        lines.append("")
    else:
        lines.append("*No DMA-BUF data*\n")

    # Frame encode
    lines.append("## Frame Encode\n")
    if frame_perf:
        sizes = [f['enc_size'] for f in frame_perf]
        lines.append(f"**{len(frame_perf)} samples**\n")
        lines.append(f"- Encode time: {stat_line([f['encode_us'] for f in frame_perf])}")
        lines.append(f"- Frame size: {stat_line(sizes, 'B')}")
        lines.append(f"- Avg frame: {statistics.mean(sizes)/1024:.1f}KB  Total: {sum(sizes)/1024/1024:.1f}MB")
        lines.append("")
    else:
        lines.append("*No frame encode data*\n")

    # FPS
    lines.append("## Framerate\n")
    if timing:
        fps_vals = [t['fps'] for t in timing]
        lines.append(f"- avg={statistics.mean(fps_vals):.0f}fps  min={min(fps_vals)}  max={max(fps_vals)}")
        lines.append(f"- {len(timing)} samples over ~{len(timing)}s")
        lines.append("")
    else:
        lines.append("*No timing data*\n")

    # Image
    lines.append("## Plots\n")
    lines.append(f"![Performance plots]({img_path})\n")

    with open(md_path, 'w') as f:
        f.write('\n'.join(lines))
    print(f"[+] Report: {md_path}")

# ── CSV ──────────────────────────────────────────────────────────────────

def write_csv(dmabuf_frames, frame_perf, timing, csv_path):
    import csv
    with open(csv_path, 'w', newline='') as f:
        w = csv.writer(f)
        w.writerow(['frame', 'encode_us', 'enc_size_B', 'ifi_us', 'drops', 'keyframe',
                     'dmabuf_total_us', 'dmabuf_map_us', 'dmabuf_filt_out_us', 'dmabuf_send_us',
                     'fps'])

        # Merge data by sample index (dmabuf and frame_perf log at the same interval)
        max_len = max(len(dmabuf_frames), len(frame_perf), len(timing))
        for i in range(max_len):
            fp = frame_perf[i] if i < len(frame_perf) else {}
            db = dmabuf_frames[i] if i < len(dmabuf_frames) else {}
            tm = timing[i] if i < len(timing) else {}
            w.writerow([
                fp.get('frame', ''),
                fp.get('encode_us', ''),
                fp.get('enc_size', ''),
                fp.get('ifi_us', ''),
                fp.get('drops', ''),
                fp.get('keyframe', ''),
                db.get('total', ''),
                db.get('map', ''),
                db.get('filt_out', ''),
                db.get('send', ''),
                tm.get('fps', ''),
            ])
    print(f"[+] CSV: {csv_path}")

# ── Live mode ───────────────────────────────────────────────────────────

LIVE_TAIL = 300  # max samples shown on time-series live graphs

def live_mode(path, refresh_s=1.0):
    import matplotlib
    matplotlib.use('TkAgg')
    import matplotlib.pyplot as plt

    plt.ion()
    fig, axes = plt.subplots(3, 3, figsize=(18, 13))
    fig.suptitle("Anchor Live Performance", fontsize=12)
    fig.canvas.manager.set_window_title("Anchor Live")

    prev_size = 0
    prev_lines_count = 0

    def read_lines():
        nonlocal prev_size, prev_lines_count
        try:
            cur_size = os.path.getsize(path)
        except OSError:
            return None
        if cur_size == prev_size:
            return None
        prev_size = cur_size
        with open(path, 'r', errors='replace') as f:
            all_lines = f.readlines()
        if len(all_lines) == prev_lines_count:
            return None
        prev_lines_count = len(all_lines)
        start = find_last_run(all_lines)
        return all_lines[start:]

    def update(lines):
        settings = parse_settings(lines)
        dmabuf_frames = parse_dmabuf_perf(lines)
        frame_perf = parse_frame_perf(lines)
        timing_data = parse_timing(lines)

        for row in axes:
            for ax in row:
                ax.clear()

        # 1. DMA-BUF encode time distribution
        ax = axes[0][0]
        if dmabuf_frames:
            totals = [f['total'] for f in dmabuf_frames]
            ax.hist(totals, bins=50, color='#42A5F5', edgecolor='#1565C0')
            ax.set_xlabel('Encode time (us)')
            ax.set_ylabel('Count')
            avg = statistics.mean(totals)
            ax.set_title(f'1. DMA-BUF Encode Time (avg={avg:.0f}us)')
            ax.axvline(avg, color='red', linestyle='--', label='avg')
            ax.legend()
        else:
            ax.set_title('1. DMA-BUF Encode Time (waiting...)')

        # 2. Inter-frame interval (jitter)
        ax = axes[0][1]
        ifi_data = [f['ifi_us'] / 1000 for f in frame_perf if f.get('ifi_us', 0) > 0]
        if ifi_data:
            ax.hist(ifi_data, bins=50, color='#66BB6A', edgecolor='#2E7D32')
            ax.set_xlabel('Inter-frame interval (ms)')
            ax.set_ylabel('Count')
            target_ms = 1000 / max(settings.get('fps', 60), 1)
            ax.axvline(target_ms, color='red', linestyle='--', label=f'target={target_ms:.1f}ms')
            ax.set_title(f'2. Frame Jitter (avg={statistics.mean(ifi_data):.1f}ms)')
            ax.legend()
        else:
            ax.set_title('2. Frame Jitter (waiting...)')

        # 3. Frame size over time with keyframe markers
        ax = axes[0][2]
        if frame_perf:
            tail = frame_perf[-LIVE_TAIL:]
            sizes_kb = [f['enc_size'] / 1024 for f in tail]
            keyframe_idx = [i for i, f in enumerate(tail) if f.get('keyframe')]
            keyframe_sizes = [tail[i]['enc_size'] / 1024 for i in keyframe_idx]
            ax.plot(sizes_kb, color='#26A69A', linewidth=0.5, label='P-frame')
            if keyframe_idx:
                ax.scatter(keyframe_idx, keyframe_sizes, color='#EF5350', s=15, zorder=5, label='IDR')
            avg_kb = statistics.mean(sizes_kb)
            ax.axhline(avg_kb, color='red', linestyle='--', alpha=0.3)
            ax.set_xlabel('Sample')
            ax.set_ylabel('Frame size (KB)')
            ax.set_title(f'3. Frame Size (avg={avg_kb:.1f}KB)')
            ax.legend(fontsize=8)
        else:
            ax.set_title('3. Frame Size (waiting...)')

        # 4. FPS over time
        ax = axes[1][0]
        if timing_data:
            tail = timing_data[-LIVE_TAIL:]
            fps_vals = [t['fps'] for t in tail]
            ax.plot(fps_vals, color='#42A5F5', linewidth=1)
            avg_fps = statistics.mean(fps_vals)
            ax.axhline(avg_fps, color='red', linestyle='--', alpha=0.5)
            ax.set_ylim(0, max(fps_vals) + 10)
            ax.set_xlabel('Second')
            ax.set_ylabel('FPS')
            ax.set_title(f'4. Framerate (avg={avg_fps:.0f} fps)')
        else:
            ax.set_title('4. Framerate (waiting...)')

        # 5. DMA-BUF pipeline breakdown (stacked bar)
        ax = axes[1][1]
        if dmabuf_frames:
            tail = dmabuf_frames[-LIVE_TAIL:]
            stages = ['dup', 'desc', 'map', 'filt_in', 'filt_out', 'send', 'recv']
            colors = ['#EF5350', '#FF7043', '#FFA726', '#66BB6A', '#42A5F5', '#5C6BC0', '#AB47BC']
            bottom = [0] * len(tail)
            for stage, color in zip(stages, colors):
                vals = [f[stage] for f in tail]
                ax.bar(range(len(tail)), vals, bottom=bottom, color=color, label=stage, width=1.0)
                bottom = [b + v for b, v in zip(bottom, vals)]
            ax.set_xlabel('Sample')
            ax.set_ylabel('Time (us)')
            ax.set_title('5. Pipeline Breakdown')
            ax.legend(fontsize=7, loc='upper right')
        else:
            ax.set_title('5. Pipeline Breakdown (waiting...)')

        # 6. Frame drops over time
        ax = axes[1][2]
        if frame_perf:
            tail = frame_perf[-LIVE_TAIL:]
            drops = [f.get('drops', 0) for f in tail]
            if any(d > 0 for d in drops):
                ax.bar(range(len(drops)), drops, color='#EF5350', width=1.0)
                ax.set_title(f'6. Frame Drops (total={sum(f.get("drops", 0) for f in frame_perf)})')
            else:
                ax.text(0.5, 0.5, 'No drops', ha='center', va='center', transform=ax.transAxes,
                        fontsize=14, color='#66BB6A')
                ax.set_title('6. Frame Drops (0)')
            ax.set_xlabel('Sample')
            ax.set_ylabel('Drops')
        else:
            ax.set_title('6. Frame Drops (waiting...)')

        # 7. Encode time over time (stability)
        ax = axes[2][0]
        if frame_perf:
            tail = frame_perf[-LIVE_TAIL:]
            enc_times = [f['encode_us'] for f in tail]
            ax.plot(enc_times, color='#5C6BC0', linewidth=0.5)
            ax.set_xlabel('Sample')
            ax.set_ylabel('Encode time (us)')
            avg_enc = statistics.mean(enc_times)
            ax.axhline(avg_enc, color='red', linestyle='--', alpha=0.5)
            ax.set_title(f'7. Encode Time Stability (avg={avg_enc:.0f}us)')
        else:
            ax.set_title('7. Encode Time (waiting...)')

        # 8. Live stats panel
        ax = axes[2][1]
        ax.axis('off')
        stat_lines = []
        if settings:
            stat_lines.append(f"Bitrate:  {settings.get('bitrate_bps', 0)/1e6:.0f} Mbps")
            stat_lines.append(f"Target:   {settings.get('fps', '?')} fps")
            stat_lines.append(f"GOP:      {settings.get('gop', '?')}")
            stat_lines.append(f"RC:       {settings.get('rate_control', '?')}")
            stat_lines.append(f"LowPower: {settings.get('low_power', '?')}")
            stat_lines.append("")
        if dmabuf_frames:
            all_totals = [f['total'] for f in dmabuf_frames]
            st = sorted(all_totals)
            n = len(st)
            stat_lines.append(f"Encode p50={st[n//2]}us  p99={st[int(n*0.99)]}us")
            stat_lines.append("")
            stat_lines.append("Stage Breakdown (avg / %):")
            avg_total = statistics.mean(all_totals) if all_totals else 1
            for key, label in [('map','map'), ('filt_out','color'), ('send','encode'), ('recv','drain')]:
                vals = [f[key] for f in dmabuf_frames]
                avg = statistics.mean(vals)
                pct = avg / avg_total * 100
                stat_lines.append(f"  {label:6s} {avg:6.0f}us ({pct:4.1f}%)")
        if frame_perf:
            stat_lines.append("")
            total_drops = sum(f.get('drops', 0) for f in frame_perf)
            stat_lines.append(f"Frames: {len(frame_perf)}   Drops: {total_drops}")
            kf = sum(1 for f in frame_perf if f.get('keyframe'))
            stat_lines.append(f"Keyframes: {kf}")
        if timing_data:
            stat_lines.append(f"Duration: ~{len(timing_data)}s")
        ax.text(0.05, 0.95, '\n'.join(stat_lines), transform=ax.transAxes,
                fontsize=10, verticalalignment='top', fontfamily='monospace',
                bbox=dict(boxstyle='round', facecolor='#f5f5f5', alpha=0.8))
        ax.set_title('8. Live Stats')

        # Hide unused subplot
        axes[2][2].set_visible(False)

        fig.tight_layout()
        fig.canvas.draw_idle()
        fig.canvas.flush_events()

    print(f"[live] Watching {path}  (refresh={refresh_s}s, Ctrl-C to stop)")
    try:
        while plt.fignum_exists(fig.number):
            lines = read_lines()
            if lines is not None:
                update(lines)
            plt.pause(refresh_s)
    except KeyboardInterrupt:
        pass
    print("\n[live] Stopped.")

# ── Main ─────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="Anchor performance analyzer")
    parser.add_argument('logfile', nargs='?', default='/tmp/anchor.log', help='Path to anchor.log')
    parser.add_argument('--live', action='store_true', help='Live-tail mode with updating graphs')
    parser.add_argument('--refresh', type=float, default=1.0, help='Live refresh interval in seconds (default: 1.0)')
    args = parser.parse_args()

    path = args.logfile

    if args.live:
        live_mode(path, refresh_s=args.refresh)
        return

    try:
        with open(path, 'r', errors='replace') as f:
            all_lines = f.readlines()
    except FileNotFoundError:
        print(f"Log file not found: {path}")
        print("Usage: python3 analyze_perf.py [/path/to/anchor.log]")
        return

    if not all_lines:
        print("Empty log.")
        return

    start = find_last_run(all_lines)
    lines = all_lines[start:]
    print(f"Analyzing {len(lines)} lines from last run (of {len(all_lines)} total)")

    settings = parse_settings(lines)
    dmabuf_frames = parse_dmabuf_perf(lines)
    frame_perf = parse_frame_perf(lines)
    timing = parse_timing(lines)

    # Timestamped output
    ts = datetime.now().strftime("%Y%m%d_%H%M%S")
    img_path = f"/tmp/anchor_perf_{ts}.png"
    md_path = f"/tmp/anchor_perf_{ts}.md"

    # Print summary to terminal
    print(f"\n{'='*60}")
    print(f"  ANCHOR PERFORMANCE SUMMARY")
    print(f"{'='*60}")
    if settings:
        print(f"  Bitrate:      {settings.get('bitrate_bps', 0)/1e6:.0f} Mbps")
        print(f"  FPS target:   {settings.get('fps', '?')}")
        print(f"  GOP:          {settings.get('gop', '?')}")
        print(f"  Rate control: {settings.get('rate_control', '?')}")
        print(f"  Low power:    {settings.get('low_power', '?')}")
    print(f"{'─'*60}")
    if dmabuf_frames:
        totals = [f['total'] for f in dmabuf_frames]
        print(f"  Encode:  avg={statistics.mean(totals):.0f}us  p99={sorted(totals)[int(len(totals)*0.99)]}us  ({len(dmabuf_frames)} samples)")
        print(f"{'─'*60}")
        print(f"  Pipeline Stage Breakdown:")
        stages = [
            ('dup',      'fd dup      '),
            ('desc',     'descriptor  '),
            ('map',      'DMA-BUF map '),
            ('filt_in',  'filter push '),
            ('filt_out', 'color conv  '),
            ('send',     'GPU encode  '),
            ('recv',     'packet drain'),
        ]
        for key, label in stages:
            vals = sorted([f[key] for f in dmabuf_frames])
            n = len(vals)
            if n > 0:
                pct = statistics.mean(vals) / statistics.mean(totals) * 100
                print(f"    {label}  avg={statistics.mean(vals):6.0f}us  "
                      f"min={vals[0]:5}us  max={vals[-1]:5}us  "
                      f"p50={vals[n//2]:5}us  p99={vals[int(n*0.99)]:5}us  "
                      f"({pct:4.1f}%)")
    print(f"{'─'*60}")
    if frame_perf:
        sizes = [f['enc_size'] for f in frame_perf]
        print(f"  Size:    avg={statistics.mean(sizes)/1024:.1f}KB  total={sum(sizes)/1024/1024:.1f}MB  ({len(frame_perf)} frames)")
    if timing:
        fps = [t['fps'] for t in timing]
        print(f"  FPS:     avg={statistics.mean(fps):.0f}  min={min(fps)}  max={max(fps)}  ({len(timing)}s)")
    print(f"{'='*60}\n")

    csv_path = f"/tmp/anchor_perf_{ts}.csv"

    # CSV export
    write_csv(dmabuf_frames, frame_perf, timing, csv_path)

    # Plot
    plot(settings, dmabuf_frames, frame_perf, timing, img_path)

    # Report
    write_report(settings, dmabuf_frames, frame_perf, timing, img_path, md_path)

    # Open in browser — detach so it doesn't die when script exits
    import subprocess, shutil
    opener = 'xdg-open' if shutil.which('xdg-open') else 'open'
    subprocess.Popen([opener, img_path], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                     start_new_session=True)

if __name__ == '__main__':
    main()
