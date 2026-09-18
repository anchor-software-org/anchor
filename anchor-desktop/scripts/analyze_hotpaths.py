#!/usr/bin/env python3
"""
Analyze perf.data to identify CPU hot paths and bottlenecks.

Usage:
    # From perf.data
    python3 scripts/analyze_hotpaths.py perf.data

    # From an already-generated folded stacks file
    python3 scripts/analyze_hotpaths.py --folded stacks.folded

    # Compare two profiles
    python3 scripts/analyze_hotpaths.py --diff before.data after.data

Output:
    - Terminal: top-N self-time functions, grouped hot paths
    - Markdown report (optional: -o report.md)
"""

import argparse
import re
import subprocess
import sys
import os
from collections import defaultdict
from pathlib import Path


# ── Perf data → folded stacks ────────────────────────────────────────────

def perf_to_folded(perf_data: str) -> str:
    """Convert perf.data to folded stack format using `perf script` + collapse."""
    # Check if inferno-collapse-perf exists (faster)
    for collapser in ["inferno-collapse-perf", "perf script"]:
        if collapser == "inferno-collapse-perf":
            if subprocess.run(["which", collapser], capture_output=True).returncode == 0:
                continue
            else:
                continue

    # Use perf script directly and fold ourselves
    result = subprocess.run(
        ["perf", "script", "-i", perf_data, "--no-demangle"],
        capture_output=True, text=True
    )
    return fold_stacks(result.stdout)


def fold_stacks(perf_script_output: str) -> str:
    """Fold perf script output into flamegraph collapsed format."""
    stacks = defaultdict(int)

    for line in perf_script_output.strip().split("\n"):
        if not line.strip() or line.startswith("#"):
            continue

        # perf script output format: process  pid  [cpu] timestamp trace: | ... stack
        # The stack is after the first " | " marker
        if " | " not in line:
            continue

        # Extract stack: everything after the first " | "
        stack_part = line.split(" | ", 1)[1]

        # Parse the stack frames
        # Frames are whitespace-separated hex addresses followed by symbol in parens
        # e.g.: 7f1234 func_name (lib.so)
        frames = []
        for token in stack_part.strip().split():
            if not token:
                continue
            # Try to extract symbol name from pattern like func_name+0x123 (lib.so)
            m = re.match(r'^[0-9a-f]+', token)
            if m:
                continue  # skip raw addresses

            # Frame format: symbol+offset (module)
            frame = token
            # Strip the +0x... offset
            frame = re.sub(r'\+0x[0-9a-f]+', '', frame)
            # Strip surrounding whitespace and parens
            frame = frame.strip('()')
            if frame and not frame.startswith('[') and not re.match(r'^[0-9a-f]+$', frame):
                frames.append(frame)

        if frames:
            # Reverse: perf stack is leaf-first, flamegraph wants root-first
            stack_str = ";".join(reversed(frames))
            stacks[stack_str] += 1

    # Format as folded stacks
    lines = []
    for stack, count in sorted(stacks.items(), key=lambda x: -x[1]):
        lines.append(f"{stack} {count}")
    return "\n".join(lines)


# ── Parse folded stacks ──────────────────────────────────────────────────

def parse_folded(folded_text: str):
    """Parse folded stacks into structured data."""
    stacks = []
    total_samples = 0
    func_samples = defaultdict(int)
    func_callers = defaultdict(set)
    func_callees = defaultdict(set)

    for line in folded_text.strip().split("\n"):
        if not line.strip():
            continue
        parts = line.rsplit(" ", 1)
        if len(parts) != 2:
            continue
        stack_str, count_str = parts
        try:
            count = int(count_str)
        except ValueError:
            continue

        frames = stack_str.split(";")
        stacks.append((frames, count))
        total_samples += count

        # Count self-time: the last frame (leaf) gets the samples
        if frames:
            leaf = frames[-1]
            func_samples[leaf] += count

        # Build caller/callee relationships
        for i, func in enumerate(frames):
            if i < len(frames) - 1:
                func_callees[func].add(frames[i + 1])
                func_callers[frames[i + 1]].add(func)

    return {
        "stacks": stacks,
        "total_samples": total_samples,
        "func_samples": dict(func_samples),
        "func_callers": {k: v for k, v in func_callers.items()},
        "func_callees": {k: v for k, v in func_callees.items()},
    }


