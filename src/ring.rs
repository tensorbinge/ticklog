//! A single-producer, single-consumer ring buffer for lock-free record passing.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::builder::Backpressure;
use crate::record::{END_OF_BUFFER, MAX_RECORD_SIZE, VERSION};

/// Cache-line size in bytes.
///
/// Apple Silicon (M1/M2/M3/M4) uses 128-byte cache lines on P-cores.
/// Intel, AMD, and standard ARM64 use 64-byte cache lines.
const CACHE_LINE_SIZE: usize = {
    #[cfg(all(target_arch = "aarch64", target_vendor = "apple"))]
    {
        128
    }
    #[cfg(not(all(target_arch = "aarch64", target_vendor = "apple")))]
    {
        64
    }
};

const ATOMIC_U64_SIZE: usize = std::mem::size_of::<AtomicU64>();
const UNSAFE_CELL_U64_SIZE: usize = std::mem::size_of::<UnsafeCell<u64>>();

/// Slot size in bytes. Records are placed at slot-aligned positions so
/// adjacent records never share a data cache line.
pub(crate) const SLOT_SIZE: usize = CACHE_LINE_SIZE;

/// A reserved write slot in the ring. `ptr` points to a contiguous region of
/// at least the requested size; `head` is the value to store (with Release
/// ordering) after the caller has written the record bytes.
pub(crate) struct Reservation {
    pub(crate) ptr: *mut u8,
    pub(crate) head: u64,
}

/// Ring buffer capacity in bytes. Power of 2 for bitmask indexing.
pub(crate) const RING_SIZE: usize = 1_048_576; // 1 MB

/// Round `n` up to the nearest multiple of `align`.
/// `align` must be a power of 2.
#[inline(always)]
pub(crate) const fn align_up(n: u64, align: u64) -> u64 {
    n.wrapping_add(align - 1) & !(align - 1)
}

/// A single-producer, single-consumer ring buffer with cache-line-padded
/// atomic control fields.
///
/// The producer writes records and advances `head` (Release); the drain
/// reads `head` (Acquire), processes records, and advances `tail` (Release).
/// Control fields are split across two cache lines so the producer and drain
/// never contend for the same line on the hot path.
#[repr(C)]
#[cfg_attr(
    all(target_arch = "aarch64", target_vendor = "apple"),
    repr(align(128))
)]
#[cfg_attr(
    not(all(target_arch = "aarch64", target_vendor = "apple")),
    repr(align(64))
)]
pub(crate) struct RingBuffer {
    // Producer cache line: offsets 0..CACHE_LINE-1
    /// Monotonic write position. Producer stores with Release; drain loads
    /// with Acquire.
    pub(crate) head: AtomicU64,
    /// Producer's local copy of the drain's tail. Cached to avoid reading
    /// the drain's cache line on every record.
    pub(crate) tail_cache: UnsafeCell<u64>,
    /// Fills the producer cache line.
    _pad_p: [u8; CACHE_LINE_SIZE - ATOMIC_U64_SIZE - UNSAFE_CELL_U64_SIZE],

    // Drain cache line
    /// Monotonic read position. Drain stores with Release; producer loads
    /// with Acquire during capacity checks. The Release/Acquire pair keeps the
    /// producer from overwriting a wrapped slot the drain is still reading.
    pub(crate) tail: AtomicU64,
    /// Drain's local copy of the producer's head. Cached to detect new
    /// records without a redundant atomic load.
    pub(crate) head_cache: UnsafeCell<u64>,
    /// Fills the drain cache line.
    _pad_d: [u8; CACHE_LINE_SIZE - ATOMIC_U64_SIZE - UNSAFE_CELL_U64_SIZE],

    /// Set to `false` by the producer on thread exit. The drain reads with
    /// Acquire to detect dead rings whose remaining records have been
    /// consumed.
    pub(crate) live: AtomicBool,

    /// Ring storage as a slice of per-byte cells. The `UnsafeCell<u8>` interior
    /// mutability lets the producer and drain reach disjoint bytes through a
    /// shared `&self` without ever forming a `&`/`&mut` spanning the buffer: a
    /// whole-slice borrow on one thread overlapping the other's is a
    /// Stacked/Tree-Borrows violation (and a data race on the retag) even when
    /// the byte ranges are disjoint. The base pointer is taken with
    /// `data.as_ptr()`, which for `UnsafeCell` bytes is not a read of their
    /// contents and does not disable the owning allocation.
    pub(crate) data: Box<[UnsafeCell<u8>]>,
}

