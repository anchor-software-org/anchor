#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# flamegraph_app.sh — Profile the running anchor application with flamegraphs
#
# Two modes:
#   --attach PID   Profile a running anchor process for N seconds
#   (default)      Build + launch under perf, stop with Ctrl+C
#
# Usage:
#   scripts/flamegraph_app.sh                           # build + launch, Ctrl+C to stop
#   scripts/flamegraph_app.sh --attach $(pgrep anchor)  # profile running process
#   scripts/flamegraph_app.sh -p 12345 -d 30            # PID 12345 for 30s
#   scripts/flamegraph_app.sh -d 30 --open              # 30s then open SVG
#   scripts/flamegraph_app.sh --no-build                # skip rebuild
#
# Prerequisites:
#   - perf       (linux-tools / linux-perf)
#   - flamegraph (cargo install flamegraph)
#   - Debug symbols: builds with profile=profiling (debug=true, optimized)
# ---------------------------------------------------------------------------

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_DIR"

# ── defaults ─────────────────────────────────────────────────────────────
MODE="launch"               # launch | attach
PID=""
DURATION=0                  # 0 = until Ctrl+C
FREQ=997
OUTPUT_DIR="${PROJECT_DIR}/target/flamegraph"
OPEN=false
SKIP_BUILD=false
ANCHOR_BIN="./target/profiling/anchor"
OUTPUT=""

# ── parse args ───────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --attach|-p) MODE="attach"; PID="$2"; shift 2 ;;
        -d|--duration) DURATION="$2"; shift 2 ;;
        -o|--output)  OUTPUT="$2"; shift 2 ;;
        --open)       OPEN=true; shift ;;
        --no-build)   SKIP_BUILD=true; shift ;;
        -F|--freq)    FREQ="$2"; shift 2 ;;
        -h|--help)
            cat <<'EOF'
Usage: flamegraph_app.sh [OPTIONS]

  --attach, -p PID    Profile a running anchor process
  -d, --duration SEC  Profile for SEC seconds (default: until Ctrl+C)
  -o, --output PATH   Output SVG path
  --open              Open SVG in browser after generation
  --no-build          Skip building; use existing binary
  -F, --freq HZ       perf sampling frequency (default: 997)
  -h, --help          Show this help

Examples:
  flamegraph_app.sh                           # build + launch, Ctrl+C to stop
  flamegraph_app.sh -p $(pgrep anchor) -d 30  # running process, 30 sec
  flamegraph_app.sh -d 60 --open              # 60 sec snapshot, open SVG
EOF
            exit 0
            ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

# ── check tooling ────────────────────────────────────────────────────────
for cmd in perf flamegraph; do
    command -v "$cmd" &>/dev/null || {
        echo "ERROR: '$cmd' not found. Install: linux-tools + cargo install flamegraph"
        exit 1
    }
done

mkdir -p "$OUTPUT_DIR"

# ── timestamp label ──────────────────────────────────────────────────────
TS=$(date +%Y%m%d_%H%M%S)

if [[ -z "$OUTPUT" ]]; then
    OUTPUT="${OUTPUT_DIR}/anchor_flamegraph_${TS}.svg"
fi
PERF_DATA="${OUTPUT_DIR}/anchor_perf_${TS}.data"

# ── build ────────────────────────────────────────────────────────────────
build_app() {
    if $SKIP_BUILD && [[ -x "$ANCHOR_BIN" ]]; then
        echo "[+] Using existing binary: $ANCHOR_BIN"
        return
    fi
    echo "[+] Building with profiling profile (optimized + debuginfo)..."
    rtk cargo build --profile profiling 2>&1
    if [[ ! -x "$ANCHOR_BIN" ]]; then
        echo "ERROR: Build failed or binary not at $ANCHOR_BIN"
        exit 1
    fi
    echo "[+] Binary: $ANCHOR_BIN"
}

# ── launch mode: run the app under perf ──────────────────────────────────
launch_and_profile() {
    build_app

    local perf_args=(-g -F "$FREQ" --call-graph dwarf -o "$PERF_DATA")

    if [[ "$DURATION" -gt 0 ]]; then
        echo "[+] Profiling for ${DURATION}s with $FREQ Hz sampling..."
        timeout "$DURATION" perf record "${perf_args[@]}" -- "$ANCHOR_BIN" 2>/dev/null || true
    else
        echo "╔══════════════════════════════════════════════════════════╗"
        echo "║  App running under perf. Interact normally, then        ║"
        echo "║  press Ctrl+C in THIS terminal to stop profiling.       ║"
        echo "╚══════════════════════════════════════════════════════════╝"
        perf record "${perf_args[@]}" -- "$ANCHOR_BIN" 2>/dev/null || true
    fi
}

# ── attach mode: profile an already-running PID ──────────────────────────
attach_and_profile() {
    if [[ -z "$PID" ]]; then
        echo "ERROR: --attach requires a PID"
        exit 1
    fi
    if ! kill -0 "$PID" 2>/dev/null; then
        echo "ERROR: Process $PID not found or not running"
        exit 1
    fi

    if [[ "$DURATION" -gt 0 ]]; then
        echo "[+] Attaching to PID $PID for ${DURATION}s..."
        perf record -g -F "$FREQ" --call-graph dwarf -o "$PERF_DATA" -p "$PID" -- sleep "$DURATION" 2>/dev/null
    else
        echo "[+] Attaching to PID $PID. Press Ctrl+C to stop..."
        perf record -g -F "$FREQ" --call-graph dwarf -o "$PERF_DATA" -p "$PID" 2>/dev/null || true
    fi
}

# ── generate flamegraph ──────────────────────────────────────────────────
generate_flamegraph() {
    local git_ref
    git_ref=$(git rev-parse --short HEAD 2>/dev/null || echo "unknown")

    echo "[+] Generating flamegraph..."

    flamegraph \
        --perfdata "$PERF_DATA" \
        --title "Anchor Desktop — $TS (${git_ref})" \
        --subtitle "${DURATION}s sample @ ${FREQ}Hz" \
        --palette rust \
        -o "$OUTPUT" \
        2>&1

    local size_kb
    size_kb=$(du -k "$OUTPUT" | cut -f1)
    echo "[+] Flamegraph: $OUTPUT (${size_kb}KB)"

    if [[ -z "${KEEP_PERF_DATA:-}" ]]; then
        rm -f "$PERF_DATA"
    else
        echo "[+] Kept perf data: $PERF_DATA"
    fi
}

# ── main ─────────────────────────────────────────────────────────────────
if [[ "$MODE" == "attach" ]]; then
    attach_and_profile
else
    launch_and_profile
fi

generate_flamegraph

if $OPEN; then
    xdg-open "$OUTPUT" 2>/dev/null || open "$OUTPUT" 2>/dev/null || true
fi