# ── Analysis ─────────────────────────────────────────────────────────────

def top_self_time(data: dict, top_n: int = 30) -> list[tuple[str, int, float]]:
    """Return functions with highest self-time (where CPU was spent)."""
    total = data["total_samples"]
    sorted_funcs = sorted(data["func_samples"].items(), key=lambda x: -x[1])
    return [(name, count, count / total * 100) for name, count in sorted_funcs[:top_n]]


def hot_paths(data: dict, top_n: int = 10, min_pct: float = 1.0) -> list[tuple[str, int, float]]:
    """Return the hottest complete call paths (root-to-leaf)."""
    total = data["total_samples"]
    sorted_stacks = sorted(data["stacks"], key=lambda x: -x[1])
    result = []
    for frames, count in sorted_stacks:
        pct = count / total * 100
        if pct >= min_pct and len(result) < top_n:
            result.append((";".join(frames), count, pct))
        if len(result) >= top_n:
            break
    return result


def group_by_module(data: dict, top_n: int = 15) -> list[tuple[str, int, float]]:
    """Group self-time by module/namespace (enclosing crate/module)."""
    total = data["total_samples"]
    module_samples = defaultdict(int)

    for func, count in data["func_samples"].items():
        # Extract module from e.g. anchor::wayland::capture::foo
        parts = func.split("::")
        if len(parts) >= 2:
            module = "::".join(parts[:2])  # e.g. anchor::wayland
        elif len(parts) == 1:
            module = "::".join(parts[:1])
        else:
            module = func
        module_samples[module] += count

    sorted_mods = sorted(module_samples.items(), key=lambda x: -x[1])
    return [(name, count, count / total * 100) for name, count in sorted_mods[:top_n]]


def find_bottlenecks(data: dict, threshold_pct: float = 3.0) -> list[tuple[str, int, float, str]]:
    """Find functions above threshold_pct that may be bottlenecks."""
    total = data["total_samples"]
    bottlenecks = []
    for func, count in sorted(data["func_samples"].items(), key=lambda x: -x[1]):
        pct = count / total * 100
        if pct < threshold_pct:
            break

        # Determine category
        category = "general"
        if "alloc" in func.lower() or "::new" in func or "clone" in func.lower():
            category = "allocation"
        elif "lock" in func.lower() or "mutex" in func.lower():
            category = "locking"
        elif "encode" in func.lower() or "enc_" in func:
            category = "encoding"
        elif "capture" in func.lower() or "screencopy" in func.lower():
            category = "capture"
        elif "send" in func.lower() or "recv" in func.lower() or "write" in func.lower():
            category = "IO"
        elif "memcpy" in func.lower() or "copy" in func.lower() or "convert" in func.lower():
            category = "memory"

        bottlenecks.append((func, count, pct, category))

    return bottlenecks


# ── Diff analysis ────────────────────────────────────────────────────────

def diff_profiles(data_a: dict, data_b: dict) -> list[tuple[str, float, float, float]]:
    """Compare two profiles: show functions with largest self-time delta."""
    total_a = data_a["total_samples"]
    total_b = data_b["total_samples"]

    all_funcs = set(data_a["func_samples"]) | set(data_b["func_samples"])
    diffs = []
    for func in all_funcs:
        pct_a = data_a["func_samples"].get(func, 0) / total_a * 100 if total_a else 0
        pct_b = data_b["func_samples"].get(func, 0) / total_b * 100 if total_b else 0
        delta = pct_b - pct_a
        if abs(delta) > 0.2:
            diffs.append((func, pct_a, pct_b, delta))

    return sorted(diffs, key=lambda x: abs(x[3]), reverse=True)


