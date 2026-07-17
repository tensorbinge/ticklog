//! The drain thread: consumes encoded records from every registered ring
//! buffer and writes formatted log lines to the sink.
//!
//! This is the only module that performs raw-pointer arithmetic. All byte
//! access goes through [`Cursor`], the sole audit point for unsafe reads.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::encode::{FIXED_SIZES, TAG_COUNT, TAG_STR};
use crate::format::{self, FormatSpec};
use crate::level::Level;
use crate::record::{
    END_OF_BUFFER, FLAG_COMPLEX, FLAG_FORMAT, FLAG_PROCESS, FLAG_SOURCE, FLAG_THREAD, HEADER_SIZE,
    LOG_RECORD, VERSION,
};
use crate::ring::{RING_SIZE, RingBuffer, SLOT_SIZE, align_up};
use crate::sink::LogSink;
use crate::thread_buf::REGISTRY;
use crate::timestamp::{Calibration, format_iso8601, ticks_to_ns};

/// Spin batch used when the drain is freshly idle or has just done work.
const SPIN_MIN: u32 = 8;
/// Upper bound on the spin batch. Caps worst-case wakeup latency.
const SPIN_CAP: u32 = 256;
/// Re-scan the ring registry once every this many poll iterations. Must be a
/// power of two so the free-running sync counter stays aligned across its u32
/// wrap. Bounds how stale the drain's local ring list can become without
/// locking the registry mutex on every pass.
const SYNC_EVERY: u32 = 1024;
/// Mask selecting one pass in every `SYNC_EVERY`. Valid because `SYNC_EVERY` is
/// a power of two.
const SYNC_MASK: u32 = SYNC_EVERY - 1;

// Fail the build if SYNC_EVERY is ever set to a non-power-of-two, which would
// misalign SYNC_MASK across the counter's u32 wrap.
const _: () = assert!(SYNC_EVERY.is_power_of_two());

/// A bounded raw-pointer byte parser over one record slice. Every read is
/// clamped to `[base, base + len)`, so a corrupt in-record length can never read
/// past the slice: a fixed-width read that would overrun yields `0`, and
/// `read_bytes` returns a short slice. This is defense-in-depth on a cooperative
/// single-process channel, and runs off the hot path.
struct Cursor {
    ptr: *const u8,
    end: *const u8,
}

impl Cursor {
    /// Creates a cursor over the `len`-byte record slice starting at `base`.
    ///
    /// # Safety
    ///
    /// `base` must point at the start of an initialized region of at least
    /// `len` bytes; reads never cross `base + len`.
    #[inline(always)]
    unsafe fn new(base: *const u8, len: usize) -> Self {
        Self {
            ptr: base,
            // SAFETY: the caller's contract guarantees `base` starts an
            // initialized region of at least `len` bytes, so `base + len` is at
            // most one-past-the-end and is valid to form (it is never dereferenced,
            // only compared against `ptr`).
            end: unsafe { base.add(len) },
        }
    }

    /// Bytes remaining before the end of the record slice.
    #[inline(always)]
    fn remaining(&self) -> usize {
        self.end as usize - self.ptr as usize
    }

    /// Reads the next `N` bytes as an array, advancing past them; returns
    /// `[0; N]` if fewer than `N` remain. `N` is inferred from the caller's
    /// `from_le_bytes` target type, so the width is never a literal.
    #[inline(always)]
    unsafe fn read_array<const N: usize>(&mut self) -> [u8; N] {
        if self.remaining() < N {
            return [0; N];
        }
        // SAFETY: N readable bytes remain; `[u8; N]` has alignment 1, so the
        // cast-and-copy is a valid unaligned load. The pointer advances past
        // exactly the N bytes consumed.
        let v = unsafe { *(self.ptr as *const [u8; N]) };
        self.ptr = unsafe { self.ptr.add(N) };
        v
    }

    #[inline(always)]
    unsafe fn read_u8(&mut self) -> u8 {
        // SAFETY: `read_array` is bounds-clamped (it yields `[0; N]` when fewer
        // than N bytes remain), so it never reads past `end`; here N = 1.
        u8::from_le_bytes(unsafe { self.read_array() })
    }

    #[inline(always)]
    unsafe fn read_u16(&mut self) -> u16 {
        // SAFETY: `read_array` is bounds-clamped and never reads past `end`; N = 2.
        u16::from_le_bytes(unsafe { self.read_array() })
    }

    #[inline(always)]
    unsafe fn read_u32(&mut self) -> u32 {
        // SAFETY: `read_array` is bounds-clamped and never reads past `end`; N = 4.
        u32::from_le_bytes(unsafe { self.read_array() })
    }

    #[inline(always)]
    unsafe fn read_u64(&mut self) -> u64 {
        // SAFETY: `read_array` is bounds-clamped and never reads past `end`; N = 8.
        u64::from_le_bytes(unsafe { self.read_array() })
    }

    #[inline(always)]
    unsafe fn skip(&mut self, n: usize) {
        let n = n.min(self.remaining());
        // SAFETY: `n` is clamped to the bytes remaining, so the advanced pointer
        // stays within `[base, end]`.
        self.ptr = unsafe { self.ptr.add(n) };
    }

    /// Returns the next `min(n, remaining)` bytes and advances past them. A
    /// short return means the record ended early (corruption).
    #[inline(always)]
    unsafe fn read_bytes(&mut self, n: usize) -> &[u8] {
        let n = n.min(self.remaining());
        // SAFETY: `n` is clamped to the remaining initialized bytes of the slice.
        let data = unsafe { std::slice::from_raw_parts(self.ptr, n) };
        self.ptr = unsafe { self.ptr.add(n) };
        data
    }

    /// Reads a `u16` length prefix, then returns the following
    /// `min(len, remaining)` bytes (clamped, like `read_bytes`).
    #[inline(always)]
    unsafe fn read_len_prefixed(&mut self) -> (&[u8], u16) {
        // SAFETY: `read_u16` and `read_bytes` are both bounds-clamped and never
        // read past `end`; a corrupt `len` only yields a short slice.
        let len = unsafe { self.read_u16() };
        let data = unsafe { self.read_bytes(len as usize) };
        (data, len)
    }
}

/// A formatter reads a fixed-size (or already length-stripped) argument payload,
/// decodes it to its native type, and appends the formatted value to `buf`.
type Formatter = fn(&[u8], &FormatSpec, &mut Vec<u8>);

/// Function-pointer dispatch table indexed by type tag (0x00..=0x0B). Sized to
/// [`TAG_COUNT`] so it stays in lockstep with [`FIXED_SIZES`]; `format_arg`
/// bounds the tag against `FIXED_SIZES.len()` before indexing here.
static FORMATTERS: [Formatter; TAG_COUNT] = [
    fmt_u64,  // 0x00
    fmt_i64,  // 0x01
    fmt_f64,  // 0x02
    fmt_u32,  // 0x03
    fmt_i32,  // 0x04
    fmt_f32,  // 0x05
    fmt_u16,  // 0x06
    fmt_i16,  // 0x07
    fmt_u8,   // 0x08
    fmt_i8,   // 0x09
    fmt_bool, // 0x0A
    fmt_str,  // 0x0B
];

