//! Shared bench setup: a sink that discards every record, and ticklog
//! initialized against it.

use std::io;

use ticklog::{Level, LogSink};

/// Discards every record with no work, so a benchmark measures only the work
/// under test.
pub struct NullSink;

impl LogSink for NullSink {
    fn accept(&mut self, _line: &[u8], _level: Level) -> io::Result<()> {
        Ok(())
    }
}

/// Initializes ticklog with a [`NullSink`] at [`Level::Trace`] and leaks the
/// guard so logging stays active for the rest of the process.
///
/// `configure!` runs once per process, so each bench binary calls this at most
/// once. Leaking the guard is deliberate: dropping it would stop logging
/// partway through a run.
///
/// Panics if ticklog is already initialized, which would mean a second call.
pub fn init_ticklog(drain_affinity: Option<Vec<usize>>) {
    let guard = ticklog::configure! {
        sink: NullSink,
        max_level: ticklog::Level::Trace,
        drain_affinity: drain_affinity,
    }
    .expect("ticklog build");
    std::mem::forget(guard);
}
