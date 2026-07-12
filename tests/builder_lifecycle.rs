//! Builder lifecycle: `build()` spawns the drain, a second `build()` is
//! rejected, and dropping the `Guard` joins the drain thread cleanly.
//!
//! This is an integration test (its own binary), so the process-global
//! `REGISTRY` OnceLock starts fresh -- `build()` can succeed exactly once here,
//! which unit tests in the shared lib-test process cannot guarantee.

use ticklog::{TicklogError, WriterSink};

#[test]
fn build_spawns_drain_rejects_second_build_and_joins_on_drop() {
    // A no-op sink is enough: this test exercises the lifecycle, not output
    // (no records flow until the Phase 5 macros exist).
    let guard = ticklog::builder()
        .sink(WriterSink::new(std::io::sink()))
        .build()
        .expect("first build in a fresh process must succeed");

    // The registry is now claimed; a second build must be rejected without
    // spawning another drain.
    let second = ticklog::builder().build();
    assert!(matches!(second, Err(TicklogError::AlreadyInitialized)));

    // Dropping the guard signals shutdown and joins the drain thread. A hung
    // join or a panicked drain would hang or abort this test.
    drop(guard);
}
