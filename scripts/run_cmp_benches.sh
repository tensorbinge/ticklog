#!/usr/bin/env bash
set -euo pipefail

# Run ecosystem comparison benches and save results to logs/benches/.
# Usage:
#   ./scripts/run_cmp_benches.sh --list          # list available benches
#   ./scripts/run_cmp_benches.sh --latency       # all latency benches
#   ./scripts/run_cmp_benches.sh --throughput    # all throughput benches
#   ./scripts/run_cmp_benches.sh --scaling       # all scaling benches
#   ./scripts/run_cmp_benches.sh --all           # everything (slow!)
#   ./scripts/run_cmp_benches.sh latency_vs_ticklog ...  # specific benches

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
LOGS_DIR="$PROJECT_DIR/logs/benches"
TIMESTAMP="$(date +%Y%m%d%H%M%S)"

LIBS=(baseline tracing env_logger slog)

# Ensure cargo is on PATH. Source .cargo/env from common locations.
if ! command -v cargo &>/dev/null; then
    for cargo_env in "${CARGO_HOME:+$CARGO_HOME/env}" "$HOME/.cargo/env"; do
        if [[ -f "$cargo_env" ]]; then
            source "$cargo_env"
            break
        fi
    done
fi

list_benches() {
    echo "Available cmp benches:"
    echo ""
    echo "  Latency (per-call hot-path):"
    for lib in "${LIBS[@]}"; do
        echo "    latency_vs_$lib"
    done
    echo ""
    echo "  Throughput (calls/s, bytes/s):"
    for lib in "${LIBS[@]}"; do
        echo "    throughput_vs_$lib"
    done
    echo ""
    echo "  Scaling (multi-thread 1/2/4/8):"
    for lib in "${LIBS[@]}"; do
        echo "    scaling_vs_$lib"
    done
    echo ""
    echo "Usage:"
    echo "  $0 --latency       # all latency benches"
    echo "  $0 --throughput    # all throughput benches"
    echo "  $0 --scaling       # all scaling benches"
    echo "  $0 --all           # every comparison bench"
    echo "  $0 latency_vs_slog throughput_vs_baseline  # specific"
}

cd "$PROJECT_DIR"
mkdir -p "$LOGS_DIR"

# Parse args
if [[ $# -eq 0 ]] || [[ "$1" == "--list" ]] || [[ "$1" == "-l" ]]; then
    list_benches
    exit 0
fi

declare -a TO_RUN=()

case "$1" in
    --all)
        for dim in latency throughput scaling; do
            for lib in "${LIBS[@]}"; do
                TO_RUN+=("${dim}_vs_${lib}")
            done
        done
        ;;
    --latency)
        for lib in "${LIBS[@]}"; do
            TO_RUN+=("latency_vs_${lib}")
        done
        ;;
    --throughput)
        for lib in "${LIBS[@]}"; do
            TO_RUN+=("throughput_vs_${lib}")
        done
        ;;
    --scaling)
        for lib in "${LIBS[@]}"; do
            TO_RUN+=("scaling_vs_${lib}")
        done
        ;;
    *)
        TO_RUN=("$@")
        ;;
esac

echo "=== ticklog cmp bench runner ==="
echo "Timestamp: $TIMESTAMP"
echo "Benches:   ${TO_RUN[*]}"
echo "Logs dir:  $LOGS_DIR"
echo ""

run_one() {
    local bench="$1"
    local logfile="$LOGS_DIR/bench-${TIMESTAMP}-${bench}.txt"

    echo -n "  $bench ... "

    if cargo bench --bench "$bench" >> "$logfile" 2>&1; then
        echo "OK"
    else
        local rc=$?
        echo "FAILED (rc=$rc)"
    fi
}

for bench in "${TO_RUN[@]}"; do
    run_one "$bench"
done

# Quick summary
echo ""
echo "=== Results ==="
for arg in "${TO_RUN[@]}"; do
    logfile="$LOGS_DIR/bench-${TIMESTAMP}-${arg}.txt"
    if [[ ! -f "$logfile" ]]; then
        echo "[$arg] log not found"
        continue
    fi
    echo "[$arg]"
    grep -E 'time:\s*\[' "$logfile" | sed -E 's/^[[:space:]]+//' | while read -r line; do
        bname=$(echo "$line" | awk '{print $1}')
        median=$(echo "$line" | grep -oE '\[[^]]*\]' | tail -1 | tr -d '[]' | awk '{print $2}')
        echo "  $bname  $median"
    done
    echo ""
done
echo "Logs: $LOGS_DIR/bench-${TIMESTAMP}-*"
