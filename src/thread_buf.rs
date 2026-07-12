//! Per-thread ring buffer ownership and the global ring registry.
//!
//! Each thread holds an [`Arc`]`<`[`RingBuffer`]`>` in a thread-local
//! [`UnsafeCell`] slot. The global [`REGISTRY`] tracks all active rings.

use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use crate::error::TicklogError;
use crate::ring::RingBuffer;

/// Initial capacity of a [`ThreadBuf`]'s scratch buffer. Allocated once when a
/// thread is warmed up and sized to hold essentially every record without ever
/// reallocating; an unusually large record grows it once and it stays grown.
const INITIAL_SCRATCH: usize = 4096;

/// A thread's local ring buffer and cached metadata.
///
/// Cached values (`thread_id`, `thread_name`) are set once at creation and
/// never change. The ring is shared with the drain thread via [`Arc`]. The
/// `scratch` buffer is reused across log calls so record assembly allocates
/// only when a record is larger than any seen before.
pub(crate) struct ThreadBuf {
    /// The ring buffer this thread writes records into.
    pub(crate) ring: Arc<RingBuffer>,
    /// Cached stable thread identifier.
    #[allow(unused)]
    pub(crate) thread_id: u64,
    /// Cached thread name, if set.
    #[allow(unused)]
    pub(crate) thread_name: Option<String>,
    /// Reusable buffer that a record is assembled into before being copied
    /// into the ring. Kept per-thread to avoid a hot-path allocation.
    pub(crate) scratch: Vec<u8>,
}

impl Drop for ThreadBuf {
    fn drop(&mut self) {
        // Signal to the drain: no more records will be written.
        // Release pairs with the drain's Acquire load of `live`,
        // guaranteeing all prior head stores are visible.
        self.ring.live.store(false, Ordering::Release);
        // Arc<RingBuffer> dropped implicitly. Buffer stays alive if
        // the drain still holds its clone.
    }
}

/// Extracts a stable `u64` identifier from [`std::thread::ThreadId`] by
/// parsing its `Debug` representation.
///
/// Returns `0` if the internal format changes unexpectedly.
fn get_stable_thread_id() -> u64 {
    let id = thread::current().id();
    let id_str = format!("{id:?}"); // e.g. "ThreadId(2)"
    id_str
        .strip_prefix("ThreadId(")
        .and_then(|s| s.strip_suffix(')'))
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Per-thread state: the ring buffer plus a re-entrancy flag.
///
/// The flag lives alongside `buf` in the same thread-local so the hot path
/// still performs a single TLS lookup. It is read and written through [`Cell`]
/// (interior mutability, never a `&mut`), so touching it never aliases the
/// `&mut Option<ThreadBuf>` formed from the sibling `buf` field.
struct ThreadSlot {
    /// Set while [`with_thread_buf`] is running its closure. A re-entrant call
    /// on the same thread observes it set and bails out before forming a second
    /// `&mut *buf.get()`; that aliasing would be Undefined Behavior.
    active: Cell<bool>,
    /// The thread's ring buffer and scratch. [`UnsafeCell`] (not
    /// [`std::cell::RefCell`]) keeps the hot path branch-free; the `active`
    /// flag, not a runtime borrow count, is what enforces exclusive access.
    buf: UnsafeCell<Option<ThreadBuf>>,
}

thread_local! {
    /// Per-thread ring buffer and re-entrancy flag. Each thread has exclusive
    /// access to its own slot.
    static THREAD_SLOT: ThreadSlot = const {
        ThreadSlot {
            active: Cell::new(false),
            buf: UnsafeCell::new(None),
        }
    };
}

/// Resets the re-entrancy flag when it drops, so the flag is cleared even if the
/// closure passed to [`with_thread_buf`] panics; a single panicking encode
/// must not permanently silence this thread's logging.
struct ActiveGuard<'a>(&'a Cell<bool>);

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// Accesses the current thread's [`ThreadBuf`], initializing it on first
/// call, and runs `f` with exclusive access to it.
///
/// Lazy init allocates a new [`RingBuffer`], registers it with the global
/// [`REGISTRY`], and caches thread metadata. Subsequent calls return
/// immediately.
///
/// Returns `Some(f(..))` normally. Returns `None` without running `f` if this
/// is a **re-entrant** call on the same thread, i.e. `f` itself (a
/// `Loggable::encode` that logs, or a panic hook that logs mid-encode) called
/// back into `with_thread_buf`. Such a nested log is dropped rather than
/// allowed to form a second aliasing `&mut` into the thread-local slot.
///
/// # Panics
///
/// Panics if the [`REGISTRY`] has not been initialized by
/// [`Builder::build`].
pub(crate) fn with_thread_buf<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut ThreadBuf) -> R,
{
    THREAD_SLOT.with(|slot| {
        // Refuse a re-entrant call. Checked through `Cell` (no `&mut`), so it
        // happens before (and without aliasing) the `&mut *buf.get()` below.
        if slot.active.get() {
            return None;
        }
        slot.active.set(true);
        // Clear `active` on the way out, including on a panic unwinding through
        // `f`, so one panicking encode doesn't leave the thread permanently
        // marked active (which would drop all its future records).
        let _active = ActiveGuard(&slot.active);

        // SAFETY: `active` was false and is now true, so no other frame on this
        // thread holds a reference into `buf`; this `&mut` is exclusive. The raw
        // pointer from `buf.get()` is thread-local and never shared across
        // threads.
        let opt = unsafe { &mut *slot.buf.get() };

        if opt.is_none() {
            let ring = Arc::new(RingBuffer::new());
            register_ring(Arc::clone(&ring));
            *opt = Some(ThreadBuf {
                ring,
                thread_id: get_stable_thread_id(),
                thread_name: thread::current().name().map(String::from),
                scratch: Vec::with_capacity(INITIAL_SCRATCH),
            });
        }

        // SAFETY: `opt` was just initialized above if it was `None`, so
        // `unwrap_unchecked` is sound.
        Some(f(unsafe { opt.as_mut().unwrap_unchecked() }))
    })
}