fn fmt_u64(data: &[u8], spec: &FormatSpec, buf: &mut Vec<u8>) {
    let bytes: [u8; 8] = data
        .try_into()
        .expect("invariant: u64 argument must be 8 bytes");
    format::format_u64(u64::from_le_bytes(bytes), spec, buf);
}

fn fmt_i64(data: &[u8], spec: &FormatSpec, buf: &mut Vec<u8>) {
    let bytes: [u8; 8] = data
        .try_into()
        .expect("invariant: i64 argument must be 8 bytes");
    format::format_i64(i64::from_le_bytes(bytes), spec, buf);
}

fn fmt_f64(data: &[u8], spec: &FormatSpec, buf: &mut Vec<u8>) {
    let bytes: [u8; 8] = data
        .try_into()
        .expect("invariant: f64 argument must be 8 bytes");
    format::format_f64(f64::from_le_bytes(bytes), spec, buf);
}

fn fmt_u32(data: &[u8], spec: &FormatSpec, buf: &mut Vec<u8>) {
    let bytes: [u8; 4] = data
        .try_into()
        .expect("invariant: u32 argument must be 4 bytes");
    format::format_u32(u32::from_le_bytes(bytes), spec, buf);
}

fn fmt_i32(data: &[u8], spec: &FormatSpec, buf: &mut Vec<u8>) {
    let bytes: [u8; 4] = data
        .try_into()
        .expect("invariant: i32 argument must be 4 bytes");
    format::format_i32(i32::from_le_bytes(bytes), spec, buf);
}

fn fmt_f32(data: &[u8], spec: &FormatSpec, buf: &mut Vec<u8>) {
    let bytes: [u8; 4] = data
        .try_into()
        .expect("invariant: f32 argument must be 4 bytes");
    format::format_f32(f32::from_le_bytes(bytes), spec, buf);
}

fn fmt_u16(data: &[u8], spec: &FormatSpec, buf: &mut Vec<u8>) {
    let bytes: [u8; 2] = data
        .try_into()
        .expect("invariant: u16 argument must be 2 bytes");
    format::format_u16(u16::from_le_bytes(bytes), spec, buf);
}

fn fmt_i16(data: &[u8], spec: &FormatSpec, buf: &mut Vec<u8>) {
    let bytes: [u8; 2] = data
        .try_into()
        .expect("invariant: i16 argument must be 2 bytes");
    format::format_i16(i16::from_le_bytes(bytes), spec, buf);
}

fn fmt_u8(data: &[u8], spec: &FormatSpec, buf: &mut Vec<u8>) {
    format::format_u8(data[0], spec, buf);
}

fn fmt_i8(data: &[u8], spec: &FormatSpec, buf: &mut Vec<u8>) {
    format::format_i8(data[0] as i8, spec, buf);
}

fn fmt_bool(data: &[u8], spec: &FormatSpec, buf: &mut Vec<u8>) {
    format::format_bool(data[0] != 0, spec, buf);
}

fn fmt_str(data: &[u8], spec: &FormatSpec, buf: &mut Vec<u8>) {
    // `from_utf8_lossy` borrows when the bytes are valid UTF-8 (the normal
    // case) and only allocates to substitute replacement chars if a corrupted
    // ring ever yields invalid bytes. No unsafe, no UB.
    let s = String::from_utf8_lossy(data);
    format::format_str(&s, spec, buf);
}

/// Controls which optional fields appear in each formatted line.
pub(crate) struct LogMetadata {
    /// Render the source file path.
    pub(crate) file: bool,
    /// Append `:line` after the file path.
    pub(crate) line_number: bool,
    /// Render the thread name. Default false.
    pub(crate) thread_name: bool,
    /// Render `ThreadId(N)`. Default false.
    pub(crate) thread_id: bool,
}

impl Default for LogMetadata {
    fn default() -> Self {
        Self {
            file: true,
            line_number: true,
            thread_name: false,
            thread_id: false,
        }
    }
}

/// The drain thread state: the sink, formatting configuration, the shutdown
/// signal, and the drain's private list of ring buffers.
pub(crate) struct Drain {
    sink: Box<dyn LogSink>,
    timezone_offset: i32,
    metadata: LogMetadata,
    shutdown: Arc<AtomicBool>,
    rings: Vec<Arc<RingBuffer>>,
    calibration: Calibration,
}

impl Drain {
    /// Creates a drain. The `shutdown` flag must be the same one the owning
    /// [`Guard`](crate::Guard) sets on drop; `calibration` is the counter-to-
    /// wall-clock mapping sampled once at startup.
    pub(crate) fn new(
        sink: Box<dyn LogSink>,
        timezone_offset: i32,
        metadata: LogMetadata,
        shutdown: Arc<AtomicBool>,
        calibration: Calibration,
    ) -> Self {
        Self {
            sink,
            timezone_offset,
            metadata,
            shutdown,
            rings: Vec::new(),
            calibration,
        }
    }

    /// Runs the poll loop until shutdown is signaled, then does one final pass
    /// so records written just before shutdown are not lost.
    pub(crate) fn run(&mut self) {
        let mask = (RING_SIZE - 1) as u64;
        // Reused across every record; cleared per record. No per-record alloc.
        let mut buf = Vec::with_capacity(4096);
        // Idle backoff counter. The idle path reads no clock and takes no lock.
        let mut spin = SPIN_MIN;
        // Free-running ring-sync cadence counter; wraps at u32::MAX.
        let mut since_sync: u32 = 0;

        // Set when a poll emits records and cleared by the flush that follows
        // the drain going idle, so a buffered sink is flushed exactly once per
        // busy-then-idle transition rather than on every idle iteration.
        let mut dirty = false;

        loop {
            if self.shutdown.load(Ordering::Acquire) {
                break;
            }

            // Sync on a coarse cadence, not every iteration: sync_rings() locks
            // the global registry mutex. since_sync free-runs and wraps;
            // SYNC_EVERY being a power of two keeps the mask aligned across the
            // wrap, so every SYNC_EVERY-th pass syncs. A ring registered between
            // syncs keeps its records in its own buffer until the next sync.
            if (since_sync & SYNC_MASK) == 0 {
                self.sync_rings(mask, &mut buf);
            }
            since_sync = since_sync.wrapping_add(1);

            if self.poll_once(mask, &mut buf) {
                spin = SPIN_MIN; // work found: reset backoff and re-poll now
                dirty = true;
                continue;
            }

            // Just caught up: flush the buffered sink once so records emitted in
            // the burst above become visible without a syscall per record.
            if dirty {
                self.flush_sink();
                dirty = false;
            }

            // Idle: back off with a capped, doubling spin. No clock, no lock.
            for _ in 0..spin {
                std::hint::spin_loop();
            }
            spin = (spin << 1).min(SPIN_CAP); // no overflow: capped
        }

        // Final pass: pick up any last records, including from rings whose
        // producers exited after the previous sync, then flush so nothing is
        // left buffered when the drain exits.
        self.sync_rings(mask, &mut buf);
        self.poll_once(mask, &mut buf);
        self.flush_sink();
    }