// SAFETY: RingBuffer is safe to share between threads because the SPSC
// protocol guarantees the producer and drain never access the same bytes
// concurrently:
//
// - `tail_cache` is written and read only by the producer.
// - `head_cache` is written and read only by the drain.
// - `head` (producer-write, drain-read) and `tail` (drain-write,
//   producer-read) are AtomicU64 with paired Acquire/Release ordering.
// - The data region is partitioned: the producer writes to offsets
//   [head..head+record_size], the drain reads from [tail..head]. The
//   invariant tail <= head ensures these ranges never overlap. Each byte
//   is accessed by exactly one thread at any time.
unsafe impl Sync for RingBuffer {}

impl RingBuffer {
    /// Creates a new ring buffer with zero-initialized storage and `live`
    /// set to `true`.
    pub(crate) fn new() -> Self {
        // SAFETY: `UnsafeCell<u8>` is `repr(transparent)` over `u8`, so a
        // zero-filled `Box<[u8]>` and a `Box<[UnsafeCell<u8>]>` of the same
        // length share layout. The cast reinterprets the one heap buffer and
        // preserves the slice length metadata.
        let bytes = vec![0u8; RING_SIZE].into_boxed_slice();
        let data: Box<[UnsafeCell<u8>]> =
            unsafe { Box::from_raw(Box::into_raw(bytes) as *mut [UnsafeCell<u8>]) };
        Self {
            head: AtomicU64::new(0),
            tail_cache: UnsafeCell::new(0),
            _pad_p: [0u8; CACHE_LINE_SIZE - ATOMIC_U64_SIZE - UNSAFE_CELL_U64_SIZE],
            tail: AtomicU64::new(0),
            head_cache: UnsafeCell::new(0),
            _pad_d: [0u8; CACHE_LINE_SIZE - ATOMIC_U64_SIZE - UNSAFE_CELL_U64_SIZE],
            live: AtomicBool::new(true),
            data,
        }
    }

    /// Reserves a slot in the ring for a record of `total_size` bytes.
    ///
    /// Returns a [`Reservation`] with a write pointer and the future `head`
    /// value, or `None` if the ring is full under [`Backpressure::Drop`].
    /// Under [`Backpressure::Block`] this spins until space frees, so it
    /// always returns `Some`.
    ///
    /// The caller writes exactly `total_size` bytes to `Reservation::ptr`,
    /// then calls [`publish`](Self::publish). The slot is slot-aligned so
    /// adjacent records never share a cache line.
    ///
    /// Single-producer: the calling thread is the sole writer of this ring's
    /// `head` and `tail_cache`.
    #[inline]
    pub(crate) fn reserve(&self, total_size: usize, policy: Backpressure) -> Option<Reservation> {
        debug_assert!(
            total_size <= MAX_RECORD_SIZE,
            "invariant: total_size must fit the u16 total_size field"
        );
        let total = total_size as u64;
        let aligned = align_up(total, SLOT_SIZE as u64);

        // The producer is the sole writer of `head`, so a Relaxed load of its
        // own position is sufficient.
        let head = self.head.load(Ordering::Relaxed);
        let offset = (head & (RING_SIZE as u64 - 1)) as usize;
        let remaining_phys = (RING_SIZE - offset) as u64;

        // If the record would straddle the physical end of the buffer, an
        // EndOfBuffer record first fills the tail and the real record wraps to
        // offset 0. Both the filler and the record must fit, so both count
        // toward the space this write needs.
        let wrap = aligned > remaining_phys;
        let needed = if wrap {
            aligned + remaining_phys
        } else {
            aligned
        };

        if !self.try_ensure_capacity(head, needed, policy) {
            return None;
        }

        // The producer reaches the buffer through a base pointer taken from the
        // shared `data` cells, never a slice reference that would race the
        // drain. The capacity check guarantees the drain's tail will not enter
        // `[head, head + needed)` while this write proceeds, so these bytes are
        // the producer's alone. The allocation outlives the call via the shared
        // `Arc<RingBuffer>`.
        let base = self.data.as_ptr() as *mut u8;

        let mut next = head;
        if wrap {
            // `remaining_phys < aligned <= 65536` here (see the top of this fn),
            // so the u16 cast is exact.
            // SAFETY: `offset` starts a slot-aligned region of `remaining_phys`
            // bytes inside the ring; the EOB header occupies its first 4 bytes.
            unsafe { write_eob(base.add(offset), remaining_phys as u16) };
            next = next.wrapping_add(remaining_phys);
        }

        // The record lands at the next slot boundary: offset 0 after a wrap.
        let dst = (next & (RING_SIZE as u64 - 1)) as usize;
        let new_head = next.wrapping_add(aligned);

        Some(Reservation {
            // SAFETY: `dst` starts the `aligned`-byte region reserved above;
            // the caller writes `total_size` bytes, which is <= aligned, so the
            // write stays within the ring.
            ptr: unsafe { base.add(dst) },
            head: new_head,
        })
    }

