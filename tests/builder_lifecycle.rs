//! Lifecycle: `configure!` spawns the drain, a second init is rejected,
//! and dropping the `Guard` joins the drain thread cleanly.
//!
//! This is an integration test (its own binary), so the process-global
//! `REGISTRY` OnceLock starts fresh -- `configure!` can succeed exactly once
//! here, which unit tests in the shared lib-test process cannot guarantee.

use ticklog::{TicklogError, WriterSink};

#[test]
fn configure_spawns_drain_rejects_second_init_and_joins_on_drop() {
    // A no-op sink is enough: this test exercises the lifecycle, not output.
    let guard = ticklog::configure! {
        sink: WriterSink::new(std::io::sink()),
    }
    .expect("first configure in a fresh process must succeed");

    // The registry is now claimed; a second call to the underlying runtime init
    // must be rejected without spawning another drain.
    let second =
        ticklog::__private::__configure_rt(Box::new(WriterSink::new(std::io::sink())), 0, None);
    assert!(matches!(second, Err(TicklogError::AlreadyInitialized)));

    // Dropping the guard signals shutdown and joins the drain thread. A hung
    // join or a panicked drain would hang or abort this test.
    drop(guard);
}
