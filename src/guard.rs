//! Owns the drain thread. Dropping the [`Guard`] shuts down the drain and
//! waits for it to complete its final poll cycle.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// A running logger. Keep it alive for as long as you want to log.
///
/// Returned by [`Builder::build`]. When the guard is dropped it flushes the
/// sink, stops the background drain thread, and disables logging, so **any log
/// call after the guard is dropped is a silent no-op**.
///
/// [`Builder::build`]: crate::Builder::build
pub struct Guard {
    drain_thread: Option<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl Guard {
    /// Creates a new guard from a running drain thread and a shared shutdown
    /// flag.
    ///
    /// `shutdown` must be the same `Arc<AtomicBool>` that the drain thread
    /// reads at the top of its poll loop.
    pub(crate) fn new(drain_thread: JoinHandle<()>, shutdown: Arc<AtomicBool>) -> Self {
        Self {
            drain_thread: Some(drain_thread),
            shutdown,
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Lower the level ceiling to "off" before joining. Once the drain is
        // gone, records written into a ring would never be consumed: under
        // `Backpressure::Block` a producer whose ring fills would spin forever
        // (the tail never advances), and under `Drop` records vanish silently.
        // Disabling the ceiling here makes every producer short-circuit before
        // touching a ring. Doing it *before* the join keeps the drain alive
        // through the join window, so any producer already mid-write can still
        // be unblocked; after the join returns, logging is a no-op.
        crate::builder::disable_logging();

        // Signal the drain to exit at the top of its next poll cycle.
        // Release pairs with the drain's Acquire load of `shutdown`,
        // guaranteeing the store is visible before the drain checks it.
        self.shutdown.store(true, Ordering::Release);

        // Wait for the drain to complete its final poll cycle and exit.
        if let Some(handle) = self.drain_thread.take() {
            if let Err(e) = handle.join() {
                // The drain thread panicked. Try to extract a message from
                // the panic payload and write it to stderr.
                if let Some(msg) = e.downcast_ref::<&str>() {
                    eprintln!("ticklog: drain thread panicked: {}", msg);
                } else if let Some(msg) = e.downcast_ref::<String>() {
                    eprintln!("ticklog: drain thread panicked: {}", msg);
                } else {
                    eprintln!("ticklog: drain thread panicked");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn new_wraps_handle_and_shutdown() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let handle = thread::spawn(|| {});
        let guard = Guard::new(handle, Arc::clone(&shutdown));
        assert!(guard.drain_thread.is_some());
        assert!(!guard.shutdown.load(Ordering::Relaxed));
    }

    #[test]
    fn drop_sets_shutdown_flag() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let handle = thread::spawn(move || {
            // Busy-wait until shutdown is signaled, then exit.
            while !flag.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
        });

        let guard = Guard::new(handle, Arc::clone(&shutdown));
        assert!(!shutdown.load(Ordering::Relaxed));
        drop(guard);
        assert!(shutdown.load(Ordering::Relaxed));
    }

    #[test]
    fn drop_joins_drain_thread() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let done = Arc::clone(&exited);

        let handle = thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            done.store(true, Ordering::Release);
        });

        let guard = Guard::new(handle, Arc::clone(&shutdown));
        drop(guard);
        // After Guard::drop returns, the thread must have exited.
        assert!(exited.load(Ordering::Relaxed));
    }

    #[test]
    fn guard_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Guard>();
    }
}