    /// Flushes the sink, logging any error to stderr. Called when the drain
    /// goes idle and once more at shutdown.
    fn flush_sink(&mut self) {
        if let Err(e) = self.sink.flush() {
            eprintln!("ticklog: sink flush failed: {}", e);
        }
    }

    /// Syncs the drain's local ring list with the global registry: adds newly
    /// registered rings, and gives each ring whose producer has exited one final
    /// drain before dropping it. The registry lock is released before that
    /// drain, so sink I/O never runs under the lock.
    fn sync_rings(&mut self, mask: u64, buf: &mut Vec<u8>) {
        let mut registry = REGISTRY
            .get()
            .expect("invariant: ring registry must be initialized by configure! before the drain starts")
            .lock()
            .expect("invariant: ring registry mutex poisoned by a panic in another thread");

        // Add rings not yet tracked locally. Arc::ptr_eq compares the
        // allocation, avoiding a refcount bump for the membership check.
        for ring in registry.iter() {
            if !self.rings.iter().any(|r| Arc::ptr_eq(r, ring)) {
                self.rings.push(Arc::clone(ring));
            }
        }

        // Drop dead rings from the shared registry. Acquire pairs with the
        // producer's Release store of `live`.
        registry.retain(|r| r.live.load(Ordering::Acquire));
        // Release the lock before the final drain: it performs sink I/O, which
        // must never run under the registry mutex.
        drop(registry);

        // Give each dead ring one last drain before dropping it locally. A
        // single liveness check per ring decides both drain and removal, so a
        // ring cannot slip from "kept" to "dropped" between the two. Once
        // `live == false` is observed (Acquire, pairing with the producer's
        // Release store), the producer is gone and its head is final, so this
        // drain captures every record it published before exiting. That is the
        // guarantee the live flag exists to provide.
        let mut i = 0;
        while i < self.rings.len() {
            if self.rings[i].live.load(Ordering::Acquire) {
                i += 1;
            } else {
                let ring = self.rings.swap_remove(i);
                drain_ring(
                    &ring,
                    mask,
                    self.sink.as_mut(),
                    self.timezone_offset,
                    &self.metadata,
                    &self.calibration,
                    buf,
                );
            }
        }
    }

    /// Drains every ring once, emitting all records published since the last
    /// pass. Returns `true` if any record was processed.
    fn poll_once(&mut self, mask: u64, buf: &mut Vec<u8>) -> bool {
        let mut had_work = false;
        for ring in &self.rings {
            if drain_ring(
                ring,
                mask,
                self.sink.as_mut(),
                self.timezone_offset,
                &self.metadata,
                &self.calibration,
                buf,
            ) {
                had_work = true;
            }
        }
        had_work
    }
}

/// Drains one ring: emits every record in `[tail, head)` to `sink` and
/// publishes the advanced tail. Returns `true` if any record range was
/// processed.
///
/// Takes the drain's formatting inputs explicitly rather than `&self` so it can
/// run both from the poll loop (over live rings) and from `sync_rings` (a final
/// drain of a dead ring before it is dropped).
fn drain_ring(
    ring: &RingBuffer,
    mask: u64,
    sink: &mut dyn LogSink,
    timezone_offset: i32,
    metadata: &LogMetadata,
    calibration: &Calibration,
    buf: &mut Vec<u8>,
) -> bool {
    // Own index: Relaxed load; the drain is the sole writer of `tail`.
    let mut tail = ring.tail.load(Ordering::Relaxed);
    // SAFETY: head_cache is drain-private; no producer touches it and the
    // drain is single-threaded, so this raw access is unaliased.
    let mut head_cache = unsafe { *ring.head_cache.get() };

    // Cached-index fast path: pay the cross-core Acquire load of `head` only
    // when the cache says we have caught up.
    if tail == head_cache {
        head_cache = ring.head.load(Ordering::Acquire);
        if tail == head_cache {
            return false; // no new records in this ring
        }
    }

    // The drain reaches the buffer through a base pointer taken from the shared
    // `data` cells, never a slice reference that would race the producer's
    // write. It reads only [tail, head_cache), a region the producer published
    // via its Release store of `head` and will not overwrite while tail lags.
    let base = ring.data.as_ptr() as *const u8;

    while tail < head_cache {
        let offset = (tail & mask) as usize;

        // SAFETY: `offset` is within the ring; the prefix cursor is bounded to
        // the bytes from `offset` to the end of the ring storage.
        let (version, rectype, total_size) = unsafe {
            let mut c = Cursor::new(base.add(offset), RING_SIZE - offset);
            let version = c.read_u8();
            let rectype = c.read_u8();
            let total_size = c.read_u16() as u64;
            (version, rectype, total_size)
        };

        if total_size == 0 {
            break; // empty slot: nothing more published in this ring
        }

        // Layer A: reject a corrupt frame before trusting `total_size` to slice
        // or advance the tail. On a cooperative single-process channel this
        // never fires; if it does, resync to the producer's published head and
        // resume rather than walk off into garbage.
        if version != VERSION
            || (rectype != LOG_RECORD && rectype != END_OF_BUFFER)
            || total_size < HEADER_SIZE as u64
            || offset + total_size as usize > RING_SIZE
        {
            eprintln!(
                "ticklog: drain discarded a corrupt record \
                 (version={version}, type={rectype}, size={total_size}); resyncing to head"
            );
            tail = head_cache;
            break;
        }

        if rectype == END_OF_BUFFER {
            tail += align_up(total_size, SLOT_SIZE as u64);
            continue; // `tail & mask` wraps to 0 at the ring boundary
        }

        // SAFETY: `total_size` frames one complete record inside the ring
        // (validated above), so this slice is initialized and in-bounds.
        let record = unsafe { std::slice::from_raw_parts(base.add(offset), total_size as usize) };
        let level = record
            .get(4)
            .and_then(|&b| Level::from_u8(b))
            .unwrap_or(Level::Error);

        buf.clear();
        decode_and_format(record, timezone_offset, metadata, calibration, buf);
        if let Err(e) = sink.accept(buf, level) {
            eprintln!("ticklog: sink accept failed: {}", e);
        }

        tail += align_up(total_size, SLOT_SIZE as u64);
    }

    // Publish the consumed position. Release pairs with the producer's Acquire
    // load of `tail` in its capacity check, so the producer never overwrites a
    // slot the drain has not finished reading.
    ring.tail.store(tail, Ordering::Release);
    // SAFETY: head_cache is drain-private, as above.
    unsafe {
        *ring.head_cache.get() = head_cache;
    }

    true
}

