//! Traits and adapters for log output destinations.
//!
//! A [`LogSink`] is the final recipient of formatted log lines. The crate ships
//! three concrete sinks: [`ConsoleSink`] (the default), [`FileSink`], and the
//! [`WriterSink`] escape hatch for any [`io::Write`]. It also provides a
//! level-filtering adapter ([`WithLevel`]) and a multi-sink fan-out ([`FanOut`]).

use std::io;

use crate::level::Level;

mod console;
mod file;
mod writer;

pub use console::{ColorMode, ConsoleSink};
pub use file::FileSink;
pub use writer::WriterSink;

/// A destination for formatted log lines.
///
/// The `line` passed to [`accept`](LogSink::accept) is valid UTF-8 without a
/// trailing newline. To use any [`io::Write`] as a sink, wrap it in
/// [`WriterSink`].
pub trait LogSink: Send + 'static {
    /// Writes a formatted log line to this sink.
    ///
    /// `line` is valid UTF-8. No trailing newline is included; sinks that
    /// require one must append it.
    fn accept(&mut self, line: &[u8], level: Level) -> io::Result<()>;

    /// Maximum [`Level`] this sink accepts.
    ///
    /// Records with a level strictly greater than this value are not passed
    /// to [`accept`](LogSink::accept). The default is [`Level::Trace`]
    /// (accepts all levels).
    fn max_level(&self) -> Level {
        Level::Trace
    }

    /// Flushes any buffered output to the underlying destination.
    ///
    /// Called when logging goes idle and once more at shutdown, so a buffered
    /// sink never holds a record indefinitely. The default is a no-op, for
    /// sinks that do not buffer.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A [`LogSink`] adapter that overrides the maximum level of an inner sink.
pub struct WithLevel<S> {
    sink: S,
    level: Level,
}

impl<S: LogSink> LogSink for WithLevel<S> {
    fn accept(&mut self, line: &[u8], level: Level) -> io::Result<()> {
        self.sink.accept(line, level)
    }

    fn max_level(&self) -> Level {
        self.level
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
}

/// Extension trait for [`LogSink`] providing per-sink level filtering.
///
/// ```
/// use ticklog::{ConsoleSink, Level, LogSinkExt};
///
/// let sink = ConsoleSink::stdout().with_max_level(Level::Info);
/// ```
pub trait LogSinkExt: LogSink + Sized {
    /// Wraps this sink in a [`WithLevel`] adapter that limits accepted
    /// records to `level` and below.
    fn with_max_level(self, level: Level) -> WithLevel<Self> {
        WithLevel { sink: self, level }
    }
}

impl<T: LogSink + Sized> LogSinkExt for T {}

/// A sink that dispatches to multiple inner sinks.
///
/// Each inner sink receives records at or below its own
/// [`max_level`](LogSink::max_level). If an inner sink returns an error,
/// the error is logged to stderr and dispatch continues to the remaining
/// sinks.
///
/// ```
/// use ticklog::{ConsoleSink, FanOut, Level, LogSinkExt};
///
/// let fan = FanOut::new()
///     .add(ConsoleSink::stdout().with_max_level(Level::Info))
///     .add(ConsoleSink::stderr().with_max_level(Level::Error));
/// ```
pub struct FanOut {
    entries: Vec<Box<dyn LogSink>>,
}

impl FanOut {
    /// Creates an empty fan-out sink.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Adds a sink to the fan-out set.
    // `add` is the idiomatic builder verb here; it is not `std::ops::Add`.
    #[allow(clippy::should_implement_trait)]
    pub fn add<S: LogSink>(mut self, sink: S) -> Self {
        self.entries.push(Box::new(sink));
        self
    }
}

impl Default for FanOut {
    /// Creates an empty fan-out sink, the same as [`FanOut::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl LogSink for FanOut {
    fn accept(&mut self, line: &[u8], level: Level) -> io::Result<()> {
        for entry in &mut self.entries {
            if level <= entry.max_level() {
                if let Err(e) = entry.accept(line, level) {
                    eprintln!("ticklog: sink error: {}", e);
                }
            }
        }
        Ok(())
    }

    fn max_level(&self) -> Level {
        Level::Trace
    }

    fn flush(&mut self) -> io::Result<()> {
        for entry in &mut self.entries {
            if let Err(e) = entry.flush() {
                eprintln!("ticklog: sink flush failed: {}", e);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// Shared handle to the `(line, level)` pairs a [`SpySink`] recorded.
    type SpyCalls = Arc<Mutex<Vec<(Vec<u8>, Level)>>>;

    /// A [`LogSink`] that records every `accept` call into a shared
    /// `Arc<Mutex<Vec>>`. The calls handle is returned at construction
    /// so tests can inspect recorded data after the sink is moved into a
    /// [`FanOut`] or [`WithLevel`].
    struct SpySink {
        calls: SpyCalls,
        error_on_next: Cell<bool>,
        max_level: Level,
    }

    impl SpySink {
        fn new(max_level: Level) -> (Self, SpyCalls) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let sink = Self {
                calls: Arc::clone(&calls),
                error_on_next: Cell::new(false),
                max_level,
            };
            (sink, calls)
        }
    }

    impl LogSink for SpySink {
        fn accept(&mut self, line: &[u8], level: Level) -> io::Result<()> {
            if self.error_on_next.get() {
                self.error_on_next.set(false);
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "spy error"));
            }
            self.calls.lock().unwrap().push((line.to_vec(), level));
            Ok(())
        }

        fn max_level(&self) -> Level {
            self.max_level
        }
    }

