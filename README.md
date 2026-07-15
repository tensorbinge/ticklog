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

[`configure!`] returns a `Guard`. Keep it alive for as long as you want to log: when it is dropped it flushes the sink, stops the background thread, and disables logging, so every log call afterwards is a silent no-op.

## Benchmarks

Per-call latency on a Mac (M4, macOS 15, Rust 1.85, `release` profile). Lower is better.

| Logger      | info!("x={}", 42u64) | info!("{}", "hello world") | info!("{} {} {}", 42u64, 3.14159, "hello world") |
| ----------- | -------------------: | -------------------------: | -----------------------------------------------: |
| **ticklog** |           **5.2 ns** |                 **5.9 ns** |                                       **7.0 ns** |
| env_logger  |               231 ns |                     232 ns |                                           307 ns |
| slog        |               274 ns |                     269 ns |                                           454 ns |
| tracing     |               386 ns |                     425 ns |                                           458 ns |

## Configuration

[`configure!`] accepts these keys, each optional:

| Key | Purpose | Default |
| --- | ------- | ------- |
| `sink` | Where output goes. | `ConsoleSink` on stderr |
| `max_level` | Records above this level are dropped on the calling thread before any encoding. | `Level::Info` |
| `backpressure` | What a logging thread does when its buffer is full. | `Backpressure::Drop` |
| `timezone_offset` | Seconds east of UTC, applied to timestamp formatting only. | `0` (UTC) |
| `drain_affinity` | Pin the background thread to a set of logical CPUs. | none |

Example with every key:

```rust
use ticklog::{configure, ConsoleSink, Level, Backpressure};

let _guard = configure! {
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
