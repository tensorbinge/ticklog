//! Using logroller's rotating file writer as a ticklog sink.
//!
//! logroller handles file rotation, compression, and retention; ticklog handles
//! the low-latency logging path. `LogRoller` implements `io::Write`, so it plugs
//! into ticklog by wrapping it in [`WriterSink`], the adapter for any writer.
//!
//! ```text
//! cargo run --example sink_logroller
//! ls logs/
//! ```

use std::fs;
use std::io;

use logroller::{LogRollerBuilder, Rotation, RotationAge};
use ticklog::{error, info, warn, Level, WriterSink};

fn main() -> io::Result<()> {
    fs::create_dir_all("logs")?;

    // Daily rotation, keeping the 3 most recent files.
    let appender = LogRollerBuilder::new("logs", "ticklog-logroller.log")
        .rotation(Rotation::AgeBased(RotationAge::Daily))
        .max_keep_files(3)
        .graceful_shutdown(true)
        .build()
        .expect("logroller appender builds");

    let guard = ticklog::builder()
        .sink(WriterSink::new(appender))
        .max_level(Level::Info)
        .build()
        .expect("ticklog builds once per process");

    info!("listening on {}", 8080);
    warn!("retry {} of {}", 3, 10);
    error!("connection refused: {}", "127.0.0.1:9090");

    drop(guard);
    println!("wrote logs/ticklog-logroller.log");
    Ok(())
}