    /// A [`LogSink`] that counts `flush` calls into a shared counter, so tests
    /// can assert an adapter forwards flush to its inner sink(s).
    struct FlushSpy(Arc<AtomicUsize>);

    impl LogSink for FlushSpy {
        fn accept(&mut self, _line: &[u8], _level: Level) -> io::Result<()> {
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn trait_is_object_safe() {
        fn _accepts_boxed(_s: Box<dyn LogSink>) {}
        let (spy, _calls) = SpySink::new(Level::Trace);
        _accepts_boxed(Box::new(spy));
    }

    #[test]
    fn trait_requires_send_and_static() {
        fn _assert_send<T: Send>() {}
        fn _assert_static<T: 'static>() {}
        _assert_send::<Box<dyn LogSink>>();
        _assert_static::<Box<dyn LogSink>>();
    }

    #[test]
    fn default_max_level_is_trace() {
        let (spy, _calls) = SpySink::new(Level::Trace);
        assert_eq!(spy.max_level(), Level::Trace);
    }

    #[test]
    fn default_flush_is_noop() {
        let (mut spy, _calls) = SpySink::new(Level::Trace);
        // SpySink does not override flush, so the default no-op runs and is Ok.
        assert!(spy.flush().is_ok());
    }

    #[test]
    fn with_level_overrides_max_level() {
        let (inner, _calls) = SpySink::new(Level::Trace);
        let wrapped = inner.with_max_level(Level::Error);
        assert_eq!(wrapped.max_level(), Level::Error);
    }

    #[test]
    fn with_level_delegates_accept() {
        let (inner, calls) = SpySink::new(Level::Trace);
        let mut wrapped = WithLevel {
            sink: inner,
            level: Level::Info,
        };
        wrapped.accept(b"test message", Level::Debug).unwrap();
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, b"test message");
        assert_eq!(recorded[0].1, Level::Debug);
    }

    #[test]
    fn with_level_forwards_flush() {
        let flushes = Arc::new(AtomicUsize::new(0));
        let mut wrapped = FlushSpy(Arc::clone(&flushes)).with_max_level(Level::Info);
        wrapped.flush().unwrap();
        assert_eq!(flushes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn log_sink_ext_with_max_level_returns_with_level() {
        let (inner, _calls) = SpySink::new(Level::Trace);
        let wrapped = inner.with_max_level(Level::Warn);
        assert_eq!(wrapped.max_level(), Level::Warn);
    }

    #[test]
    fn fan_out_empty_accept_is_noop() {
        let mut fan = FanOut::new();
        let result = fan.accept(b"hello", Level::Info);
        assert!(result.is_ok());
    }

    #[test]
    fn fan_out_dispatches_to_all_sinks() {
        let (s1, s1_calls) = SpySink::new(Level::Trace);
        let (s2, s2_calls) = SpySink::new(Level::Trace);
        let mut fan = FanOut::new().add(s1).add(s2);
        fan.accept(b"hello", Level::Info).unwrap();
        assert_eq!(s1_calls.lock().unwrap().len(), 1);
        assert_eq!(s2_calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn fan_out_respects_per_sink_level_filtering() {
        let (s_trace, trace_calls) = SpySink::new(Level::Trace);
        let (s_error, error_calls) = SpySink::new(Level::Error);
        let mut fan = FanOut::new().add(s_trace).add(s_error);
        // Debug (4) <= Trace (5): accepted. Debug (4) <= Error (1): filtered.
        fan.accept(b"debug msg", Level::Debug).unwrap();
        assert_eq!(trace_calls.lock().unwrap().len(), 1);
        assert_eq!(error_calls.lock().unwrap().len(), 0);
    }

    #[test]
    fn fan_out_error_isolation() {
        let (s_bad, bad_calls) = SpySink::new(Level::Trace);
        s_bad.error_on_next.set(true);
        let (s_ok, ok_calls) = SpySink::new(Level::Trace);

        let mut fan = FanOut::new().add(s_bad).add(s_ok);
        let result = fan.accept(b"test", Level::Info);
        // FanOut always returns Ok; errors are logged to stderr.
        assert!(result.is_ok());
        // The bad sink recorded no calls (error fired before push).
        assert_eq!(bad_calls.lock().unwrap().len(), 0);
        // The ok sink still received the record.
        assert_eq!(ok_calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn fan_out_max_level_is_always_trace() {
        let fan = FanOut::new()
            .add(SpySink::new(Level::Error).0)
            .add(SpySink::new(Level::Warn).0);
        assert_eq!(fan.max_level(), Level::Trace);
    }

    #[test]
    fn fan_out_flushes_every_entry() {
        let flushes = Arc::new(AtomicUsize::new(0));
        let mut fan = FanOut::new()
            .add(FlushSpy(Arc::clone(&flushes)))
            .add(FlushSpy(Arc::clone(&flushes)));
        fan.flush().unwrap();
        assert_eq!(flushes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn level_ordering_accepts_equal() {
        let (spy, _calls) = SpySink::new(Level::Info);
        let wrapped = spy.with_max_level(Level::Info);
        assert!(Level::Info <= wrapped.max_level());
    }

    #[test]
    fn level_ordering_rejects_higher() {
        let (spy, _calls) = SpySink::new(Level::Info);
        let wrapped = spy.with_max_level(Level::Info);
        assert!(Level::Debug > wrapped.max_level());
    }

    #[test]
    fn level_ordering_accepts_lower() {
        let (spy, _calls) = SpySink::new(Level::Info);
        let wrapped = spy.with_max_level(Level::Info);
        assert!(Level::Error <= wrapped.max_level());
    }
}
