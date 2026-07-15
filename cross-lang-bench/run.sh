#!/usr/bin/env bash
# run.sh -- Calibrate, run every harness, and generate the comparison report.
#
# macOS:
#   Best-effort run — no core isolation, no frequency locking, no perf.
#   NanoLog is skipped (Linux-only).
#
# Linux:
#   Full methodology — CPU pinning via taskset, hardware counters via
#   perf stat, performance governor via cpupower. Pass --cpu <n> to
#   specify the isolated core (default: 1).
#
# Usage:
#   ./run.sh                        # best-effort (macOS) or default Linux
#   ./run.sh --cpu 2                # pin to CPU 2 (Linux only)
#   ./run.sh --cpu 2 --drain-cpu 4  # ticklog two-core placement
#   ./run.sh --no-perf              # skip perf stat (Linux only)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

OS="$(uname -s)"
CPU_CORE=""
DRAIN_CORE=""
USE_PERF=1
DRY_RUN=0

# -- CLI ----------------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case "$1" in
        --cpu)
            CPU_CORE="$2"
            shift 2
            ;;
        --drain-cpu)
            DRAIN_CORE="$2"
            shift 2
            ;;
        --no-perf)
            USE_PERF=0
            shift
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        *)
            echo "error: unknown flag '$1'" >&2
            echo "usage: $0 [--cpu <n>] [--drain-cpu <n>] [--no-perf] [--dry-run]" >&2
            exit 1
            ;;
    esac
done

mkdir -p results

# -- Prerequisites (Linux) ----------------------------------------------

if [[ "$OS" == "Linux" ]]; then
    if [[ -z "$CPU_CORE" ]]; then
        echo "NOTE: --cpu not set. Running without CPU pinning."
        echo "     For canonical results, isolate a core and pass --cpu <n>."
        echo ""
    fi

    if [[ "$USE_PERF" -eq 1 ]]; then
        if ! command -v perf &>/dev/null; then
            echo "WARN: 'perf' not found. Running without hardware counters." >&2
            echo "      Install: apt install linux-tools-common linux-tools-\$(uname -r)" >&2
            USE_PERF=0
        fi
    fi

    # Check for cpupower (non-fatal — the user may have already set the governor).
    if command -v cpupower &>/dev/null; then
        echo "=== Setting performance governor ==="
        sudo cpupower frequency-set -g performance 2>&1 || true
        GOVERNOR_CHANGED=1
    else
        echo "NOTE: cpupower not found. Ensure the performance governor is set."
    fi

    # Build the taskset / perf prefix.
    if [[ -n "$CPU_CORE" ]]; then
        PIN_PREFIX="taskset -c $CPU_CORE"
    else
        PIN_PREFIX=""
    fi
fi

# -- Calibration --------------------------------------------------------

echo "=== Calibrating ==="
if [[ ! -x ./calibrate ]]; then
    echo "error: calibrate binary not found. Run ./setup.sh first." >&2
    exit 1
fi

if [[ "$DRY_RUN" -eq 1 ]]; then
    NS_PER_TICK="1.0"
    echo "  dry-run: using ns_per_tick = $NS_PER_TICK"
else
    NS_PER_TICK="$(./calibrate)"
    echo "  ns_per_tick = $NS_PER_TICK"
fi

# -- Run harnesses ------------------------------------------------------

run_one() {
    local name="$1"
    local bin="$2"
    local extra_args="${3:-}"

    echo ""
    echo "=== Running $name ==="

    if [[ ! -x "$bin" ]]; then
        echo "  SKIP: binary not found at $bin" >&2
        return
    fi

    local pin="$PIN_PREFIX"
    local cmd
    if [[ "$OS" == "Linux" && "$USE_PERF" -eq 1 ]]; then
        cmd="$pin perf stat -e cycles,instructions,cache-misses,cache-references,branches,branch-misses \
            -o results/${name}.perf -- $bin --ns-per-tick $NS_PER_TICK --output results/${name}.json $extra_args"
    elif [[ -n "${pin:-}" ]]; then
        cmd="$pin $bin --ns-per-tick $NS_PER_TICK --output results/${name}.json $extra_args"
    else
        cmd="$bin --ns-per-tick $NS_PER_TICK --output results/${name}.json $extra_args"
    fi

    if [[ "$DRY_RUN" -eq 1 ]]; then
        echo "  [dry-run] $cmd"
        return
    fi

    eval "$cmd"
    echo "  done -> results/${name}.json"
    if [[ "$OS" == "Linux" && "$USE_PERF" -eq 1 ]]; then
        echo "  perf -> results/${name}.perf"
    fi
}

if [[ -n "$CPU_CORE" && -n "$DRAIN_CORE" ]]; then
    # Two-core placement for ticklog: producer on CPU_CORE, drain on DRAIN_CORE
    PIN_PREFIX="taskset -c $CPU_CORE,$DRAIN_CORE"
    run_one "ticklog" "rust/ticklog/target/release/ticklog-cross-lang-harness" \
        "--producer-core $CPU_CORE --backend-core $DRAIN_CORE"
    # Restore single-core pinning for inline loggers
    PIN_PREFIX="taskset -c $CPU_CORE"
else
    run_one "ticklog" "rust/ticklog/target/release/ticklog-cross-lang-harness"
fi

run_one "zerolog"  "bin/zerolog_harness"
run_one "zap"      "bin/zap_harness"
run_one "quill"    "cpp/quill/build/quill_harness"

if [[ "$OS" == "Linux" ]]; then
    run_one "nanolog" "cpp/nanolog/build/nanolog_harness"
else
    echo ""
    echo "=== Skipping NanoLog (Linux only) ==="
fi

# -- Report -------------------------------------------------------------

echo ""
echo "=== Generating report ==="

# Collect all available JSON result files.
shopt -s nullglob
JSON_FILES=(results/*.json)
if [[ ${#JSON_FILES[@]} -eq 0 ]]; then
    echo "error: no result JSON files found in results/" >&2
    exit 1
fi

if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "  python3 results.py ${JSON_FILES[*]}"
else
    python3 results.py "${JSON_FILES[@]}" > BENCHMARKS.md
    echo "  done -> BENCHMARKS.md"
fi

# -- Restore (Linux) ----------------------------------------------------

if [[ "$OS" == "Linux" && -n "${GOVERNOR_CHANGED:-}" ]]; then
    echo ""
    echo "=== Restoring power-saving governor ==="
    sudo cpupower frequency-set -g powersave 2>&1 || true
fi

# -- Caveats ------------------------------------------------------------

echo ""
if [[ "$OS" == "Darwin" ]]; then
    echo "NOTE: macOS results are best-effort (no core isolation, no frequency"
    echo "locking, no perf stat). For canonical numbers, run on Linux x86_64"
    echo "with core isolation and the performance governor."
else
    if [[ -z "$CPU_CORE" ]]; then
        echo "NOTE: No CPU pinning was used. For canonical numbers, isolate a"
        echo "core via isolcpus= and re-run with --cpu <n>."
    fi
    echo "Hardware counter data written to results/*.perf (if perf was available)."
fi
echo ""
echo "Report: BENCHMARKS.md"
