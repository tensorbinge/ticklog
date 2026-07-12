#!/usr/bin/env python3
"""
Collect cross-language benchmark results and generate markdown comparison
tables.

Usage:
    python3 results.py results/*.json > BENCHMARKS.md

Each candidate emits one JSON file with a shared schema (dimension, candidate,
metric, value). This script pivots the per-candidate files into per-dimension
tables suitable for a README or design document.
"""

import json
import sys


def load_results(paths):
    """Parse every JSON file and return a dict keyed by candidate name."""
    data = {}
    for p in paths:
        with open(p) as f:
            d = json.load(f)
        data[d["candidate"]] = d
    return data


def latency_table(data, workload, thread_count):
    """Return a markdown table of latency percentiles for one config."""
    header = [
        "Candidate",
        "p50 (ns)",
        "p95 (ns)",
        "p99 (ns)",
        "p999 (ns)",
        "max (ns)",
    ]
    rows = []
    for name in sorted(data):
        d = data[name]
        for r in d["results"]:
            if r["workload"] == workload and r["threads"] == thread_count:
                rows.append(
                    [
                        name,
                        f'{r["p50"]:.1f}',
                        f'{r["p95"]:.1f}',
                        f'{r["p99"]:.1f}',
                        f'{r["p999"]:.1f}',
                        f'{r["max"]:.1f}',
                    ]
                )
                break

    if not rows:
        return ""

    widths = [max(len(row[i]) for row in [header] + rows) for i in range(len(header))]
    fmt = "| " + " | ".join(f"{{:<{w}}}" for w in widths) + " |"
    sep = "|-" + "-|-".join("-" * w for w in widths) + "-|"

    lines = [fmt.format(*header), sep]
    for row in rows:
        lines.append(fmt.format(*row))
    return "\n".join(lines)


def throughput_table(data, thread_count):
    """Return a markdown table of throughput (records/sec) by workload."""
    workloads = ["single_int", "mixed", "string"]
    header = ["Candidate"] + [f"{w} (r/s)" for w in workloads]
    rows = []
    for name in sorted(data):
        d = data[name]
        row = [name]
        for wl in workloads:
            for r in d["results"]:
                if r["workload"] == wl and r["threads"] == thread_count:
                    row.append(f'{r["throughput"]:,}')
                    break
            else:
                row.append("-")
        rows.append(row)

    widths = [max(len(row[i]) for row in [header] + rows) for i in range(len(header))]
    fmt = "| " + " | ".join(f"{{:<{w}}}" for w in widths) + " |"
    sep = "|-" + "-|-".join("-" * w for w in widths) + "-|"

    lines = [fmt.format(*header), sep]
    for row in rows:
        lines.append(fmt.format(*row))
    return "\n".join(lines)


def scaling_table(data):
    """Return a markdown table of throughput scaling across thread counts."""
    header = ["Candidate", "1 thread", "2 threads", "4 threads", "scale 1->4"]
    rows = []
    for name in sorted(data):
        d = data[name]
        # Aggregate throughput across all workloads for each thread count.
        tps = {}
        for r in d["results"]:
            tps.setdefault(r["threads"], 0)
            tps[r["threads"]] += r["throughput"]

        t1 = tps.get(1, 0)
        t2 = tps.get(2, 0)
        t4 = tps.get(4, 0)
        scale = f"{t4 / t1:.2f}x" if t1 > 0 else "-"

        rows.append([name, f"{t1:,}", f"{t2:,}", f"{t4:,}", scale])

    widths = [max(len(row[i]) for row in [header] + rows) for i in range(len(header))]
    fmt = "| " + " | ".join(f"{{:<{w}}}" for w in widths) + " |"
    sep = "|-" + "-|-".join("-" * w for w in widths) + "-|"

    lines = [fmt.format(*header), sep]
    for row in rows:
        lines.append(fmt.format(*row))
    return "\n".join(lines)


def jitter_table(data):
    """Return a markdown table of tail latency (p99, p999, max, p99/p50 ratio).

    Aggregates across all workloads: worst-case jitter per candidate.
    """
    header = ["Candidate", "p99 (ns)", "p999 (ns)", "max (ns)", "p99/p50"]
    rows = []
    for name in sorted(data):
        d = data[name]
        # Take the maximum p99, p999, max, and p99/p50 ratio across all
        # single-threaded configs (jitter is most meaningful at low load).
        worst_p99 = 0.0
        worst_p999 = 0.0
        worst_max = 0.0
        worst_ratio = 0.0
        for r in d["results"]:
            if r["threads"] == 1:
                if r["p99"] > worst_p99:
                    worst_p99 = r["p99"]
                if r["p999"] > worst_p999:
                    worst_p999 = r["p999"]
                if r["max"] > worst_max:
                    worst_max = r["max"]
                ratio = r["p99"] / r["p50"] if r["p50"] > 0 else 0
                if ratio > worst_ratio:
                    worst_ratio = ratio

        rows.append([
            name,
            f"{worst_p99:.1f}",
            f"{worst_p999:.1f}",
            f"{worst_max:.1f}",
            f"{worst_ratio:.1f}x",
        ])

    widths = [max(len(row[i]) for row in [header] + rows) for i in range(len(header))]
    fmt = "| " + " | ".join(f"{{:<{w}}}" for w in widths) + " |"
    sep = "|-" + "-|-".join("-" * w for w in widths) + "-|"

    lines = [fmt.format(*header), sep]
    for row in rows:
        lines.append(fmt.format(*row))
    return "\n".join(lines)


def main():
    if len(sys.argv) < 2:
        print(f"usage: {sys.argv[0]} results/*.json", file=sys.stderr)
        sys.exit(1)

    data = load_results(sys.argv[1:])
    if not data:
        print("error: no results loaded", file=sys.stderr)
        sys.exit(1)

    print("# Cross-Language Benchmark Results\n")
    print(f"Candidates: {', '.join(sorted(data))}\n")

    # Platform info from the first candidate.
    first = next(iter(data.values()))
    print(f"**OS:** {first['os']}  \n")
    print(f"**Arch:** {first['arch']}  \n")
    print(f"**Batch size:** {first['batch_size']}  \n")
    print(f"**Samples per config:** {first['samples']}  \n")
    print(f"**Total messages per config:** {first['total_messages']:,}  \n")
    print("")

    workloads = ["single_int", "mixed", "string"]
    thread_counts = [1, 2, 4]

    # Latency tables
    print("## Latency\n")
    for wl in workloads:
        for tc in thread_counts:
            print(f"### {wl}: {tc} thread(s)\n")
            t = latency_table(data, wl, tc)
            if t:
                print(t)
                print("")

    # Throughput tables
    print("## Throughput\n")
    for tc in thread_counts:
        print(f"### {tc} thread(s)\n")
        print(throughput_table(data, tc))
        print("")

    # Scaling
    print("## Thread Scaling\n")
    print(scaling_table(data))
    print("")

    # Jitter (tail latency)
    print("## Jitter (1 thread, worst across workloads)\n")
    print(jitter_table(data))
    print("")

    # Notes
    print("## Notes\n")
    print("- macOS results are best-effort (no core isolation, no frequency locking).")
    print("- Go and C++ harnesses self-calibrate via CNTFRQ_EL0 due to SDK-linkage differences.")
    print("- Quill single_int format (\"x={}\") is slower than simple \"{}\"; real fmt behavior.")
    print("- Canonical numbers require Linux x86_64 with `perf stat` and core isolation.")


if __name__ == "__main__":
    main()
