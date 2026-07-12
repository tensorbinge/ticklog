//! `warm_up` front-loads a thread's one-time costs so the first log after it
//! allocates nothing on the caller.
//!
//! Two guarantees are checked under one `build()` (the registry is claimed once
//! per process):
//!
//! 1. `warm_up` is idempotent -- a second call on an already-warmed thread is a
//!    no-op that still returns `Ok`.
//! 2. The first `info!` after `warm_up` performs zero heap allocations on the
//!    calling thread. `warm_up` has already allocated the ring and scratch
//!    buffer, initialized the thread-local slot, and registered its destructor,
//!    so the hot path only reads the clock, encodes into the existing scratch,
//!    and copies into the ring.
//!
//! A process-wide counting allocator makes producer-thread allocations
//! observable. It counts only while a thread-local flag is set, and only on the
//! thread that sets it, so the drain thread's formatting allocations (a
//! different thread) are never counted. The flag and counter use const-
//! initialized thread-locals, which need no allocation to access -- the counting
//! path itself must not allocate, or it would recurse into the allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io;

use ticklog::{info, warm_up, Level, WriterSink};

thread_local! {
    /// Heap allocations observed on this thread while counting is enabled.
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
    /// Whether this thread is currently counting its allocations.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

/// Records one allocation when the current thread has counting enabled. Uses
/// only const-initialized thread-locals, so it never allocates and never
/// re-enters the allocator.
fn note_alloc() {
    COUNTING.with(|on| {
        if on.get() {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
    });
}

/// Delegates to the system allocator, tallying each allocation on the counting
/// thread.
struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_alloc();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_alloc();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_alloc();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

#[test]
fn warm_up_is_idempotent_and_first_log_is_allocation_free() {
    // build() and the sink live outside the measured window; a null sink keeps
    // the drain's own work irrelevant (it runs on another thread regardless).
    let guard = ticklog::builder()
        .sink(WriterSink::new(io::sink()))
        .max_level(Level::Trace)
        .build()
        .expect("first build in a fresh process must succeed");

    // First call allocates the ring, scratch, and thread-local state.
    warm_up().expect("warm_up after build must succeed");
    // Second call is a no-op on an already-warmed thread.
    warm_up().expect("warm_up must be idempotent");

    // Prime the counting thread-locals so their first access is outside the
    // measured window (const init means this does not allocate anyway).
    COUNTING.with(|on| on.set(false));
    ALLOCATIONS.with(|n| n.set(0));

    // Measure exactly one log call on the warmed thread.
    COUNTING.with(|on| on.set(true));
    info!("warm {}", 1u64);
    COUNTING.with(|on| on.set(false));

    let allocations = ALLOCATIONS.with(|n| n.get());
    assert_eq!(
        allocations, 0,
        "first log after warm_up allocated {allocations} time(s) on the caller"
    );

    drop(guard);
}
