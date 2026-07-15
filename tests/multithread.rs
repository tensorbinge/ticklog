//! Concurrent producers: many threads log at once, each into its own ring.
//! ticklog guarantees per-thread (intra-ring) order, not a global total order,
//! so the test asserts that each thread's own records arrive complete and in
//! program order, with no interleaving corruption across threads.
//!
//! `Backpressure::Block` guarantees no record is dropped, so each thread's
//! sequence must arrive as the contiguous run `0..RECORDS_PER_THREAD`.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::thread;

use ticklog::{info, Backpressure, Level, LogSink};

const THREADS: usize = 8;
const RECORDS_PER_THREAD: usize = 1_000;

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

/// Parses the `(worker, seq)` pair from a line ending in `worker=<w> seq=<s>`.
fn parse_worker_seq(line: &str) -> Option<(usize, usize)> {
    let after = line.split("worker=").nth(1)?;
    let mut parts = after.split(" seq=");
    let worker = parts.next()?.trim().parse().ok()?;
    let seq = parts.next()?.trim().parse().ok()?;
    Some((worker, seq))
}

#[test]
fn each_thread_sequence_arrives_ordered_and_complete() {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = CaptureSink {
        lines: Arc::clone(&lines),
    };

    let guard = ticklog::configure! {
        sink: sink,
        max_level: Level::Info,
        backpressure: Backpressure::Block,
    }
    .expect("first configure in a fresh process must succeed");

    let handles: Vec<_> = (0..THREADS)
        .map(|worker| {
            thread::spawn(move || {
                for seq in 0..RECORDS_PER_THREAD {
                    info!("worker={} seq={}", worker as u64, seq as u64);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("worker thread must not panic");
    }

    // Final drain flushes every ring, including the exited workers'.
    drop(guard);

    // Group the captured sequence numbers by worker, preserving arrival order.
    let captured = lines.lock().unwrap();
    let mut per_worker: HashMap<usize, Vec<usize>> = HashMap::new();
    for line in captured.iter() {
        let (worker, seq) =
            parse_worker_seq(line).unwrap_or_else(|| panic!("unparseable line: {line:?}"));
        per_worker.entry(worker).or_default().push(seq);
    }

    assert_eq!(
        per_worker.len(),
        THREADS,
        "every worker must contribute records"
    );

    let expected: Vec<usize> = (0..RECORDS_PER_THREAD).collect();
    for worker in 0..THREADS {
        let seqs = per_worker
            .get(&worker)
            .unwrap_or_else(|| panic!("worker {worker} produced no records"));
        // Contiguous and ordered proves both intra-thread ordering and that
        // Block dropped nothing; any cross-thread corruption would break this.
        assert_eq!(
            seqs, &expected,
            "worker {worker} records out of order or incomplete"
        );
    }
}
