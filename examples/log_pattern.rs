//! Custom log-line pattern via `configure!(format: "...")`.
//!
//! ```text
//! cargo run --example log_pattern
//! ```

use ticklog::{ConsoleSink, error, info, warn};

fn main() {
    let guard = ticklog::configure! {
        sink: ConsoleSink::stderr(),
        format: "{timestamp} {level:>5} {file}:{line} [{thread_id}|{thread_name}] - {message}",
    }
    .expect("ticklog builds once per process");

    info!("server listening on {}", 8080);
    warn!("retry {} of {}", 3, 10);
    error!("connection refused: {}", "127.0.0.1:9090");

    // 2026-07-25T13:57:44.842300838Z  INFO examples/log_pattern.rs:16 [ThreadId(1)|main] - server listening on 8080
    // 2026-07-25T13:57:44.842306147Z  WARN examples/log_pattern.rs:17 [ThreadId(1)|main] - retry 3 of 10
    // 2026-07-25T13:57:44.842308438Z ERROR examples/log_pattern.rs:18 [ThreadId(1)|main] - connection refused: 127.0.0.1:9090

    drop(guard);
}