    /// Publishes a reserved slot to the drain. Release pairs with the drain's
    /// Acquire load of `head`, so every byte written to `r.ptr` is visible
    /// before the drain sees the new head and reads the region.
    #[inline(always)]
    pub(crate) fn publish(&self, r: Reservation) {
        self.head.store(r.head, Ordering::Release);
    }

    /// Ensures `[head, head + needed)` is free for the producer to write,
    /// applying `policy` when the ring is full.
    ///
    /// Returns `true` once the space is available, or `false` if the record
    /// must be dropped under [`Backpressure::Drop`].
    fn try_ensure_capacity(&self, head: u64, needed: u64, policy: Backpressure) -> bool {
        // Fast path: trust the cached tail. Occupancy after the write is
        // `(head - tail) + needed`; it fits when that does not exceed RING_SIZE.
        // SAFETY: `tail_cache` is producer-private; only this thread touches
        // it.
        let cached = unsafe { *self.tail_cache.get() };
        if head.wrapping_add(needed).wrapping_sub(cached) <= RING_SIZE as u64 {
            return true;
        }

        loop {
            // Refresh from the drain. Acquire pairs with the drain's Release
            // store of `tail`, so a freed slot's reads complete before the
            // producer reuses it.
            let tail = self.tail.load(Ordering::Acquire);
            // SAFETY: producer-private, as above.
            unsafe { *self.tail_cache.get() = tail };
            if head.wrapping_add(needed).wrapping_sub(tail) <= RING_SIZE as u64 {
                return true;
            }
            match policy {
                Backpressure::Drop => return false,
                Backpressure::Block => {
                    if !self.live.load(Ordering::Relaxed) {
                        return false;
                    }
                    std::hint::spin_loop();
                }
            }
        }
    }
}

