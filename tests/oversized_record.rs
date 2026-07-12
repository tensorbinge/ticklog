//! Regression: a record too large for the u16 `total_size` field must be
//! dropped, never written with a truncated header.
//!
//! Without the size gate in `dispatch`, logging a string of ~70 KB makes
//! `total_size` (70_044) overflow the u16 header field: it is stored as
//! `70_044 & 0xFFFF = 4508`, but the producer still copies all 70_044 bytes into
//! the ring and advances `head` by the true length. The drain then frames only
//! 4508 bytes, lands mid-payload, and reinterprets the interior of the oversized
//! record as the next record -- so a normal record logged right after is
//! misframed (in a debug build the `debug_assert` in `assemble` fires first).
//!
//! With the gate, the oversized record is dropped and the following record
//! arrives intact. This test proves the sentinel survives.

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
fn oversized_record_is_dropped_and_next_record_survives() {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = CaptureSink {
        lines: Arc::clone(&lines),
    };

    let guard = ticklog::builder()
        .sink(sink)
        .max_level(Level::Trace)
        .build()
        .expect("first build in a fresh process must succeed");

    // ~70 KB argument: encoded record size exceeds u16::MAX (65_535). Without
    // the drop gate this misframes the ring (or trips the debug_assert).
    let huge = "x".repeat(70_000);
    info!("huge={}", huge);

    // A normal record immediately after. It must reach the sink uncorrupted --
    // that is the proof the oversized record did not desync the ring.
    info!("sentinel={}", 42u64);

    // Flush and join the drain.
    drop(guard);

    let captured = lines.lock().unwrap();

    // The sentinel survives, byte-for-byte.
    assert!(
        captured.iter().any(|l| l.ends_with("sentinel=42")),
        "sentinel record must survive the oversized record; got {captured:?}"
    );

    // The oversized record is dropped, not partially emitted.
    assert!(
        captured.iter().all(|l| !l.contains("huge=")),
        "oversized record must be dropped, not emitted; got {captured:?}"
    );
}
