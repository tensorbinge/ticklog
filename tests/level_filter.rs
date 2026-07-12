//! The global level ceiling filters on the producer side: with
//! `max_level(Error)`, a record above `Error` is discarded by the logging macro
//! before its arguments are evaluated, so it never reaches the ring or the sink.

use std::io;
use std::sync::{Arc, Mutex};

use ticklog::{debug, error, info, trace, warn, Level, LogSink};

/// Captures every accepted `(line, level)` pair.
struct CaptureSink {
    calls: Arc<Mutex<Vec<(String, Level)>>>,
}

impl LogSink for CaptureSink {
    fn accept(&mut self, line: &[u8], level: Level) -> io::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push((String::from_utf8_lossy(line).into_owned(), level));
        Ok(())
    }
}

#[test]
fn max_level_error_admits_only_error_records() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sink = CaptureSink {
        calls: Arc::clone(&calls),
    };

    let guard = ticklog::builder()
        .sink(sink)
        .max_level(Level::Error)
        .build()
        .expect("first build in a fresh process must succeed");

    // Only `error!` is at or below the Error ceiling; the rest are dropped at
    // the macro before dispatch.
    error!("kept {}", 1);
    warn!("dropped {}", 2);
    info!("dropped {}", 3);
    debug!("dropped {}", 4);
    trace!("dropped {}", 5);

    drop(guard);

    let recorded = calls.lock().unwrap();
    assert_eq!(
        recorded.len(),
        1,
        "only the Error record should reach the sink, got {recorded:?}"
    );
    assert_eq!(recorded[0].1, Level::Error);
    assert!(recorded[0].0.ends_with("kept 1"));
    assert!(recorded[0].0.contains(" ERROR "));
}
