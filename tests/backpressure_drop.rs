//! Backpressure under `Backpressure::Drop`: when a producer outruns a stalled
//! drain and its ring fills, the newest records are discarded and the producer
//! never blocks.
//!
//! The test stalls the drain by blocking inside the sink's first `accept`. The
//! drain publishes `tail` only after `accept` returns (drain.rs), so a blocked
//! sink freezes the ring: the producer fills it with a contiguous prefix of
//! records and drops every record after it. Opening the gate then lets that
//! prefix flush in order, which is what the assertions check.
//!
//! Each integration test is its own binary, so this file's single `configure!`
//! owns the process-global registry for the whole run.

use std::io;
use std::sync::{Arc, Condvar, Mutex};

use ticklog::{Backpressure, Level, LogSink, info};

/// More records than any ring can hold, so saturation and drops are guaranteed
/// on every platform (a 1 MB ring holds at most ~16k of these records).
const PRODUCED: usize = 200_000;

/// A one-shot gate: the drain blocks on `wait` until the test calls `open`.
struct Gate {
    open: Mutex<bool>,
    ready: Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            open: Mutex::new(false),
            ready: Condvar::new(),
        }
    }

    fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.ready.wait(open).unwrap();
        }
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.ready.notify_all();
    }
}

/// A sink that blocks on its first `accept` until the gate opens, stalling the
/// drain, then records every line it receives.
struct GateSink {
    lines: Arc<Mutex<Vec<String>>>,
    gate: Arc<Gate>,
    passed: bool,
}

impl LogSink for GateSink {
    fn accept(&mut self, line: &[u8], _level: Level) -> io::Result<()> {
        if !self.passed {
            self.gate.wait();
            self.passed = true;
        }
        self.lines
            .lock()
            .unwrap()
            .push(String::from_utf8_lossy(line).into_owned());
        Ok(())
    }
}

/// Extracts the `i` from a line ending in `seq=<i>`.
fn seq_of(line: &str) -> Option<usize> {
    line.rsplit("seq=").next()?.trim().parse().ok()
}

#[test]
fn drop_policy_discards_newest_records_and_never_blocks() {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(Gate::new());
    let sink = GateSink {
        lines: Arc::clone(&lines),
        gate: Arc::clone(&gate),
        passed: false,
    };

    let guard = ticklog::configure! {
        sink: sink,
        max_level: Level::Info,
        backpressure: Backpressure::Drop,
    }
    .expect("first configure in a fresh process must succeed");

    // The drain is stalled on the gate, so the ring fills and the tail never
    // advances. Under Drop this loop never blocks; if it did, the test would
    // hang here rather than reach the assertions.
    for i in 0..PRODUCED {
        info!("seq={}", i as u64);
    }

    // Let the drain deliver the records the ring retained, then flush on drop.
    gate.open();
    drop(guard);

    let captured = lines.lock().unwrap();
    let received = captured.len();

    // Saturation dropped the tail of the sequence.
    assert!(received > 0, "expected some records to survive");
    assert!(
        received < PRODUCED,
        "Drop must discard under saturation, but all {PRODUCED} survived"
    );

    // The survivors are the contiguous prefix 0..received, in order: the ring
    // kept the earliest records and dropped every later one, and the drain
    // emits a single ring in write order.
    let seqs: Vec<usize> = captured
        .iter()
        .map(|line| seq_of(line).expect("each line ends in seq=<i>"))
        .collect();
    let expected: Vec<usize> = (0..received).collect();
    assert_eq!(seqs, expected, "survivors must be an in-order prefix");
}
