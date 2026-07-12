//! Backpressure under `Backpressure::Block`: a producer that outruns the drain
//! spins until the drain frees ring space, so no record is ever lost even when
//! the total volume far exceeds the ring's capacity.
//!
//! The drain runs normally here (a fast capture sink), so a blocked producer
//! always unblocks once the drain advances `tail`. The test produces many times
//! the ring's capacity and asserts every record arrives, in order -- the
//! property that distinguishes Block from Drop.

use std::io;
use std::sync::{Arc, Mutex};

use ticklog::{info, Backpressure, Level, LogSink};

/// Several times a 1 MB ring's capacity, so the producer must block and wait
/// for the drain repeatedly during the run.
const PRODUCED: usize = 50_000;

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

/// Extracts the `i` from a line ending in `seq=<i>`.
fn seq_of(line: &str) -> Option<usize> {
    line.rsplit("seq=").next()?.trim().parse().ok()
}

#[test]
fn block_policy_never_loses_a_record() {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = CaptureSink {
        lines: Arc::clone(&lines),
    };

    let guard = ticklog::builder()
        .sink(sink)
        .max_level(Level::Info)
        .backpressure(Backpressure::Block)
        .build()
        .expect("first build in a fresh process must succeed");

    for i in 0..PRODUCED {
        info!("seq={}", i as u64);
    }

    // Flush the remainder and join the drain.
    drop(guard);

    let captured = lines.lock().unwrap();
    assert_eq!(
        captured.len(),
        PRODUCED,
        "Block must not drop any record; lost {}",
        PRODUCED - captured.len()
    );

    // A single producer thread writes one ring, and the drain emits a ring in
    // write order, so the whole sequence arrives contiguous and ordered.
    let seqs: Vec<usize> = captured
        .iter()
        .map(|line| seq_of(line).expect("each line ends in seq=<i>"))
        .collect();
    let expected: Vec<usize> = (0..PRODUCED).collect();
    assert_eq!(seqs, expected, "records must arrive in program order");
}
