# ticklog

A fast, minimal logging library for latency-critical Rust applications, such as high-frequency trading, where the cost of a log call on the hot path must stay in the low tens of nanoseconds.

Log calls run exclusively on the calling thread's hot path: check the level, encode a compact binary record into that thread's private lock-free buffer, and return. A background drain thread does the rest: decoding, formatting, timestamping, and writing each record, keeping all of that cost off the calling thread.

## Features

- **Nanosecond hot path:** 5–7 ns per call, over 30x faster than general-purpose loggers (see [Benchmarks](#benchmarks)), with no per-call allocation, formatting, or I/O on the calling thread, so the cost stays small and predictable on latency-critical paths.
- **Console/file sinks + Fanout:** stdout or stderr (colored by level) and buffered files, with fan-out from one record to several sinks and per-sink level filtering.
- **Ecosystem support:** file rotation, compression, and retention come from existing crates like [logroller](https://crates.io/crates/logroller) and [tracing-appender](https://crates.io/crates/tracing-appender) (see [Examples](examples/sinks/)); anything that implements `io::Write` plugs in as a sink.
- **Zero dependencies:** no runtime dependencies; a minimal, self-contained crate.

## Installation

```toml
[dependencies]
ticklog = "0.1"
```

Requires Rust 1.85 or newer (edition 2024).

## Quick start

```rust
use ticklog::{info, FileSink};

let _guard = ticklog::configure! {
    sink: FileSink::new("app.log").unwrap(),
}
.unwrap();

info!("listening on {}", 8080);
```

`ticklog::configure!` returns a `Guard`. Keep it alive for as long as you want to log: when it is dropped it flushes the sink, stops the background thread, and disables logging, so every log call afterwards is a silent no-op.

## Benchmarks

Per-call latency (p50). Lower is better.

### Benchmark names

| Name    | Log call                                           |
| ------- | -------------------------------------------------- |
| one_u64 | `info!("x={}", 42u64)`                             |
| one_str | `info!("{}", "hello world")`                       |
| mixed   | `info!("{} {} {}", 42u64, 3.14159, "hello world")` |

### Rust Ecosystem

**Mac M4** (Apple M4, 4.4 GHz, macOS 15):

| Logger      |    one_u64 |    one_str |      mixed |
| ----------- | ---------: | ---------: | ---------: |
| **ticklog** | **5.2 ns** | **5.9 ns** | **7.0 ns** |
| env_logger  |     231 ns |     232 ns |     307 ns |
| slog        |     274 ns |     269 ns |     454 ns |
| tracing     |     386 ns |     425 ns |     458 ns |

**Granite Rapids** (Intel Xeon 6982P-C, 3.9 GHz, Ubuntu 24.04):

| Logger      |    one_u64 |    one_str |      mixed |
| ----------- | ---------: | ---------: | ---------: |
| **ticklog** | **8.6 ns** | **8.4 ns** | **9.9 ns** |
| env_logger  |     370 ns |     371 ns |     491 ns |
| slog        |     499 ns |     453 ns |     686 ns |
| tracing     |     837 ns |     854 ns |     937 ns |

Run with `cargo bench --bench latency_vs_baseline` (ticklog) and `cargo bench --bench latency_vs_<logger>` (others).

### Cross-Language Comparison

Granite Rapids bare metal, identical protocol (BATCH=1000, RDTSC). All numbers p50, single thread.

| Logger      | Language |    one_u64 |    one_str |      mixed |
| ----------- | -------- | ---------: | ---------: | ---------: |
| nanolog     | C++      |     7.6 ns |     7.6 ns |     7.7 ns |
| **ticklog** | **Rust** | **7.6 ns** | **7.8 ns** | **8.4 ns** |
| quill       | C++      |     7.7 ns |    10.0 ns |     9.9 ns |
| zerolog     | Go       |    56.8 ns |    60.6 ns |   114.0 ns |
| zap         | Go       |   286.3 ns |   296.2 ns |   391.3 ns |

Reproduce: `cd cross-lang-bench && ./setup.sh && ./run.sh --cpu <n> --drain-cpu <m> --no-perf`. See [cross-lang-bench](cross-lang-bench/) for details.

## Configuration

`ticklog::configure!` accepts these keys, each optional:

| Key               | Purpose                                                                         | Default                 |
| ----------------- | ------------------------------------------------------------------------------- | ----------------------- |
| `sink`            | Where output goes.                                                              | `ConsoleSink` on stderr |
| `max_level`       | Records above this level are dropped on the calling thread before any encoding. | `Level::Info`           |
| `backpressure`    | What a logging thread does when its buffer is full.                             | `Backpressure::Drop`    |
| `timezone_offset` | Seconds east of UTC, applied to timestamp formatting only.                      | `0` (UTC)               |
| `drain_affinity`  | Pin the background thread to a set of logical CPUs.                             | none                    |

Example with every key:

```rust
use ticklog::{ConsoleSink, Level, Backpressure};

let _guard = ticklog::configure! {
    sink: ConsoleSink::stderr(),
    max_level: Level::Trace,
    backpressure: Backpressure::Drop,
    timezone_offset: 3600,
    drain_affinity: Some(vec![0]),
}
.unwrap();
```

`Backpressure::Drop` discards the record and returns immediately, never blocking the caller. `Backpressure::Block` spins until space frees up: it never drops records but burns CPU while the buffer stays full.

## Sinks

A `LogSink` is the final destination for formatted lines. The crate ships three:

```rust
use ticklog::{ConsoleSink, ColorMode, FileSink};

// stdout or stderr, colored by level (auto-detected, or forced on/off)
let console = ConsoleSink::stderr();
let plain = ConsoleSink::stdout().with_color(ColorMode::Never);

// a buffered single file, appended to or truncated on open
let appended = FileSink::new("app.log").unwrap();
let fresh = FileSink::truncate("app.log").unwrap();
```

Compose and filter with `FanOut` (dispatch one record to several sinks) and `with_max_level` (limit a sink to a level and below):

```rust
use ticklog::{ConsoleSink, FanOut, Level, LogSinkExt};

let sink = FanOut::new()
    .add(ConsoleSink::stderr().with_max_level(Level::Warn))
    .add(ConsoleSink::stdout().with_max_level(Level::Info));
```

### Custom sinks

For a destination that is not `io::Write`, such as a channel or a metrics counter, implement `LogSink` directly.

```rust
use std::io;
use std::net::UdpSocket;
use ticklog::{Level, LogSink};

struct UdpSink {
    socket: UdpSocket,
}

impl LogSink for UdpSink {
    fn accept(&mut self, line: &[u8], _level: Level) -> io::Result<()> {
        self.socket.send(line).map(|_| ())
    }
}
```

## Threads

Any thread may log, and each allocates its own buffer on first use. To move that one-time allocation off a latency-sensitive path, call `warm_up()` on the thread before its first log call. `pin_thread` pins the calling thread to a set of logical CPUs.

```rust
// A latency-sensitive worker: pin it to a core and pre-allocate its buffer
// up front, so its first log call is as cheap as the rest.
let worker = std::thread::spawn(|| {
    ticklog::pin_thread(&[3]);
    ticklog::warm_up().unwrap();

    // hot loop...
});
```

## License

MIT OR Apache-2.0
