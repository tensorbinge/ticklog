//! Per-thread ring buffer ownership and the global ring registry.
//!
//! Each thread holds an [`Arc`]`<`[`RingBuffer`]`>` in a thread-local
//! [`UnsafeCell`] slot. The global [`REGISTRY`] tracks all active rings.

use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use crate::error::TicklogError;
use crate::record::THREAD_SECTION_BASE_SIZE;
use crate::ring::RingBuffer;

/// A thread's local ring buffer and cached metadata.
///
/// Cached values (`thread_id`, `thread_name`) are set once at creation and
/// never change. The ring is shared with the drain thread via [`Arc`].
pub(crate) struct ThreadBuf {
    /// The ring buffer this thread writes records into.
    pub(crate) ring: Arc<RingBuffer>,
    /// Cached stable thread identifier.
    pub(crate) thread_id: u64,
    /// Cached thread name, if set.
    pub(crate) thread_name: Option<String>,
    /// Encoded wire size of the thread section for this thread.
    pub(crate) thread_section_size: u16,
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

/// Maximum length of a cached thread name, in bytes. Names longer than this
/// are truncated to avoid bloating every record from the thread.
const MAX_THREAD_NAME_LEN: usize = 256;

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
/// [`crate::configure!`].
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
            let mut thread_name: Option<String> = thread::current().name().map(String::from);
            if let Some(ref name) = thread_name {
                if name.len() > MAX_THREAD_NAME_LEN {
                    // Walk back from the byte limit to a valid UTF-8
                    // boundary so the slice never splits a multi-byte
                    // character (which would panic).
                    let mut end = MAX_THREAD_NAME_LEN;
                    while !name.is_char_boundary(end) {
                        end -= 1;
                    }
                    thread_name = Some(name[..end].to_string());
                }
            }
            let thread_section_size: u16 =
                (THREAD_SECTION_BASE_SIZE + thread_name.as_ref().map_or(0, |n| n.len())) as u16;
            *opt = Some(ThreadBuf {
                ring,
                thread_id: get_stable_thread_id(),
                thread_name,
                thread_section_size,
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
        .expect("invariant: ring registry not initialized; call ticklog::configure! before logging")
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
/// initialized yet (no [`configure!`][crate::configure!] has run).
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
            thread_section_size: (THREAD_SECTION_BASE_SIZE + "test-thread".len()) as u16,
        };
        assert_eq!(tb.thread_id, 42);
        assert_eq!(tb.thread_name.as_deref(), Some("test-thread"));
        assert_eq!(
            tb.thread_section_size as usize,
            THREAD_SECTION_BASE_SIZE + "test-thread".len(),
        );
        assert!(tb.ring.live.load(Ordering::Relaxed));
    }

    #[test]
    fn drop_sets_live_to_false() {
        let ring = Arc::new(RingBuffer::new());
        let tb = ThreadBuf {
            ring: Arc::clone(&ring),
            thread_id: 1,
            thread_name: None,
            thread_section_size: THREAD_SECTION_BASE_SIZE as u16,
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
            thread_section_size: THREAD_SECTION_BASE_SIZE as u16,
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
        // outer `&mut ThreadBuf` is live. On the unguarded code the inner
        // `&mut *buf.get()` aliases the outer one -> UB (Miri flags it).
        // With the re-entrancy guard the inner call is refused.
        let outer = with_thread_buf(|_outer| {
            // Re-entrant call on the same thread must return None.
            with_thread_buf(|_inner| ())
        });
        assert_eq!(
            outer,
            Some(None),
            "outer must succeed, inner must be refused"
        );
    }

    /// A thread name whose byte-length exceeds [`MAX_THREAD_NAME_LEN`] and
    /// whose 256th byte falls inside a multi-byte UTF-8 character must not
    /// panic. 255 ASCII `a`s + `é` (2 bytes) = 257 bytes; the byte-index
    /// slice `[..256]` lands mid-char without `floor_char_boundary`.
    #[test]
    fn thread_name_truncation_respects_utf8_boundary() {
        init_registry();
        let mut name = "a".repeat(255);
        name.push('é'); // U+00E9, 2 bytes in UTF-8
        assert_eq!(name.len(), 257);

        let joined = std::thread::Builder::new()
            .name(name)
            .spawn(move || {
                with_thread_buf(|tb| {
                    if let Some(ref n) = tb.thread_name {
                        assert!(
                            n.len() <= MAX_THREAD_NAME_LEN,
                            "truncated name too long: {}",
                            n.len()
                        );
                        // Must be valid UTF-8.
                        let _ = n.chars().count();
                    }
                    tb.thread_id
                })
            })
            .unwrap()
            .join()
            .unwrap();

        assert!(joined.is_some(), "with_thread_buf must succeed");
    }
}
