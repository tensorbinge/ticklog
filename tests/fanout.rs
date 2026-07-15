//! Per-sink level filtering through `FanOut`: with the global ceiling open to
//! `Trace`, every record reaches the fan-out, and each inner sink receives only
//! the records at or below its own `max_level`.
//!
//! This is the level check that lives on the drain side (in `FanOut::accept`),
//! distinct from the producer-side global ceiling exercised in `level_filter`.

use std::io;
use std::sync::{Arc, Mutex};

use ticklog::{debug, error, info, trace, warn, FanOut, Level, LogSink, LogSinkExt};

/// Captures the level of every record it accepts.
struct CaptureSink {
    levels: Arc<Mutex<Vec<Level>>>,
}

impl LogSink for CaptureSink {
    fn accept(&mut self, _line: &[u8], level: Level) -> io::Result<()> {
        self.levels.lock().unwrap().push(level);
        Ok(())
    }
}

/// Builds a capture sink and returns it alongside a handle to inspect after the
/// sink has been moved into the fan-out.
fn capture() -> (CaptureSink, Arc<Mutex<Vec<Level>>>) {
    let levels = Arc::new(Mutex::new(Vec::new()));
    let sink = CaptureSink {
        levels: Arc::clone(&levels),
    };
    (sink, levels)
}

#[test]
fn fanout_routes_each_record_by_per_sink_level() {
    let (err_sink, err_levels) = capture();
    let (info_sink, info_levels) = capture();
    let (trace_sink, trace_levels) = capture();

    let fan = FanOut::new()
        .add(err_sink.with_max_level(Level::Error))
        .add(info_sink.with_max_level(Level::Info))
        .add(trace_sink.with_max_level(Level::Trace));

    // The global ceiling admits every level, so filtering happens only in the
    // fan-out, per inner sink.
    let guard = ticklog::configure! {
        sink: fan,
        max_level: Level::Trace,
    }
    .expect("first configure in a fresh process must succeed");

    error!("e");
    warn!("w");
    info!("i");
    debug!("d");
    trace!("t");

    drop(guard);

    // Error sink: only Error (1) is <= its ceiling.
    assert_eq!(*err_levels.lock().unwrap(), vec![Level::Error]);

    // Info sink: Error, Warn, Info are <= Info.
    assert_eq!(
        *info_levels.lock().unwrap(),
        vec![Level::Error, Level::Warn, Level::Info]
    );

    // Trace sink: every level is <= Trace.
    assert_eq!(
        *trace_levels.lock().unwrap(),
        vec![
            Level::Error,
            Level::Warn,
            Level::Info,
            Level::Debug,
            Level::Trace
        ]
    );
}