/// Writes a header-only `END_OF_BUFFER` record of `span` bytes at `ptr`.
///
/// The drain reads only the 4-byte prefix (version, type, total_size), then
/// skips the whole span and wraps to the start of the ring.
///
/// # Safety
///
/// `ptr` must start a writable region of at least 4 bytes within the ring's
/// data allocation.
unsafe fn write_eob(ptr: *mut u8, span: u16) {
    let size = span.to_le_bytes();
    // SAFETY: the caller guarantees `ptr..ptr+4` is writable and in-bounds.
    unsafe {
        ptr.write(VERSION);
        ptr.add(1).write(END_OF_BUFFER);
        ptr.add(2).write(size[0]);
        ptr.add(3).write(size[1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem;

    #[test]
    fn constants_are_power_of_two() {
        assert!(RING_SIZE.is_power_of_two());
        assert!(SLOT_SIZE.is_power_of_two());
    }

    #[test]
    fn ring_size_is_1mb() {
        assert_eq!(RING_SIZE, 1024 * 1024);
    }

    #[test]
    fn struct_alignment_matches_cache_line() {
        assert_eq!(mem::align_of::<RingBuffer>(), CACHE_LINE_SIZE);
    }

    #[test]
    fn struct_size_is_multiple_of_alignment() {
        let size = mem::size_of::<RingBuffer>();
        assert_eq!(size % CACHE_LINE_SIZE, 0);
    }

    #[test]
    fn head_at_offset_zero() {
        let rb = RingBuffer::new();
        let base = &rb as *const RingBuffer as usize;
        let head_addr = &rb.head as *const AtomicU64 as usize;
        assert_eq!(head_addr - base, 0);
    }

    #[test]
    fn tail_at_cache_line_offset() {
        let rb = RingBuffer::new();
        let base = &rb as *const RingBuffer as usize;
        let tail_addr = &rb.tail as *const AtomicU64 as usize;
        assert_eq!(tail_addr - base, CACHE_LINE_SIZE);
    }

    #[test]
    fn head_and_tail_on_different_cache_lines() {
        let rb = RingBuffer::new();
        let head_addr = &rb.head as *const AtomicU64 as usize;
        let tail_addr = &rb.tail as *const AtomicU64 as usize;
        let diff = head_addr.abs_diff(tail_addr);
        assert!(diff >= CACHE_LINE_SIZE);
    }

    #[test]
    fn new_initializes_head_to_zero() {
        let rb = RingBuffer::new();
        assert_eq!(rb.head.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn new_initializes_tail_to_zero() {
        let rb = RingBuffer::new();
        assert_eq!(rb.tail.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn new_initializes_tail_cache_to_zero() {
        let rb = RingBuffer::new();
        // SAFETY: this test is single-threaded; no other reference aliases
        // the UnsafeCell.
        let val = unsafe { *rb.tail_cache.get() };
        assert_eq!(val, 0);
    }

    #[test]
    fn new_initializes_head_cache_to_zero() {
        let rb = RingBuffer::new();
        // SAFETY: this test is single-threaded; no other reference aliases
        // the UnsafeCell.
        let val = unsafe { *rb.head_cache.get() };
        assert_eq!(val, 0);
    }

    #[test]
    fn new_sets_live_to_true() {
        let rb = RingBuffer::new();
        assert!(rb.live.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn new_zero_initializes_data_region() {
        let rb = RingBuffer::new();
        // SAFETY: single-threaded test; no concurrent writer aliases the ring.
        let data = unsafe { std::slice::from_raw_parts(rb.data.as_ptr() as *const u8, RING_SIZE) };
        assert_eq!(data.len(), RING_SIZE);
        assert!(data.iter().all(|&b| b == 0));
    }

    #[test]
    fn pad_fields_are_zeroed() {
        let rb = RingBuffer::new();
        assert!(rb._pad_p.iter().all(|&b| b == 0));
        assert!(rb._pad_d.iter().all(|&b| b == 0));
    }

    #[test]
    fn align_up_already_aligned() {
        assert_eq!(align_up(64, 64), 64);
        assert_eq!(align_up(128, 64), 128);
        assert_eq!(align_up(0, 64), 0);
    }

    #[test]
    fn align_up_not_aligned() {
        assert_eq!(align_up(1, 64), 64);
        assert_eq!(align_up(63, 64), 64);
        assert_eq!(align_up(65, 64), 128);
    }

    #[test]
    fn align_up_align_1_is_identity() {
        assert_eq!(align_up(0, 1), 0);
        assert_eq!(align_up(1, 1), 1);
        assert_eq!(align_up(42, 1), 42);
        assert_eq!(align_up(u64::MAX, 1), u64::MAX);
    }

    #[test]
    fn align_up_power_of_two_aligns() {
        assert_eq!(align_up(0, 2), 0);
        assert_eq!(align_up(1, 2), 2);
        assert_eq!(align_up(2, 2), 2);
        assert_eq!(align_up(3, 2), 4);

        assert_eq!(align_up(0, 8), 0);
        assert_eq!(align_up(7, 8), 8);
        assert_eq!(align_up(8, 8), 8);
        assert_eq!(align_up(9, 8), 16);
    }

    #[test]
    fn align_up_large_values() {
        // Near u64::MAX, ensure no overflow.
        let n = u64::MAX - 100;
        let aligned = align_up(n, 64);
        // Should round up to the next multiple of 64.
        assert_eq!(aligned % 64, 0);
        assert!(aligned >= n);
    }

    #[test]
    fn align_up_slot_size_boundary() {
        // Record sizes near common boundaries.
        let slot = SLOT_SIZE as u64;
        assert_eq!(align_up(0, slot), 0);
        assert_eq!(align_up(1, slot), slot);
        assert_eq!(align_up(slot - 1, slot), slot);
        assert_eq!(align_up(slot, slot), slot);
        assert_eq!(align_up(slot + 1, slot), 2 * slot);
    }

    #[test]
    fn bitmask_is_ring_size_minus_one() {
        let mask = (RING_SIZE - 1) as u64;
        // For a 1MB ring, mask should be 0xFFFFF (20 bits of 1s).
        assert_eq!(mask, 0xFFFFF);
        // mask + 1 should be exactly RING_SIZE.
        assert_eq!(mask + 1, RING_SIZE as u64);
    }

    #[test]
    fn head_wrap_with_bitmask() {
        // Free-running u64 head wraps naturally via bitmask.
        let head: u64 = RING_SIZE as u64 + 42;
        let offset = head & (RING_SIZE as u64 - 1);
        assert_eq!(offset, 42);
    }

    #[test]
    fn head_at_exact_capacity_wraps_to_zero() {
        let head: u64 = RING_SIZE as u64;
        let offset = head & (RING_SIZE as u64 - 1);
        assert_eq!(offset, 0);
    }

    #[test]
    fn ring_buffer_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<RingBuffer>();
    }

    #[test]
    fn ring_buffer_is_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<RingBuffer>();
    }

    #[test]
    fn write_record_places_record_and_advances_head() {
        let rb = RingBuffer::new();
        let record = [0xABu8; 40];
        let len = record.len();
        let slot = rb.reserve(len, Backpressure::Drop).unwrap();
        unsafe {
            std::ptr::copy_nonoverlapping(record.as_ptr(), slot.ptr, len);
        }
        rb.publish(slot);

        // Head advances by the slot-aligned record size.
        let aligned = align_up(40, SLOT_SIZE as u64);
        assert_eq!(rb.head.load(Ordering::Relaxed), aligned);

        // The bytes landed at offset 0.
        let data = unsafe { std::slice::from_raw_parts(rb.data.as_ptr() as *const u8, RING_SIZE) };
        assert_eq!(&data[..40], &record[..]);
    }

    #[test]
    fn write_record_wraps_with_eob_at_ring_end() {
        let rb = RingBuffer::new();
        let slot = SLOT_SIZE as u64;
        // Position the producer one slot from the physical end, ring empty.
        let start = RING_SIZE as u64 - slot;
        rb.head.store(start, Ordering::Relaxed);
        rb.tail.store(start, Ordering::Relaxed);
        unsafe { *rb.tail_cache.get() = start };

        // A record longer than one slot cannot fit the final slot, forcing a
        // wrap: an EOB fills the last slot and the record lands at offset 0.
        let record = vec![0xCDu8; slot as usize + 1];
        let aligned = align_up(record.len() as u64, slot); // == 2 * slot
        let len = record.len();
        let res = rb.reserve(len, Backpressure::Drop).unwrap();
        unsafe {
            std::ptr::copy_nonoverlapping(record.as_ptr(), res.ptr, len);
        }
        rb.publish(res);

        // Head advanced past the EOB filler (one slot) and the record.
        assert_eq!(rb.head.load(Ordering::Relaxed), start + slot + aligned);

        let data = unsafe { std::slice::from_raw_parts(rb.data.as_ptr() as *const u8, RING_SIZE) };
        // EOB header sits at the old offset and spans exactly one slot.
        let eob = (start & (RING_SIZE as u64 - 1)) as usize;
        assert_eq!(data[eob], VERSION);
        assert_eq!(data[eob + 1], END_OF_BUFFER);
        assert_eq!(
            u16::from_le_bytes([data[eob + 2], data[eob + 3]]) as u64,
            slot
        );
        // The record wrapped to offset 0.
        assert_eq!(&data[..record.len()], &record[..]);
    }

    #[test]
    fn write_record_drops_when_full_under_drop_policy() {
        let rb = RingBuffer::new();
        // Ring completely full: head is RING_SIZE ahead of tail.
        rb.head.store(RING_SIZE as u64, Ordering::Relaxed);
        rb.tail.store(0, Ordering::Relaxed);
        unsafe { *rb.tail_cache.get() = 0 };

        let record = [0u8; 40];
        assert!(rb.reserve(record.len(), Backpressure::Drop).is_none());
        // Head is unchanged: nothing was written.
        assert_eq!(rb.head.load(Ordering::Relaxed), RING_SIZE as u64);
    }

    #[test]
    fn write_record_block_unblocks_when_drain_frees_space() {
        let rb = std::sync::Arc::new(RingBuffer::new());
        // Start completely full.
        rb.head.store(RING_SIZE as u64, Ordering::Relaxed);
        rb.tail.store(0, Ordering::Relaxed);
        unsafe { *rb.tail_cache.get() = 0 };

        // A drain-role thread frees the whole ring shortly.
        let drain = std::sync::Arc::clone(&rb);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            drain.tail.store(RING_SIZE as u64, Ordering::Release);
        });

        let record = [0x5Au8; 40];
        // Blocks until the drain thread frees space, then writes.
        let len = record.len();
        let slot = rb.reserve(len, Backpressure::Block).unwrap();
        unsafe {
            std::ptr::copy_nonoverlapping(record.as_ptr(), slot.ptr, len);
        }
        rb.publish(slot);
        handle.join().unwrap();

        let aligned = align_up(40, SLOT_SIZE as u64);
        assert_eq!(rb.head.load(Ordering::Relaxed), RING_SIZE as u64 + aligned);
        let data = unsafe { std::slice::from_raw_parts(rb.data.as_ptr() as *const u8, RING_SIZE) };
        assert_eq!(&data[..40], &record[..]);
    }
}
