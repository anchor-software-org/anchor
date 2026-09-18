#!/usr/bin/env python3
"""
Compare multiple anchor performance runs side-by-side.

Reads all /tmp/anchor_perf_*.md files, extracts settings and metrics,
and produces a comparison chart showing how each parameter affects performance.

Usage:
    python3 scripts/compare_runs.py
"""

import re
import os
import statistics
from datetime import datetime

def parse_md_report(path):
    """Extract settings and key metrics from a markdown report."""
    with open(path, 'r') as f:
        content = f.read()

    run = {'file': os.path.basename(path)}

    # Settings
    for m in re.finditer(r'\| bitrate \| (\d+) Mbps \|', content):
        run['bitrate_mbps'] = int(m.group(1))
    for m in re.finditer(r'\| fps \| (\d+) \|', content):
        run['fps'] = int(m.group(1))
    for m in re.finditer(r'\| gop \| (\d+) \|', content):
        run['gop'] = int(m.group(1))
    for m in re.finditer(r'\| rate_control \| (\w+) \|', content):
        run['rc'] = m.group(1)
    for m in re.finditer(r'\| low_power \| (\w+) \|', content):
        run['low_power'] = m.group(1)

    # DMA-BUF total
    m = re.search(r'\| total \| min=(\d+)us max=(\d+)us avg=(\d+)us.*p99=(\d+)us', content)
    if m:
        run['encode_avg_us'] = int(m.group(3))
        run['encode_p99_us'] = int(m.group(4))
        run['encode_min_us'] = int(m.group(1))
        run['encode_max_us'] = int(m.group(2))

    # Samples
    m = re.search(r'\*\*(\d+) samples\*\*', content)
    if m:
        run['samples'] = int(m.group(1))

    # Frame size from "Avg frame: X.XKB"
    m = re.search(r'Avg frame: ([\d.]+)KB', content)
    if m:
        run['avg_frame_kb'] = float(m.group(1))

    # FPS
    m = re.search(r'avg=(\d+)fps\s+min=(\d+)\s+max=(\d+)', content)
    if m:
        run['fps_avg'] = int(m.group(1))
        run['fps_min'] = int(m.group(2))
        run['fps_max'] = int(m.group(3))

    return run

def main():
    import glob

    files = sorted(glob.glob('/tmp/anchor_perf_*.md'))
    if not files:
        print("No reports found in /tmp/anchor_perf_*.md")
        return

    runs = []
    for f in files:
        run = parse_md_report(f)
        if 'bitrate_mbps' in run and 'encode_avg_us' in run:
            runs.append(run)

    if not runs:
        print("No valid runs with settings + metrics found.")
        return

    # Deduplicate — if same settings appear multiple times, keep the one with most samples
    seen = {}
    for r in runs:
        key = (r.get('bitrate_mbps'), r.get('fps'), r.get('gop'), r.get('rc'))
        if key not in seen or r.get('samples', 0) > seen[key].get('samples', 0):
            seen[key] = r
    runs = sorted(seen.values(), key=lambda r: (r.get('bitrate_mbps', 0), r.get('fps', 0), r.get('gop', 0)))

    # Print comparison table
    print(f"\n{'='*100}")
    print(f"  ANCHOR PERFORMANCE COMPARISON — {len(runs)} unique configurations")
    print(f"{'='*100}")
    print(f"{'Bitrate':>8} {'FPS':>4} {'GOP':>4} {'RC':>4} {'Samples':>8} "
          f"{'Enc avg':>8} {'Enc p99':>8} {'Frame KB':>9} {'FPS avg':>8} {'FPS min':>8}")
    print(f"{'─'*100}")
    for r in runs:
        print(f"{r.get('bitrate_mbps','?'):>7}M {r.get('fps','?'):>4} {r.get('gop','?'):>4} "
              f"{r.get('rc','?'):>4} {r.get('samples','?'):>8} "
              f"{r.get('encode_avg_us','?'):>7}us {r.get('encode_p99_us','?'):>7}us "
              f"{r.get('avg_frame_kb','?'):>8}KB "
              f"{r.get('fps_avg','?'):>7} {r.get('fps_min','?'):>7}")
    print(f"{'='*100}\n")

    # Plot
    try:
        import matplotlib.pyplot as plt
        import matplotlib
        matplotlib.use('Agg')
    except ImportError:
        print("[!] matplotlib not installed — skipping plots")
        return

    fig, axes = plt.subplots(2, 2, figsize=(14, 9))
    fig.suptitle(f'Anchor Parameter Comparison — {len(runs)} runs', fontsize=13)

    labels = [f"{r.get('bitrate_mbps','?')}M\n{r.get('fps','?')}fps\ngop={r.get('gop','?')}" for r in runs]
    x = range(len(runs))

    # 1. Encode time vs settings
    ax = axes[0][0]
    avgs = [r.get('encode_avg_us', 0) for r in runs]
    p99s = [r.get('encode_p99_us', 0) for r in runs]
    ax.bar([i - 0.15 for i in x], avgs, 0.3, label='avg', color='#42A5F5')
    ax.bar([i + 0.15 for i in x], p99s, 0.3, label='p99', color='#EF5350')
    ax.set_xticks(list(x))
    ax.set_xticklabels(labels, fontsize=7)
    ax.set_ylabel('Encode time (us)')
    ax.set_title('Encode Time vs Settings')
    ax.legend()

    # 2. Frame size vs settings
    ax = axes[0][1]
    sizes = [r.get('avg_frame_kb', 0) for r in runs]
    colors = ['#66BB6A' if s < 20 else '#FFA726' if s < 50 else '#EF5350' for s in sizes]
    ax.bar(x, sizes, color=colors)
    ax.set_xticks(list(x))
    ax.set_xticklabels(labels, fontsize=7)
    ax.set_ylabel('Avg frame size (KB)')
    ax.set_title('Frame Size vs Settings (green=small, red=large)')

    # 3. Bitrate vs encode time (scatter)
    ax = axes[1][0]
    bitrates = [r.get('bitrate_mbps', 0) for r in runs]
    ax.scatter(bitrates, avgs, s=80, c='#42A5F5', edgecolors='#1565C0', zorder=5)
    for i, r in enumerate(runs):
        ax.annotate(f"gop={r.get('gop','?')}", (bitrates[i], avgs[i]),
                   fontsize=7, ha='center', va='bottom')
    ax.set_xlabel('Bitrate (Mbps)')
    ax.set_ylabel('Avg encode time (us)')
    ax.set_title('Bitrate vs Encode Time')

    # 4. GOP vs frame size (scatter)
    ax = axes[1][1]
    gops = [r.get('gop', 0) for r in runs]
    ax.scatter(gops, sizes, s=80, c='#26A69A', edgecolors='#00695C', zorder=5)
    for i, r in enumerate(runs):
        ax.annotate(f"{r.get('bitrate_mbps','?')}M", (gops[i], sizes[i]),
                   fontsize=7, ha='center', va='bottom')
    ax.set_xlabel('GOP size')
    ax.set_ylabel('Avg frame size (KB)')
    ax.set_title('GOP vs Frame Size')

    plt.tight_layout()
    ts = datetime.now().strftime("%Y%m%d_%H%M%S")
    out_path = f"/tmp/anchor_compare_{ts}.png"
    plt.savefig(out_path, dpi=150)
    print(f"[+] Comparison plot: {out_path}")

    import subprocess, shutil
    opener = 'xdg-open' if shutil.which('xdg-open') else 'open'
    subprocess.Popen([opener, out_path], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                     start_new_session=True)

if __name__ == '__main__':
    main()
