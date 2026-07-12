//! Writing log lines to stderr with [`ConsoleSink`](ticklog::ConsoleSink).
//!
//! `ConsoleSink::stderr()` is the default sink installed by
//! [`builder`](ticklog::builder). Use it explicitly when you want to
//! combine stderr with another destination or override the color behavior.
//!
//! ```text
//! cargo run --example sink_stderr
//! ```

use std::io;

use ticklog::{ConsoleSink, Level, error, info, warn};

fn main() -> io::Result<()> {
    // Default: stderr, colored only when stderr is a terminal.
    let guard = ticklog::builder()
        .sink(ConsoleSink::stderr())
        .max_level(Level::Info)
        .build()
        .expect("ticklog builds once per process");

    info!("listening on {}", 8080);
    warn!("disk {}% full", 91);
    error!("connection refused: {}", "127.0.0.1:9090");

    // Drop the guard to flush and join the drain thread.
    drop(guard);
    Ok(())
}
