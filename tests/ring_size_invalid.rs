//! Regression: an invalid `ring_size` is rejected before any global state is
//! claimed. Only one `configure!` expansion may appear per binary (the bridge
//! macros collide at crate root), so the "nothing was claimed" half goes
//! through the runtime init directly, like `tests/builder_lifecycle.rs`.

use std::io;

use ticklog::{TicklogError, WriterSink};

#[test]
fn invalid_ring_size_rejects_cleanly_and_claims_nothing() {
    let err = match ticklog::configure! {
        ring_size: 100_000,
    } {
        Err(e) => e,
        Ok(_guard) => panic!("non-power-of-two ring size must be rejected"),
    };
    assert!(matches!(err, TicklogError::InvalidRingSize(100_000)));

    // The failure claimed nothing: the runtime init still succeeds, proving
    // the validation ran before the registry was taken.
    let guard = ticklog::__private::__configure_rt(
        Box::new(WriterSink::new(io::sink())),
        0,
        None,
        "",
        ticklog::__private::DEFAULT_RING_SIZE,
    )
    .expect("runtime init must succeed after the macro-level rejection");
    drop(guard);
}