# ── Render ────────────────────────────────────────────────────────────────

def render_terminal(data: dict):  # pragma: no cover - terminal output
    """Print a rich terminal summary."""
    total = data["total_samples"]
    print(f"\n{'='*70}")
    print(f"  ANCHOR DESKTOP — FLAMEGRAPH ANALYSIS")
    print(f"  {total:,} total samples")
    print(f"{'='*70}")

    # Top self-time
    print(f"\n{'─'*70}")
    print(f"  TOP SELF-TIME FUNCTIONS  (where CPU cycles are spent)")
    print(f"{'─'*70}")
    print(f"  {'Function':<52} {'Samples':>8}  {'%':>6}")
    print(f"  {'─'*52} {'─'*8}  {'─'*6}")
    for name, count, pct in top_self_time(data, 25):
        # Truncate long names
        display = name[:50] + ".." if len(name) > 52 else name
        print(f"  {display:<52} {count:>8,}  {pct:>5.1f}%")

    # By module
    print(f"\n{'─'*70}")
    print(f"  CPU TIME BY MODULE")
    print(f"{'─'*70}")
    for name, count, pct in group_by_module(data, 15):
        print(f"  {name:<50} {count:>8,}  {pct:>5.1f}%")

    # Hot paths
    print(f"\n{'─'*70}")
    print(f"  HOTTEST CALL PATHS (>1%)")
    print(f"{'─'*70}")
    for path, count, pct in hot_paths(data, 15, 1.0):
        display = path[:65] + "..." if len(path) > 68 else path
        print(f"  {count:>5,} ({pct:>4.1f}%)  {display}")

    # Bottlenecks
    print(f"\n{'─'*70}")
    print(f"  POTENTIAL BOTTLENECKS  (>2% self-time)")
    print(f"{'─'*70}")
    for name, count, pct, cat in find_bottlenecks(data, 2.0):
        print(f"  [{cat:<10}] {name:<45} {count:>7,}  {pct:>5.1f}%")

    print(f"\n{'='*70}\n")


def render_markdown(data: dict, output_path: str, title: str = "Anchor Desktop"):
    """Generate a markdown report."""
    total = data["total_samples"]
    lines = [f"# {title} — Flamegraph Analysis\n",
             f"**{total:,}** total samples\n"]

    # Top self-time
    lines.append("## Top Self-Time Functions\n")
    lines.append("| Function | Samples | % |")
    lines.append("|----------|---------|---|")
    for name, count, pct in top_self_time(data, 30):
        lines.append(f"| `{name}` | {count:,} | {pct:.1f}% |")

    # By module
    lines.append("\n## CPU Time by Module\n")
    lines.append("| Module | Samples | % |")
    lines.append("|--------|---------|---|")
    for name, count, pct in group_by_module(data, 20):
        lines.append(f"| `{name}` | {count:,} | {pct:.1f}% |")

    # Bottlenecks
    lines.append("\n## Potential Bottlenecks (>2% self-time)\n")
    lines.append("| Category | Function | Samples | % |")
    lines.append("|----------|----------|---------|---|")
    for name, count, pct, cat in find_bottlenecks(data, 2.0):
        lines.append(f"| {cat} | `{name}` | {count:,} | {pct:.1f}% |")

    # Hot paths
    lines.append("\n## Hottest Call Paths (>1%)\n")
    lines.append("| Samples | % | Path |")
    lines.append("|---------|---|------|")
    for path, count, pct in hot_paths(data, 20, 1.0):
        lines.append(f"| {count:,} | {pct:.1f}% | `{path}` |")

    with open(output_path, 'w') as f:
        f.write('\n'.join(lines) + '\n')
    print(f"[+] Report: {output_path}")


# ── Main ─────────────────────────────────────────────────────────────────

