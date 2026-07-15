//! Basic ticklog usage: build with a file sink, log at several levels, flush.
//!
//! Run it, then read the output:
//!
//! ```text
//! cargo run --example basic
//! cat logs/basic.log
//! ```

use std::fs;
use std::io;

use ticklog::{FileSink, Level, error, info, warn};

fn main() -> io::Result<()> {
    // Write to ./logs/basic.log
    fs::create_dir_all("logs")?;

    // FileSink buffers writes and truncates the file on open, so each run
    // starts with a fresh log.
    let guard = ticklog::configure! {
        sink: FileSink::truncate("logs/basic.log")?,
        max_level: Level::Info,
    }
    .expect("ticklog builds once per process");

    info!("server listening on {}", 8080);
    warn!("retry {} of {}", 3, 10);
    error!("connection refused: {}", "127.0.0.1:9090");

    // Dropping the guard flushes the remaining records and joins the drain
    // thread, so the file is complete when this returns.
    drop(guard);

    println!("wrote logs/basic.log");
    Ok(())
}
