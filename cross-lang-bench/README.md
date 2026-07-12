# Ticklog Cross-Language Benchmarks

Compares ticklog against low-latency loggers from C++ and Go.

## Prerequisites

- Linux (x86_64): canonical platform. `taskset`, `cpupower`, `perf`.
- macOS (aarch64): best-effort. No core isolation, no `perf`, no NanoLog.

## Quick start

```sh
./setup.sh
./run.sh
python3 results.py results/*.json > ../BENCHMARKS.md
```

## Candidates

| Language | Library  | Output format |
| -------- | -------- | ------------- |
| Rust     | ticklog  | Text          |
| C++      | Quill    | Text          |
| C++      | NanoLog  | Binary        |
| Go       | zerolog  | JSON          |
| Go       | zap      | JSON          |