/// Decodes one encoded record and appends a formatted line to `buf`.
///
/// The line carries no trailing newline: per the [`LogSink`] contract the sink
/// terminates lines itself (the built-in sinks append `\n`).
///
/// The caller has validated that `record` spans `total_size` bytes of a
/// `LOG_RECORD`. All formatting into `Vec<u8>` is infallible; unknown argument
/// tags produce a placeholder rather than panicking.
fn decode_and_format(
    record: &[u8],
    timezone_offset: i32,
    metadata: &LogMetadata,
    calibration: &Calibration,
    buf: &mut Vec<u8>,
) {
    // SAFETY: `record.as_ptr()` starts a validated record slice of length
    // total_size (>= HEADER_SIZE for a LOG_RECORD); every read below advances
    // within that slice for a well-formed record.
    let mut c = unsafe { Cursor::new(record.as_ptr(), record.len()) };

    // Step 1: header. Only level, flags, and timestamp are needed here; the
    // version/type/total_size prefix was consumed by the poll loop.
    // SAFETY: the 16-byte header is present for every LOG_RECORD.
    let (level_byte, flags, timestamp) = unsafe {
        c.skip(4); // version, type, total_size
        let level_byte = c.read_u8();
        let flags = c.read_u16();
        c.skip(1); // _pad
        let timestamp = c.read_u64();
        (level_byte, flags, timestamp)
    };
    let level = Level::from_u8(level_byte).unwrap_or(Level::Error);

    // Step 2: timestamp and level prefix.
    let ns = ticks_to_ns(timestamp, calibration);
    format_iso8601(ns, timezone_offset, buf);
    buf.extend_from_slice(b"  ");
    buf.extend_from_slice(level.as_str().as_bytes());
    buf.push(b' ');

    // Step 3: flagged sections, in fixed order.
    let mut fmt: &str = "";
    if flags & FLAG_FORMAT != 0 {
        // SAFETY: the format section is 10 bytes (u64 ptr + u16 len) present
        // when FLAG_FORMAT is set.
        let (fmt_ptr, fmt_len) = unsafe {
            let p = std::ptr::with_exposed_provenance::<u8>(c.read_u64() as usize);
            let l = c.read_u16() as usize;
            (p, l)
        };
        // SAFETY: fmt_ptr/fmt_len are the address and length of the format
        // string. The logging macro accepts only a string literal, so the
        // format string is always a &'static str in the binary's read-only
        // data (valid for the whole process, this drain thread included) and
        // the producer wrote fmt.len() as the u16 length, so this reads exactly
        // that string's bytes. No runtime pointer check is needed: no API path
        // can place a non-static pointer here.
        let fmt_bytes = unsafe { std::slice::from_raw_parts(fmt_ptr, fmt_len) };
        fmt = std::str::from_utf8(fmt_bytes).unwrap_or("");
    }
    // Read source and thread sections (defer rendering until both are known).
    let mut file_line: Option<(&str, u32)> = None;
    if flags & FLAG_SOURCE != 0 {
        // SAFETY: the source section is 14 bytes (u64 ptr + u16 len + u32 line)
        // present when FLAG_SOURCE is set.
        let (file_ptr, file_len, line) = unsafe {
            let p = std::ptr::with_exposed_provenance::<u8>(c.read_u64() as usize);
            let l = c.read_u16() as usize;
            let ln = c.read_u32();
            (p, l, ln)
        };
        // SAFETY: file_ptr/file_len come from a &'static str (file!()) in
        // read-only data, valid for the whole process.
        let file_bytes = unsafe { std::slice::from_raw_parts(file_ptr, file_len) };
        let file = std::str::from_utf8(file_bytes).unwrap_or("<file>");
        file_line = Some((file, line));
    }
    let mut thread_id: u64 = 0;
    let mut thread_name: Option<String> = None;
    if flags & FLAG_THREAD != 0 {
        // SAFETY: the thread section is an 8-byte id followed by a
        // length-prefixed name, all within the record slice.
        unsafe {
            thread_id = c.read_u64();
            thread_name = {
                let (name_bytes, name_len) = c.read_len_prefixed();
                if name_len > 0 {
                    std::str::from_utf8(name_bytes).ok().map(String::from)
                } else {
                    None
                }
            };
        }
    }
    if flags & FLAG_PROCESS != 0 {
        // SAFETY: the process section is a 4-byte pid.
        unsafe {
            c.skip(4);
        }
    }
    if flags & FLAG_COMPLEX != 0 {
        // SAFETY: the complex section starts with an 8-byte pointer.
        unsafe {
            c.skip(8);
        }
    }

    // Step 4: render optional fields (thread, then source).
    if metadata.thread_name {
        if let Some(ref name) = thread_name {
            buf.extend_from_slice(name.as_bytes());
            buf.push(b' ');
        }
    }
    if metadata.thread_id {
        buf.extend_from_slice(b"ThreadId(");
        append_u64(thread_id, buf);
        buf.push(b')');
        buf.push(b' ');
    }
    if let Some((file, line)) = file_line {
        if metadata.file {
            buf.extend_from_slice(file.as_bytes());
            if metadata.line_number {
                buf.push(b':');
                append_u64(line as u64, buf);
            }
            buf.push(b' ');
        }
    }

    // Step 4: interleave the format string with the arguments.
    // SAFETY: n_args (u8) is followed by exactly n_args tag bytes.
    let n_args = unsafe { c.read_u8() } as usize;
    let mut tag_buf = [0u8; 256];
    // A well-framed record has n_args tags here, but a corrupt frame may hold
    // fewer: copy exactly the count read_bytes returns (never more than n_args,
    // so it fits tag_buf) so the two slices are equal-length and the copy cannot
    // panic. interleave renders any tag that went missing as "<missing arg>".
    // SAFETY: c is bounded to the validated record slice and read_bytes clamps
    // its length to the bytes remaining, so the read stays in-bounds. Copying
    // releases the cursor borrow so the argument payloads can be read next.
    let n_tags = unsafe {
        let tags = c.read_bytes(n_args);
        tag_buf[..tags.len()].copy_from_slice(tags);
        tags.len()
    };
    interleave(fmt, &tag_buf[..n_tags], &mut c, buf);
}

