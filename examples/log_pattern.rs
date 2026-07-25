//! Custom log-line pattern via `configure!(format: "...")`.
//!
//! ```text
//! cargo run --example log_pattern
//! ```

use ticklog::{ConsoleSink, error, info, warn};

fn main() {
    let guard = ticklog::configure! {
        sink: ConsoleSink::stderr(),
        format: "{timestamp} {level:>5} ({file}:{line}) [tid={thread_id}|tname={thread_name}] - {message}",
    }
    .expect("ticklog builds once per process");

    info!("server listening on {}", 8080);
    warn!("retry {} of {}", 3, 10);
    error!("connection refused: {}", "127.0.0.1:9090");

    // 2026-07-25T12:33:12.580454852Z  INFO (examples/log_pattern.rs:16) [tid=1|tname=main] - server listening on 8080
    // 2026-07-25T12:33:12.580456827Z  WARN (examples/log_pattern.rs:17) [tid=1|tname=main] - retry 3 of 10
    // 2026-07-25T12:33:12.580457660Z ERROR (examples/log_pattern.rs:18) [tid=1|tname=main] - connection refused: 127.0.0.1:9090

    drop(guard);
}
