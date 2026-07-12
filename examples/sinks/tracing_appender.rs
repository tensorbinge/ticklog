//! Using tracing-appender's rolling file writer as a ticklog sink.
//!
//! `RollingFileAppender` implements `io::Write`, so it becomes a `LogSink` by
//! wrapping it in [`WriterSink`] -- ticklog gets tracing-appender's date-based
//! file rotation for free.
//!
//! ```text
//! cargo run --example sink_tracing_appender
//! ls logs/
//! ```

use std::fs;
use std::io;

use ticklog::{error, info, warn, Level, WriterSink};
use tracing_appender::rolling;

fn main() -> io::Result<()> {
    fs::create_dir_all("logs")?;

    // Rolls daily: writes ./logs/ticklog-tracing.log.<YYYY-MM-DD>.
    let appender = rolling::daily("logs", "ticklog-tracing.log");

    let guard = ticklog::builder()
        .sink(WriterSink::new(appender))
        .max_level(Level::Info)
        .build()
        .expect("ticklog builds once per process");

    info!("listening on {}", 8080);
    warn!("retry {} of {}", 3, 10);
    error!("connection refused: {}", "127.0.0.1:9090");

    drop(guard);
    println!("wrote logs/ticklog-tracing.log.<date>");
    Ok(())
}