/// Walks the format string, substituting each `{...}` placeholder with the
/// next argument. Literal text and `{{`/`}}` escapes are copied through.
fn interleave(fmt: &str, tags: &[u8], c: &mut Cursor, buf: &mut Vec<u8>) {
    let bytes = fmt.as_bytes();
    let mut i = 0;
    let mut arg_idx = 0;
    // Cleared on the first unknown tag: once the cursor position is unknown,
    // later arguments cannot be read, only reported.
    let mut parse_ok = true;

    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                    buf.push(b'{');
                    i += 2;
                    continue;
                }
                // Find the closing brace. The format string passed compile-time
                // validation, so a match is guaranteed for well-formed input.
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != b'}' {
                    j += 1;
                }
                let spec = format::parse_spec(&fmt[start..j]);
                i = if j < bytes.len() { j + 1 } else { j };

                match tags.get(arg_idx).copied() {
                    Some(tag) if parse_ok => {
                        if !format_arg(tag, &spec, c, buf) {
                            parse_ok = false;
                            write_unknown_tag(tag, buf);
                        }
                    }
                    Some(tag) => write_unknown_tag(tag, buf),
                    None => buf.extend_from_slice(b"<missing arg>"),
                }
                arg_idx += 1;
            }
            b'}' => {
                // The only valid `}` in a compile-time-validated format string
                // is the `}}` escape; a placeholder's closing brace was already
                // consumed by the `{` arm above. A lone `}` is rejected by the
                // macro (as in std's `format!`), so reaching one here is a bug.
                debug_assert!(
                    i + 1 < bytes.len() && bytes[i + 1] == b'}',
                    "invariant: lone '}}' in validated format string"
                );
                buf.push(b'}');
                i += 2;
            }
            other => {
                buf.push(other);
                i += 1;
            }
        }
    }
}

/// Reads and formats one argument of type `tag` from the cursor. Returns
/// `false` for an unknown tag, in which case the cursor is left untouched.
fn format_arg(tag: u8, spec: &FormatSpec, c: &mut Cursor, buf: &mut Vec<u8>) -> bool {
    if tag == TAG_STR {
        // SAFETY: a string argument is a u16 length prefix followed by that
        // many UTF-8 bytes, exactly what read_len_prefixed consumes.
        let (data, _) = unsafe { c.read_len_prefixed() };
        fmt_str(data, spec, buf);
        return true;
    }

    let idx = tag as usize;
    if idx < FIXED_SIZES.len() {
        let size = FIXED_SIZES[idx];
        // SAFETY: `size` is the fixed encoded width for this tag; the producer
        // wrote exactly that many argument bytes here.
        let data = unsafe { c.read_bytes(size) };
        FORMATTERS[idx](data, spec, buf);
        return true;
    }

    false
}

/// Appends `<unknown tag 0xNN>` to `buf`.
fn write_unknown_tag(tag: u8, buf: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    buf.extend_from_slice(b"<unknown tag 0x");
    buf.push(HEX[(tag >> 4) as usize]);
    buf.push(HEX[(tag & 0x0F) as usize]);
    buf.push(b'>');
}

