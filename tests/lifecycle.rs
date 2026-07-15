//! Producer-thread lifecycle: a spawned thread that logs and then exits leaves
//! a "dead" ring behind, and the drain still delivers its records. Dropping the
//! `Guard` performs the final drain pass that flushes every remaining ring.
//!
//! When a producer thread exits, its thread-local `ThreadBuf` is dropped, which
//! marks the ring not-live (thread_buf.rs). The drain gives each dead ring one
//! last drain before releasing it (drain.rs), and the guard's shutdown triggers
//! that final pass -- so a short-lived worker never loses records.

use std::io;
use std::sync::{Arc, Mutex};
use std::thread;

use ticklog::{info, Level, LogSink};

/// Records every accepted line for post-shutdown assertions.
struct CaptureSink {
    lines: Arc<Mutex<Vec<String>>>,
}

impl LogSink for CaptureSink {
    fn accept(&mut self, line: &[u8], _level: Level) -> io::Result<()> {
        self.lines
            .lock()
            .unwrap()
            .push(String::from_utf8_lossy(line).into_owned());
        Ok(())
    }
}

fn any_line_contains(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|line| line.contains(needle))
}

#[test]
fn worker_thread_records_survive_its_exit_and_guard_shutdown() {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = CaptureSink {
        lines: Arc::clone(&lines),
    };

    let guard = ticklog::configure! {
        sink: sink,
        max_level: Level::Info,
    }
    .expect("first configure in a fresh process must succeed");

    info!("main before worker");

    // A short-lived producer: it owns a distinct ring, logs into it, then exits.
    // Joining guarantees its ThreadBuf has dropped (ring marked not-live) before
    // the guard's final drain runs.
    let worker = thread::spawn(|| {
        info!("worker record {}", 7);
    });
    worker.join().expect("worker thread must not panic");

    info!("main after worker");

    // The final drain on guard drop must pick up the dead worker ring and flush
    // both live and dead rings.
    drop(guard);

    let captured = lines.lock().unwrap();
    assert!(
        any_line_contains(&captured, "worker record 7"),
        "the exited worker's record must still be delivered, got {captured:?}"
    );
    assert!(any_line_contains(&captured, "main before worker"));
    assert!(any_line_contains(&captured, "main after worker"));
}
