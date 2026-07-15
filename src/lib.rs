//! Ticklog is a fast, minimal logging library for latency-critical Rust
//! applications such as high-frequency trading, where the cost of a log call on
//! the hot path must stay in the low tens of nanoseconds.
//!
//! # How it works
//!
//! A logging macro runs entirely on the calling thread's **hot path**: it checks
//! the level, then encodes a compact binary record into that thread's private
//! lock-free buffer and returns. Format and source strings are stored by pointer
//! and arguments in their native form, so no text formatting happens here.
//!
//! A background **drain** thread does the rest: it decodes each record, formats
//! and timestamps it, and writes the text to a [`LogSink`], keeping all of that
//! cost off the calling thread.
//!
//! # Benchmarks
//!
//! Per-call latency (p50, criterion). Lower is better.
//!
//! | Name    | Log call                                           |
//! | ------- | -------------------------------------------------- |
//! | one_u64 | `info!("x={}", 42u64)`                             |
//! | one_str | `info!("{}", "hello world")`                       |
//! | mixed   | `info!("{} {} {}", 42u64, 3.14159, "hello world")` |
//!
//! **Mac M4** (Apple M4, 4.4 GHz, macOS 15):
//!
//! | Logger      | one_u64 | one_str |  mixed |
//! | ----------- | ------: | ------: | -----: |
//! | **ticklog** | 5.2 ns  | 5.9 ns  | 7.0 ns |
//! | env_logger  |  231 ns |  232 ns | 307 ns |
//! | slog        |  274 ns |  269 ns | 454 ns |
//! | tracing     |  386 ns |  425 ns | 458 ns |
//!
//! **Granite Rapids** (Intel Xeon 6982P-C, 3.9 GHz, Ubuntu 24.04):
//!
//! | Logger      | one_u64 | one_str |  mixed |
//! | ----------- | ------: | ------: | -----: |
//! | **ticklog** | 8.6 ns  | 8.4 ns  | 9.9 ns |
//! | env_logger  |  370 ns |  371 ns | 491 ns |
//! | slog        |  499 ns |  453 ns | 686 ns |
//! | tracing     |  837 ns |  854 ns | 937 ns |
//!
//! # Quick start
//!
//! ```no_run
//! use ticklog::{info, FileSink};
//!
//! // Configure ticklog and keep the returned guard alive for as long as you
//! // want to log.
//! let _guard = ticklog::configure! {
//!     sink: FileSink::new("app.log").unwrap(),
//! }
//! .unwrap();
//!
//! info!("listening on {}", 8080);
//! ```
//!
//! [`configure!`] may be called only once per process and returns a
//! [`Guard`]. **Hold the guard for as long as you want to log:** when it is
//! dropped it flushes the sink, stops the background drain thread, and marks
//! every ring dead so subsequent log calls become silent no-ops.
//!
//! # Logging macros
//!
//! [`trace!`], [`debug!`], [`info!`], [`warn!`], and [`error!`] take a format
//! string literal followed by positional arguments:
//!
//! ```no_run
//! # use ticklog::{info, error};
//! # let _guard = ticklog::configure! {}.unwrap();
//! info!("connected to {}", "db-01");
//! error!("request {} failed after {} retries", 42, 3);
//! ```
//!
//! Placeholders are `{}` (Display) and `{:?}` (Debug). The number of
//! placeholders is checked against the number of arguments **at compile time**,
//! so a mismatch is a build error rather than a runtime surprise.
//!
//! # Configuration
//!
//! [`configure!`] accepts these keys, each optional:
//!
//! - `sink`: where output goes. Defaults to a [`ConsoleSink`] on stderr
//!   (colored when stderr is a terminal).
//! - `max_level`: the level ceiling; records above it are dropped on the hot
//!   path before any encoding. Defaults to [`Level::Info`].
//! - `backpressure`: what a logging thread does when its buffer is full, either
//!   [`Backpressure::Drop`] (the default, never blocks) or
//!   [`Backpressure::Block`] (spin until space frees up).
//! - `timezone_offset`: offset applied when formatting timestamps. Defaults to
//!   UTC (`0`).
//! - `drain_affinity`: pin the drain thread to a set of logical CPUs
//!   (`Option<Vec<usize>>`). Defaults to `None`.
//!
//! # Sinks
//!
//! A [`LogSink`] is the final destination for formatted lines. The crate ships:
//!
//! - [`ConsoleSink`]: stdout or stderr, with automatic or forced ANSI coloring
//!   by level (see [`ColorMode`]).
//! - [`FileSink`]: a buffered single file, opened in append or truncate mode.
//! - [`WriterSink`]: wraps any [`std::io::Write`]; the escape hatch for custom
//!   destinations such as rotating files or the network.
//!
//! Compose and filter sinks with [`FanOut`] (dispatch one record to several
//! sinks) and [`LogSinkExt::with_max_level`] (limit a sink to a level and below):
//!
//! ```
//! use ticklog::{ConsoleSink, FanOut, Level, LogSinkExt};
//!
//! let sink = FanOut::new()
//!     .add(ConsoleSink::stderr().with_max_level(Level::Warn))
//!     .add(ConsoleSink::stdout().with_max_level(Level::Info));
//! ```
//!
//! # Threads
//!
//! Any thread may log, and each allocates its own buffer on first use. To
//! move that one-time allocation off a latency-sensitive path, call [`warm_up`]
//! on the thread before its first log call.
//!
//! # Safety
//!
//! The public API contains no `unsafe` functions. Internally, `unsafe` is
//! confined to three areas, each with a documented invariant:
//!
//! - **Thread-local buffer access:** per-thread buffers live in an
//!   [`UnsafeCell`](std::cell::UnsafeCell) guarded by a re-entrancy flag. A
//!   re-entrant log call on the same thread is detected and refused before it
//!   can form a second mutable reference, preventing aliasing UB.
//! - **Lock-free ring buffer:** the buffer shared between the calling thread
//!   and the drain thread uses atomic ordering to coordinate access without
//!   locks. The calling thread only writes; the drain thread only reads.
//! - **Affinity syscalls:** platform thread-affinity calls require raw pointer
//!   and FFI usage, gated behind `#[cfg(target_os)]`.

#![deny(unsafe_op_in_unsafe_fn)]

mod affinity;
mod builder;
mod drain;
mod encode;
mod error;
mod format;
mod guard;
mod level;
mod macros;
mod record;
mod ring;
mod sink;
mod thread_buf;
mod timestamp;
pub use affinity::pin_thread;
pub use builder::Backpressure;
pub use error::TicklogError;
pub use guard::Guard;
pub use level::Level;
pub use sink::{
    ColorMode, ConsoleSink, FanOut, FileSink, LogSink, LogSinkExt, WithLevel, WriterSink,
};
pub use thread_buf::warm_up;

/// Internal helpers used by the logging macros. Not part of the public API and
/// exempt from semver guarantees; referenced only through `$crate::__private`
/// in macro expansions.
#[doc(hidden)]
pub mod __private {
    pub use crate::builder::__configure_rt;
    pub use crate::encode::LoggableArgs;
    pub use crate::format::check_fmt;
    pub use crate::macros::dispatch;
    pub use crate::record::BASE_RECORD_SIZE;
}