def main():
    ap = argparse.ArgumentParser(
        description="Analyze perf.data / flamegraph data for hot paths",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  python3 scripts/analyze_hotpaths.py perf.data
  python3 scripts/analyze_hotpaths.py -o report.md perf.data
  python3 scripts/analyze_hotpaths.py --diff before.data after.data
  python3 scripts/analyze_hotpaths.py --folded stacks.folded
"""
    )
    ap.add_argument("perf_data", nargs="?", help="perf.data file to analyze")
    ap.add_argument("--folded", metavar="FILE", help="Pre-folded stacks file")
    ap.add_argument("-o", "--output", metavar="FILE", help="Markdown report output")
    ap.add_argument("-n", "--top", type=int, default=30, help="Number of top functions (default: 30)")
    ap.add_argument("--diff", nargs=2, metavar=("BEFORE", "AFTER"),
                    help="Compare two perf.data files")
    args = ap.parse_args()

    # ── Diff mode ────────────────────────────────────────────────────────
    if args.diff:
        print("[+] Comparing profiles...")
        folded_a = fold_stacks(subprocess.run(
            ["perf", "script", "-i", args.diff[0], "--no-demangle"],
            capture_output=True, text=True).stdout)
        folded_b = fold_stacks(subprocess.run(
            ["perf", "script", "-i", args.diff[1], "--no-demangle"],
            capture_output=True, text=True).stdout)
        data_a = parse_folded(folded_a)
        data_b = parse_folded(folded_b)

        print(f"\n  A: {data_a['total_samples']:,} samples, B: {data_b['total_samples']:,} samples")
        print(f"\n  {'─'*55}")
        print(f"  {'Function':<40} {'A%':>5} {'B%':>5} {'Δ%':>5}")
        print(f"  {'─'*55}")
        for func, pct_a, pct_b, delta in diff_profiles(data_a, data_b)[:30]:
            arrow = "↑" if delta > 0 else "↓" if delta < 0 else " "
            print(f"  {func[:38]:<38} {pct_a:>4.1f} {pct_b:>4.1f} {arrow}{abs(delta):>4.1f}")

        if args.output:
            with open(args.output, 'w') as f:
                f.write("# Profile Diff\n\n")
                f.write(f"A: {data_a['total_samples']:,} samples, B: {data_b['total_samples']:,} samples\n\n")
                f.write("| Function | A% | B% | Δ% |\n")
                f.write("|----------|-----|-----|-----|\n")
                for func, pct_a, pct_b, delta in diff_profiles(data_a, data_b)[:50]:
                    arrow = "↑" if delta > 0 else "↓" if delta < 0 else " "
                    f.write(f"| `{func}` | {pct_a:.1f} | {pct_b:.1f} | {arrow}{abs(delta):.1f} |\n")
            print(f"\n[+] Report: {args.output}")
        return

    # ── Load data ────────────────────────────────────────────────────────
    if args.folded:
        with open(args.folded) as f:
            folded_text = f.read()
    elif args.perf_data:
        if not os.path.exists(args.perf_data):
            sys.exit(f"ERROR: {args.perf_data} not found")
        print(f"[+] Processing {args.perf_data}...")
        result = subprocess.run(
            ["perf", "script", "-i", args.perf_data, "--no-demangle"],
            capture_output=True, text=True
        )
        if result.returncode != 0:
            sys.exit(f"perf script failed: {result.stderr}")
        folded_text = fold_stacks(result.stdout)
    else:
        ap.print_help()
        sys.exit(1)

    data = parse_folded(folded_text)

    if data["total_samples"] == 0:
        print("[!] No samples found. Did perf collect any data?")
        sys.exit(1)

    # ── Terminal output ──────────────────────────────────────────────────
    render_terminal(data)

    # ── Markdown report ─────────────────────────────────────────────────
    if args.output:
        render_markdown(data, args.output)


if __name__ == "__main__":
    main()
