//! Implementing `LogSink` by hand for a destination that is not `io::Write`.
//!
//! For plain `io::Write` destinations (files, sockets), wrap the writer in
//! `WriterSink`. When the destination is something else, e.g., a channel, a metrics
//! counter, a database handle, implement `LogSink` directly.
//!
//! Run it:
//!
//! ```text
//! cargo run --example sink_custom
//! ```

use std::io::{self, Write};

use ticklog::{error, info, warn, Level, LogSink};

/// A sink that prints each line to stdout with a marker and tallies how many
/// records it has accepted.
struct MarkerSink {
    count: u64,
}

impl LogSink for MarkerSink {
    fn accept(&mut self, line: &[u8], level: Level) -> io::Result<()> {
        self.count += 1;
        // `line` is the fully formatted record without a trailing newline.
        let mut out = io::stdout().lock();
        write!(out, "[custom #{} {:?}] ", self.count, level)?;
        out.write_all(line)?;
        out.write_all(b"\n")
    }
    // A per-sink `max_level` is honored only inside `FanOut` (see the fanout
    // example). A single sink receives every record the builder's level admits.
}

fn main() {
    let guard = ticklog::builder()
        .sink(MarkerSink { count: 0 })
        .max_level(Level::Trace) // admit every level so the sink sees them all
        .build()
        .expect("ticklog builds once per process");

    info!("listening on {}", 8080);
    warn!("disk {}% full", 91);
    error!("query failed: {}", "timeout");

    // Flush and join the drain before returning.
    drop(guard);
}
