//! Optional CPU pinning for benchmarks, driven by environment variables so
//! the producer and drain cores can change between runs without a recompile.

use std::env;

use ticklog::pin_thread;

/// Pin the calling (producer) thread to the core named by
/// `TICKLOG_PRODUCER_CORE`. No-op when the variable is unset or unparseable.
pub fn pin_producer_from_env() {
    if let Some(core) = core_from_env("TICKLOG_PRODUCER_CORE") {
        pin_thread(&[core]);
    }
}

/// Drain-thread core from `TICKLOG_DRAIN_CORE`, or `None` to leave the drain
/// unpinned. Pass the result to `Builder::drain_affinity`.
pub fn drain_core_from_env() -> Option<usize> {
    core_from_env("TICKLOG_DRAIN_CORE")
}

fn core_from_env(var: &str) -> Option<usize> {
    env::var(var).ok()?.parse().ok()
}