/// Appends the decimal representation of `v` to `buf`.
fn append_u64(mut v: u64, buf: &mut Vec<u8>) {
    // u64::MAX is 20 digits.
    let mut tmp = [0u8; 20];
    let mut i = tmp.len();
    loop {
        i -= 1;
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    buf.extend_from_slice(&tmp[i..]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{TAG_BOOL, TAG_F64, TAG_I64, TAG_U16, TAG_U64};
    use crate::record::{
        FORMAT_SECTION_SIZE, HEADER_SIZE, LOG_RECORD, THREAD_SECTION_BASE_SIZE, VERSION,
    };
    use std::io;
    use std::sync::{Mutex, OnceLock};

    /// Shared handle to the `(line, level)` pairs a [`CaptureSink`] recorded.
    type CaptureCalls = Arc<Mutex<Vec<(String, Level)>>>;

    // ---- test helpers -------------------------------------------------------

    /// Identity calibration: raw tick value equals nanoseconds since epoch.
    fn identity_calibration() -> Calibration {
        Calibration {
            counter_to_ns: 1.0,
            counter_base: 0,
            wall_base_ns: 0,
        }
    }

    /// Encodes a single argument's payload bytes for a fixed-size type.
    fn le_bytes(tag: u8, value: u64) -> (u8, Vec<u8>) {
        let bytes = match tag {
            TAG_U64 | TAG_I64 | TAG_F64 => value.to_le_bytes().to_vec(),
            TAG_U16 => (value as u16).to_le_bytes().to_vec(),
            TAG_BOOL => vec![value as u8],
            _ => panic!("unsupported tag in helper"),
        };
        (tag, bytes)
    }

    /// Encodes a string argument payload: [len u16 LE][utf8 bytes].
    fn str_arg(s: &str) -> (u8, Vec<u8>) {
        let mut v = (s.len() as u16).to_le_bytes().to_vec();
        v.extend_from_slice(s.as_bytes());
        (TAG_STR, v)
    }

    /// Builds a full encoded LOG_RECORD.
    fn build_record(
        level: Level,
        timestamp: u64,
        fmt: &'static str,
        source: Option<(&'static str, u32)>,
        thread_id: u64,
        thread_name: Option<&str>,
        args: &[(u8, Vec<u8>)],
    ) -> Vec<u8> {
        let mut flags: u16 = FLAG_FORMAT | FLAG_THREAD;
        if source.is_some() {
            flags |= FLAG_SOURCE;
        }

        let mut payload: Vec<u8> = Vec::new();

        // Format section.
        payload.extend_from_slice(&(fmt.as_ptr() as u64).to_le_bytes());
        payload.extend_from_slice(&(fmt.len() as u16).to_le_bytes());

        // Source section.
        if let Some((file, line)) = source {
            payload.extend_from_slice(&(file.as_ptr() as u64).to_le_bytes());
            payload.extend_from_slice(&(file.len() as u16).to_le_bytes());
            payload.extend_from_slice(&line.to_le_bytes());
        }

        // Thread section.
        payload.extend_from_slice(&thread_id.to_le_bytes());
        let name_bytes = thread_name.map_or(&b""[..], |n| n.as_bytes());
        payload.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        payload.extend_from_slice(name_bytes);

        // Arguments.
        payload.push(args.len() as u8);
        for (tag, _) in args {
            payload.push(*tag);
        }
        for (_, data) in args {
            payload.extend_from_slice(data);
        }

        let total_size = (HEADER_SIZE + payload.len()) as u16;

        let mut record = Vec::with_capacity(total_size as usize);
        record.push(VERSION); // version
        record.push(LOG_RECORD); // type
        record.extend_from_slice(&total_size.to_le_bytes());
        record.push(level.to_u8());
        record.extend_from_slice(&flags.to_le_bytes());
        record.push(0); // _pad
        record.extend_from_slice(&timestamp.to_le_bytes());
        record.extend_from_slice(&payload);
        record
    }

    fn format_line(record: &[u8], metadata: LogMetadata) -> String {
        let mut buf = Vec::new();
        decode_and_format(record, 0, &metadata, &identity_calibration(), &mut buf);
        String::from_utf8(buf).unwrap()
    }

    /// A sink that captures every accepted `(line, level)` pair.
    struct CaptureSink {
        calls: CaptureCalls,
    }

    impl LogSink for CaptureSink {
        fn accept(&mut self, line: &[u8], level: Level) -> io::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push((String::from_utf8_lossy(line).into_owned(), level));
            Ok(())
        }
    }

    fn capture_drain() -> (Drain, CaptureCalls) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let sink = CaptureSink {
            calls: Arc::clone(&calls),
        };
        let drain = Drain::new(
            Box::new(sink),
            0,
            LogMetadata::default(),
            Arc::new(AtomicBool::new(false)),
            identity_calibration(),
        );
        (drain, calls)
    }

    /// Writes `bytes` into a ring at `offset` and advances `head` by
    /// `align_up(len, SLOT_SIZE)` to mimic the producer.
    fn place_record(ring: &RingBuffer, offset: u64, bytes: &[u8]) {
        // SAFETY: single-threaded test with exclusive access to the ring data.
        let data =
            unsafe { std::slice::from_raw_parts_mut(ring.data.as_ptr() as *mut u8, RING_SIZE) };
        let start = offset as usize;
        data[start..start + bytes.len()].copy_from_slice(bytes);
        let new_head = offset + align_up(bytes.len() as u64, SLOT_SIZE as u64);
        ring.head.store(new_head, Ordering::Release);
    }

    // ---- Cursor tests -------------------------------------------------------

    #[test]
    fn cursor_reads_types_and_advances() {
        let mut data = Vec::new();
        data.push(0xABu8);
        data.extend_from_slice(&0xBEEFu16.to_le_bytes());
        data.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        data.extend_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());

        // SAFETY: the cursor reads exactly the 15 bytes just written.
        unsafe {
            let mut c = Cursor::new(data.as_ptr(), data.len());
            assert_eq!(c.read_u8(), 0xAB);
            assert_eq!(c.read_u16(), 0xBEEF);
            assert_eq!(c.read_u32(), 0xDEAD_BEEF);
            assert_eq!(c.read_u64(), 0x0102_0304_0506_0708);
        }
    }

    #[test]
    fn cursor_skip_and_read_bytes() {
        let data = [1u8, 2, 3, 4, 5, 6];
        // SAFETY: reads/skip stay within the 6-byte array.
        unsafe {
            let mut c = Cursor::new(data.as_ptr(), data.len());
            c.skip(2);
            assert_eq!(c.read_bytes(3), &[3, 4, 5]);
            assert_eq!(c.read_u8(), 6);
        }
    }

    #[test]
    fn cursor_read_len_prefixed() {
        let mut data = (5u16).to_le_bytes().to_vec();
        data.extend_from_slice(b"hello");
        data.push(0x99); // trailing byte after the string
        // SAFETY: the length prefix (5) matches the 5 string bytes present.
        unsafe {
            let mut c = Cursor::new(data.as_ptr(), data.len());
            let (s, len) = c.read_len_prefixed();
            assert_eq!(len, 5);
            assert_eq!(s, b"hello");
            assert_eq!(c.read_u8(), 0x99);
        }
    }

    #[test]
    fn cursor_read_len_prefixed_empty() {
        let data = (0u16).to_le_bytes().to_vec();
        // SAFETY: zero-length string; only the 2-byte prefix is read.
        unsafe {
            let mut c = Cursor::new(data.as_ptr(), data.len());
            let (s, len) = c.read_len_prefixed();
            assert_eq!(len, 0);
            assert!(s.is_empty());
        }
    }

    #[test]
    fn cursor_clamps_reads_at_end() {
        // A 3-byte slice whose reads are all attempted past its end.
        let data = [1u8, 2, 3];
        // SAFETY: the cursor is bounded to these 3 bytes; every read below is
        // clamped, so none touches memory past the slice.
        unsafe {
            let mut c = Cursor::new(data.as_ptr(), data.len());
            // A u64 needs 8 bytes but only 3 remain: yields 0, consumes nothing.
            assert_eq!(c.read_u64(), 0);
            assert_eq!(c.remaining(), 3);
            // read_bytes clamps to what is left.
            assert_eq!(c.read_bytes(100), &[1, 2, 3]);
            assert_eq!(c.remaining(), 0);
            // Exhausted: further fixed-width reads are 0.
            assert_eq!(c.read_u16(), 0);
        }
    }

    #[test]
    fn cursor_len_prefix_clamps_when_string_overruns() {
        // Prefix claims 9 bytes but only 3 follow: read_len_prefixed returns the
        // 3 present rather than reading past the slice.
        let mut data = (9u16).to_le_bytes().to_vec();
        data.extend_from_slice(b"abc");
        // SAFETY: bounded to the 5 bytes written; the over-long prefix is clamped.
        unsafe {
            let mut c = Cursor::new(data.as_ptr(), data.len());
            let (s, len) = c.read_len_prefixed();
            assert_eq!(len, 9);
            assert_eq!(s, b"abc");
        }
    }

    // ---- FORMATTERS tests ---------------------------------------------------

    #[test]
    fn formatter_u64_roundtrip() {
        let mut buf = Vec::new();
        fmt_u64(&42u64.to_le_bytes(), &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"42");
    }

    #[test]
    fn formatter_i64_negative() {
        let mut buf = Vec::new();
        fmt_i64(&(-7i64).to_le_bytes(), &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"-7");
    }

    #[test]
    fn formatter_f64_roundtrip() {
        let mut buf = Vec::new();
        fmt_f64(&3.5f64.to_le_bytes(), &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"3.5");
    }

    #[test]
    fn formatter_bool_and_str() {
        let mut buf = Vec::new();
        fmt_bool(&[1], &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"true");

        buf.clear();
        fmt_str(b"hi", &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"hi");
    }

    #[test]
    fn formatter_str_invalid_utf8_is_lossy() {
        let mut buf = Vec::new();
        // 0xFF is not valid UTF-8; lossy substitution must not panic.
        fmt_str(&[0xFF], &FormatSpec::default(), &mut buf);
        assert_eq!(buf, "\u{FFFD}".as_bytes());
    }

    // ---- decode_and_format tests --------------------------------------------

    #[test]
    fn decode_full_line() {
        let record = build_record(
            Level::Info,
            0,
            "x={}",
            Some(("a.rs", 7)),
            1,
            None,
            &[le_bytes(TAG_U64, 42)],
        );
        let line = format_line(&record, LogMetadata::default());
        assert_eq!(line, "1970-01-01T00:00:00.000000000Z  INFO a.rs:7 x=42",);
    }

    #[test]
    fn decode_hides_source_when_disabled() {
        let record = build_record(Level::Warn, 0, "hi", Some(("a.rs", 7)), 1, None, &[]);
        let meta = LogMetadata {
            file: false,
            line_number: false,
            thread_name: false,
            thread_id: false,
        };
        let line = format_line(&record, meta);
        assert_eq!(line, "1970-01-01T00:00:00.000000000Z  WARN hi");
    }

    #[test]
    fn decode_file_without_line_number() {
        let record = build_record(Level::Info, 0, "m", Some(("a.rs", 7)), 1, None, &[]);
        let meta = LogMetadata {
            file: true,
            line_number: false,
            thread_name: false,
            thread_id: false,
        };
        let line = format_line(&record, meta);
        assert_eq!(line, "1970-01-01T00:00:00.000000000Z  INFO a.rs m");
    }

    #[test]
    fn decode_multiple_args_and_types() {
        let record = build_record(
            Level::Error,
            0,
            "{} {} {}",
            None,
            1,
            None,
            &[le_bytes(TAG_U16, 5), str_arg("ok"), le_bytes(TAG_BOOL, 1)],
        );
        let line = format_line(&record, LogMetadata::default());
        assert_eq!(line, "1970-01-01T00:00:00.000000000Z  ERROR 5 ok true",);
    }

    #[test]
    fn decode_escaped_braces() {
        let record = build_record(
            Level::Info,
            0,
            "{{{}}}",
            None,
            1,
            None,
            &[le_bytes(TAG_U64, 9)],
        );
        let line = format_line(&record, LogMetadata::default());
        assert_eq!(line, "1970-01-01T00:00:00.000000000Z  INFO {9}",);
    }

    #[test]
    fn decode_unknown_tag_emits_placeholder() {
        // Tag 0x7F is not a known type; the drain must not panic.
        let record = build_record(Level::Info, 0, "v={}", None, 1, None, &[(0x7F, vec![0u8])]);
        let line = format_line(&record, LogMetadata::default());
        assert_eq!(
            line,
            "1970-01-01T00:00:00.000000000Z  INFO v=<unknown tag 0x7F>",
        );
    }

    #[test]
    fn decode_missing_arg_placeholder() {
        // Two placeholders but only one argument supplied.
        let record = build_record(
            Level::Info,
            0,
            "{} {}",
            None,
            1,
            None,
            &[le_bytes(TAG_U64, 1)],
        );
        let line = format_line(&record, LogMetadata::default());
        assert_eq!(line, "1970-01-01T00:00:00.000000000Z  INFO 1 <missing arg>",);
    }

    #[test]
    fn decode_tolerates_count_byte_exceeding_tags_present() {
        // Corruption: the count byte claims more args than the record actually
        // holds. The decoder must clamp to the tags present rather than panic on
        // a length-mismatched copy, and report the shortfall as a missing arg.
        let mut record = build_record(Level::Info, 0, "v={}", None, 1, None, &[]);
        // The count byte follows the fixed header, the format section, and the
        // thread section.
        record[HEADER_SIZE + FORMAT_SECTION_SIZE + THREAD_SECTION_BASE_SIZE] = 200;
        let line = format_line(&record, LogMetadata::default());
        assert!(
            line.ends_with("v=<missing arg>"),
            "expected a missing-arg placeholder, got {line:?}"
        );
    }

    #[test]
    fn decode_timestamp_conversion() {
        // With identity calibration, the raw tick is nanoseconds since epoch.
        let record = build_record(Level::Info, 1_234_567_890, "t", None, 1, None, &[]);
        let line = format_line(&record, LogMetadata::default());
        assert_eq!(line, "1970-01-01T00:00:01.234567890Z  INFO t",);
    }

    // ---- poll loop tests ----------------------------------------------------

    #[test]
    fn poll_empty_ring_no_accepts() {
        let (mut drain, calls) = capture_drain();
        drain.rings.push(Arc::new(RingBuffer::new()));
        let mask = (RING_SIZE - 1) as u64;
        let mut buf = Vec::new();
        assert!(!drain.poll_once(mask, &mut buf));
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn poll_one_record_accepts_and_advances_tail() {
        let (mut drain, calls) = capture_drain();
        let ring = Arc::new(RingBuffer::new());
        let record = build_record(
            Level::Info,
            0,
            "x={}",
            Some(("a.rs", 7)),
            1,
            None,
            &[le_bytes(TAG_U64, 42)],
        );
        place_record(&ring, 0, &record);
        let head = ring.head.load(Ordering::Acquire);
        drain.rings.push(Arc::clone(&ring));

        let mask = (RING_SIZE - 1) as u64;
        let mut buf = Vec::new();
        assert!(drain.poll_once(mask, &mut buf));

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0].0,
            "1970-01-01T00:00:00.000000000Z  INFO a.rs:7 x=42",
        );
        assert_eq!(recorded[0].1, Level::Info);
        // tail advanced to head; a second poll finds no work.
        assert_eq!(ring.tail.load(Ordering::Relaxed), head);
        drop(recorded);
        assert!(!drain.poll_once(mask, &mut buf));
    }

    #[test]
    fn poll_discards_corrupt_record_and_resyncs_to_head() {
        let (mut drain, calls) = capture_drain();
        let ring = Arc::new(RingBuffer::new());

        // A valid record, then one whose type byte is neither LOG_RECORD nor
        // END_OF_BUFFER (framing corruption), then a second valid record.
        let r1 = build_record(Level::Info, 0, "first", None, 1, None, &[]);
        place_record(&ring, 0, &r1);
        let off2 = align_up(r1.len() as u64, SLOT_SIZE as u64);

        let mut bad = build_record(Level::Info, 0, "corrupt", None, 1, None, &[]);
        bad[1] = 0x7F; // record type: not LOG_RECORD (1) or END_OF_BUFFER (2)
        place_record(&ring, off2, &bad);
        let off3 = off2 + align_up(bad.len() as u64, SLOT_SIZE as u64);

        let r2 = build_record(Level::Info, 0, "second", None, 1, None, &[]);
        place_record(&ring, off3, &r2);

        drain.rings.push(Arc::clone(&ring));
        let mask = (RING_SIZE - 1) as u64;
        let mut buf = Vec::new();
        drain.poll_once(mask, &mut buf);

        // Only the record before the corruption is emitted: the drain must not
        // decode the corrupt bytes as a record, and resyncs `tail` to the
        // producer's published head rather than desyncing onward.
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1, "got {recorded:?}");
        assert!(recorded[0].0.ends_with("first"), "got {:?}", recorded[0].0);
    }

    #[test]
    fn poll_skips_end_of_buffer_record() {
        let (mut drain, calls) = capture_drain();
        let ring = Arc::new(RingBuffer::new());

        // An EOB record spanning one slot, followed by a real record.
        let mut eob = Vec::new();
        eob.push(0x01); // version
        eob.push(END_OF_BUFFER); // type
        eob.extend_from_slice(&(SLOT_SIZE as u16).to_le_bytes()); // total_size
        eob.resize(SLOT_SIZE, 0); // pad to a full slot
        place_record(&ring, 0, &eob);

        let record = build_record(Level::Info, 0, "hi", None, 1, None, &[]);
        place_record(&ring, SLOT_SIZE as u64, &record);

        drain.rings.push(Arc::clone(&ring));
        let mask = (RING_SIZE - 1) as u64;
        let mut buf = Vec::new();
        assert!(drain.poll_once(mask, &mut buf));

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "1970-01-01T00:00:00.000000000Z  INFO hi",);
    }

    #[test]
    fn poll_multiple_records_in_one_ring() {
        let (mut drain, calls) = capture_drain();
        let ring = Arc::new(RingBuffer::new());

        let r1 = build_record(Level::Info, 0, "a", None, 1, None, &[]);
        let r2 = build_record(Level::Warn, 0, "b", None, 1, None, &[]);
        let slot = SLOT_SIZE as u64;
        // Place two records in consecutive slots and set head past both.
        {
            // SAFETY: single-threaded test with exclusive access.
            let data =
                unsafe { std::slice::from_raw_parts_mut(ring.data.as_ptr() as *mut u8, RING_SIZE) };
            data[..r1.len()].copy_from_slice(&r1);
            let off2 = slot as usize;
            data[off2..off2 + r2.len()].copy_from_slice(&r2);
        }
        ring.head.store(2 * slot, Ordering::Release);
        drain.rings.push(Arc::clone(&ring));

        let mask = (RING_SIZE - 1) as u64;
        let mut buf = Vec::new();
        assert!(drain.poll_once(mask, &mut buf));

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].1, Level::Info);
        assert_eq!(recorded[1].1, Level::Warn);
    }

    // ---- sync_rings tests ---------------------------------------------------

    fn init_registry() {
        static INIT: OnceLock<()> = OnceLock::new();
        INIT.get_or_init(|| {
            let _ = REGISTRY.set(Mutex::new(Vec::new()));
        });
    }

    #[test]
    fn sync_picks_up_and_drops_rings() {
        init_registry();
        let (mut drain, _calls) = capture_drain();
        let ring = Arc::new(RingBuffer::new());
        REGISTRY
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .push(Arc::clone(&ring));

        let mask = (RING_SIZE - 1) as u64;
        let mut buf = Vec::new();
        drain.sync_rings(mask, &mut buf);
        assert!(drain.rings.iter().any(|r| Arc::ptr_eq(r, &ring)));

        // Mark the ring dead; the next sync must drop it.
        ring.live.store(false, Ordering::Release);
        drain.sync_rings(mask, &mut buf);
        assert!(!drain.rings.iter().any(|r| Arc::ptr_eq(r, &ring)));
    }

    #[test]
    fn sync_does_not_add_same_ring_twice() {
        init_registry();
        let (mut drain, _calls) = capture_drain();
        let ring = Arc::new(RingBuffer::new());
        REGISTRY
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .push(Arc::clone(&ring));

        let mask = (RING_SIZE - 1) as u64;
        let mut buf = Vec::new();
        drain.sync_rings(mask, &mut buf);
        drain.sync_rings(mask, &mut buf);
        let count = drain.rings.iter().filter(|r| Arc::ptr_eq(r, &ring)).count();
        assert_eq!(count, 1);
        // Clean up so a dead ring is not left in the shared registry.
        ring.live.store(false, Ordering::Release);
        drain.sync_rings(mask, &mut buf);
    }

    #[test]
    fn sync_drains_dead_ring_before_dropping_it() {
        init_registry();
        let (mut drain, calls) = capture_drain();
        let ring = Arc::new(RingBuffer::new());

        // Thread-exit ordering: publish a record (head advances), then mark the
        // producer dead (live = false) with the record still unconsumed. The
        // ring is kept out of the shared REGISTRY so the assertion on `calls`
        // cannot be perturbed by a ring another parallel test registered.
        let record = build_record(Level::Info, 0, "bye", None, 1, None, &[]);
        place_record(&ring, 0, &record);
        ring.live.store(false, Ordering::Release);
        drain.rings.push(Arc::clone(&ring));

        let mask = (RING_SIZE - 1) as u64;
        let mut buf = Vec::new();
        drain.sync_rings(mask, &mut buf);

        // The dead ring's record must reach the sink before the ring is dropped.
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "1970-01-01T00:00:00.000000000Z  INFO bye",);
        drop(recorded);
        // And the ring is gone from the drain's local list afterwards.
        assert!(!drain.rings.iter().any(|r| Arc::ptr_eq(r, &ring)));
    }

    // ---- Concurrent producer/drain (aliasing model) -------------------------

    /// The producer (`write_record`) and the drain (`drain_ring`) run against
    /// one ring on two threads at once. Both must reach the ring's byte buffer
    /// without ever forming a `&`/`&mut` that spans it: a whole-slice `&mut` on
    /// the producer overlapping a whole-slice `&` on the drain is a
    /// Stacked/Tree-Borrows violation even though the byte ranges are disjoint.
    ///
    /// This is invisible to a normal run; it is a regression guard for Miri:
    ///   MIRIFLAGS="-Zmiri-tree-borrows" \
    ///     cargo +nightly miri test --lib producer_and_drain_concurrent
    #[test]
    fn producer_and_drain_concurrent_no_aliasing_ub() {
        use crate::builder::Backpressure;
        use std::sync::Barrier;

        const N: usize = 8;
        let ring = Arc::new(RingBuffer::new());
        let mask = (RING_SIZE - 1) as u64;
        let record = build_record(
            Level::Info,
            0x1234,
            "hello {}",
            Some(("f.rs", 1)),
            1,
            None,
            &[le_bytes(TAG_U64, 42)],
        );

        // Both threads start their loops together to maximize the window in
        // which the producer and drain hold references to the buffer at once.
        let barrier = Arc::new(Barrier::new(2));

        let producer = {
            let ring = Arc::clone(&ring);
            let barrier = Arc::clone(&barrier);
            let record = record.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..N {
                    let len = record.len();
                    let slot = ring.reserve(len, Backpressure::Block).unwrap();
                    unsafe {
                        std::ptr::copy_nonoverlapping(record.as_ptr(), slot.ptr, len);
                    }
                    ring.publish(slot);
                }
            })
        };

        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut sink = CaptureSink {
            calls: Arc::clone(&calls),
        };
        let metadata = LogMetadata::default();
        let calibration = identity_calibration();
        let mut buf = Vec::new();

        barrier.wait();
        let mut guard = 0;
        while calls.lock().unwrap().len() < N {
            drain_ring(&ring, mask, &mut sink, 0, &metadata, &calibration, &mut buf);
            guard += 1;
            assert!(
                guard < 1_000_000,
                "drain stalled before consuming all records"
            );
        }

        producer.join().unwrap();
        assert_eq!(calls.lock().unwrap().len(), N);
    }
}
