//! Regression: each logging-macro argument expression is evaluated exactly
//! once.
//!
//! The macro used to expand each `$arg` at four sites (buffer sizing, tag,
//! payload sizing, payload write), so a side-effecting argument ran four times
//! and the *last* evaluation's value was the one encoded. Logging
//! `ctr.fetch_add(1, ..)` incremented the counter to 4 and emitted `seq=3`.
//!
//! A second case logs many arguments at once, guarding against any arity ceiling
//! in the fix (the encoding must stay variadic).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ticklog::{info, Level, LogSink};

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
fn each_argument_is_evaluated_once_and_arity_is_unbounded() {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = CaptureSink {
        lines: Arc::clone(&lines),
    };

    let guard = ticklog::configure! {
        sink: sink,
        max_level: Level::Trace,
    }
    .expect("first configure in a fresh process must succeed");

    // A side-effecting argument: `fetch_add` returns the pre-increment value and
    // bumps the counter. Evaluated once, it returns 0 and leaves the counter at 1.
    let ctr = AtomicU64::new(0);
    info!("seq={}", ctr.fetch_add(1, Ordering::Relaxed));

    // Many arguments in one call: the encoding must remain variadic.
    info!(
        "{} {} {} {} {} {} {} {} {} {} {} {}",
        0u64, 1u64, 2u64, 3u64, 4u64, 5u64, 6u64, 7u64, 8u64, 9u64, 10u64, 11u64
    );

    drop(guard);

    let captured = lines.lock().unwrap();

    // Exactly one evaluation: the counter advanced once, and the encoded value is
    // the pre-increment 0 (not a later evaluation's 3).
    assert_eq!(
        ctr.load(Ordering::Relaxed),
        1,
        "argument must be evaluated once, not multiple times"
    );
    assert!(
        captured[0].ends_with("seq=0"),
        "encoded value must come from the single evaluation; got {:?}",
        captured[0]
    );

    // The variadic call renders every argument, in order.
    assert!(
        captured[1].ends_with("0 1 2 3 4 5 6 7 8 9 10 11"),
        "all arguments must be encoded; got {:?}",
        captured[1]
    );
}
