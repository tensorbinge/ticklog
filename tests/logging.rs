//! End-to-end: the logging macros produce formatted lines at the sink.
//!
//! This is an integration test (its own binary), so the process-global
//! `REGISTRY` starts fresh and `configure!` succeeds exactly once -- every
//! assertion runs under a single guard.

use std::sync::{Arc, Mutex};

use ticklog::{Level, LogSink, debug, error, info, trace, warn};

/// A sink that records every accepted line, for assertions after shutdown.
struct CaptureSink {
    lines: Arc<Mutex<Vec<String>>>,
}

impl LogSink for CaptureSink {
    fn accept(&mut self, line: &[u8], _level: Level) -> std::io::Result<()> {
        self.lines
            .lock()
            .unwrap()
            .push(String::from_utf8_lossy(line).into_owned());
        Ok(())
    }
}

#[test]
fn macros_produce_formatted_lines_in_order() {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = CaptureSink {
        lines: Arc::clone(&lines),
    };

    let guard = ticklog::configure! {
        sink: sink,
        max_level: Level::Trace,
    }
    .expect("first configure in a fresh process must succeed");

    // One producer thread (this one) writes to a single ring, so the drain
    // emits these in the order they were logged.
    info!("plain message");
    warn!("value = {}", 42u64);
    error!("{} and {}", "a", "b");
    debug!("dbg {}", true);
    trace!("trc {}", 3.5f64);

    // Dropping the guard flushes and joins the drain, so every line is present.
    drop(guard);

    let captured = lines.lock().unwrap();
    assert_eq!(captured.len(), 5, "expected five lines, got {captured:?}");

    assert!(captured[0].contains(" INFO "));
    assert!(captured[0].ends_with("plain message"));
    assert!(captured[1].contains(" WARN "));
    assert!(captured[1].ends_with("value = 42"));
    assert!(captured[2].contains(" ERROR "));
    assert!(captured[2].ends_with("a and b"));
    assert!(captured[3].contains(" DEBUG "));
    assert!(captured[3].ends_with("dbg true"));
    assert!(captured[4].contains(" TRACE "));
    assert!(captured[4].ends_with("trc 3.5"));

    // Every line carries this test file's source location (default metadata).
    // Normalise Windows backslash paths so the assertion is portable.
    let line = captured[0].replace('\\', "/");
    assert!(line.contains("tests/logging.rs:"));
}
