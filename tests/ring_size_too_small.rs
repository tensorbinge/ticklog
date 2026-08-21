//! Regression: a ring size below the 128 KiB minimum is rejected.

use ticklog::TicklogError;

#[test]
fn ring_size_below_minimum_is_rejected() {
    let err = match ticklog::configure! {
        ring_size: 65_536,
    } {
        Err(e) => e,
        Ok(_guard) => panic!("ring size below the 128 KiB minimum must be rejected"),
    };
    assert!(matches!(err, TicklogError::InvalidRingSize(65_536)));
}