/// Global registry of all active ring buffers.
///
/// Set once at initialization. New producer threads register their rings
/// here during lazy init or [`warm_up`].
pub(crate) static REGISTRY: OnceLock<Mutex<Vec<Arc<RingBuffer>>>> = OnceLock::new();

/// Registers a ring buffer with the global [`REGISTRY`].
///
/// # Panics
///
/// Panics if the [`REGISTRY`] has not been initialized.
pub(crate) fn register_ring(ring: Arc<RingBuffer>) {
    let mut rings = REGISTRY
        .get()
        .expect("invariant: ring registry not initialized; call ticklog::builder().build() before logging")
        .lock()
        .expect("invariant: ring registry mutex poisoned by a panic in another thread");
    rings.push(ring);
}

/// Prepares the calling thread for logging by allocating its buffer up front,
/// moving the one-time, first-log allocation off a latency-sensitive path.
///
/// Calling it more than once on a thread is a no-op. Threads that skip it are
/// prepared lazily on their first log call instead.
///
/// # Errors
///
/// Returns [`TicklogError::NotInitialized`] if ticklog has not been
/// initialized yet (no [`Builder::build`](crate::Builder::build) has run).
pub fn warm_up() -> Result<(), TicklogError> {
    if REGISTRY.get().is_none() {
        return Err(TicklogError::NotInitialized);
    }
    with_thread_buf(|_| {});
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_registry() {
        let _ = REGISTRY.set(Mutex::new(Vec::new()));
    }

    #[test]
    fn get_stable_thread_id_returns_nonzero() {
        let id = get_stable_thread_id();
        assert!(id > 0, "expected non-zero thread id, got {id}");
    }

    #[test]
    fn thread_buf_holds_ring_and_metadata() {
        let ring = Arc::new(RingBuffer::new());
        let tb = ThreadBuf {
            ring: Arc::clone(&ring),
            thread_id: 42,
            thread_name: Some("test-thread".into()),
            scratch: Vec::new(),
        };
        assert_eq!(tb.thread_id, 42);
        assert_eq!(tb.thread_name.as_deref(), Some("test-thread"));
        assert!(tb.ring.live.load(Ordering::Relaxed));
    }

    #[test]
    fn drop_sets_live_to_false() {
        let ring = Arc::new(RingBuffer::new());
        let tb = ThreadBuf {
            ring: Arc::clone(&ring),
            thread_id: 1,
            thread_name: None,
            scratch: Vec::new(),
        };
        assert!(ring.live.load(Ordering::Relaxed));
        drop(tb);
        assert!(!ring.live.load(Ordering::Relaxed));
    }

    #[test]
    fn buffer_survives_thread_buf_drop_when_other_arcs_exist() {
        let ring = Arc::new(RingBuffer::new());
        let other = Arc::clone(&ring);
        let tb = ThreadBuf {
            ring: Arc::clone(&ring),
            thread_id: 1,
            thread_name: None,
            scratch: Vec::new(),
        };
        drop(tb);
        // Drop set live = false on the shared RingBuffer.
        assert!(!ring.live.load(Ordering::Relaxed));
        // `other` is still a valid Arc; clone and drop without panic.
        let still_here = Arc::clone(&other);
        drop(still_here);
        drop(ring);
        drop(other);
    }

    #[test]
    fn warm_up_and_with_thread_buf_lifecycle() {
        init_registry();
        // First call initializes.
        warm_up().unwrap();
        // Second call is idempotent.
        warm_up().unwrap();
        // with_thread_buf finds existing ThreadBuf from warm_up.
        with_thread_buf(|tb| {
            assert!(tb.thread_id > 0);
        });
    }

    #[test]
    fn register_ring_adds_to_registry() {
        init_registry();
        let mut rings = REGISTRY.get().unwrap().lock().unwrap();
        let count_before = rings.len();
        rings.push(Arc::new(RingBuffer::new()));
        assert_eq!(rings.len(), count_before + 1);
    }

    #[test]
    fn with_thread_buf_thread_id_matches_current_thread() {
        init_registry();
        let expected = get_stable_thread_id();
        with_thread_buf(|tb| {
            assert_eq!(tb.thread_id, expected);
        });
    }

    #[test]
    fn reentrant_with_thread_buf_is_refused() {
        init_registry();
        // Models a `Loggable::encode` (or a panic hook) that logs while the
        // outer record is still being assembled: the inner call runs while the
        // outer `&mut ThreadBuf` is live, and the outer buffer is used again
        // afterward. On the unguarded code the inner `&mut *buf.get()` aliases
        // the outer one -> UB (Miri flags it), and the inner call also mutates
        // the shared scratch. With the re-entrancy guard the inner call is
        // refused and the outer buffer is untouched by it.
        with_thread_buf(|outer| {
            outer.scratch.clear();
            outer.scratch.push(0xAA);
            // Re-entrant call on the same thread.
            with_thread_buf(|inner| inner.scratch.push(0xBB));
            // Use the outer borrow AFTER the nested call.
            outer.scratch.push(0xCC);
        });

        // The re-entrant call must not have run, so scratch holds only the
        // outer writes.
        with_thread_buf(|tb| {
            assert_eq!(
                tb.scratch,
                vec![0xAA, 0xCC],
                "re-entrant with_thread_buf must be refused, not run"
            );
        });
    }
}
