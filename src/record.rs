//! The on-the-wire log record format.
//!
//! Layout constants are shared by the producer (which assembles records here)
//! and the drain (which decodes them), so the two sides cannot drift. The
//! record is a fixed 16-byte header followed by flagged sections:
//!
//! ```text
//! header:  version u8 | type u8 | total_size u16 | level u8 | flags u16 | pad u8 | timestamp u64
//! format:  fmt_ptr u64 | fmt_len u16                          (FLAG_FORMAT)
//! source:  file_ptr u64 | file_len u16 | line u32             (FLAG_SOURCE)
//! args:    count u8 | tag u8 * count | payload bytes * count
//! ```
//!
//! The format and source strings are `&'static str` referenced by pointer, not
//! copied, so they cost nothing on the hot path and are read directly from the
//! binary's read-only data by the drain.

use crate::level::Level;
use core::mem::size_of;

/// Record format version written in byte 0 of every header.
pub(crate) const VERSION: u8 = 0x01;
/// Record type: a normal encoded log record.
pub(crate) const LOG_RECORD: u8 = 1;
/// Record type: filler written before a ring wrap so no record straddles the
/// physical end of the buffer.
pub(crate) const END_OF_BUFFER: u8 = 2;

/// Fixed record header size in bytes: version, type, total_size, level, flags,
/// pad, timestamp. Summed from field widths so it tracks the layout above.
pub(crate) const HEADER_SIZE: usize = size_of::<u8>()  // version
    + size_of::<u8>()   // type
    + size_of::<u16>()  // total_size
    + size_of::<u8>()   // level
    + size_of::<u16>()  // flags
    + size_of::<u8>()   // _pad
    + size_of::<u64>(); // timestamp
/// Encoded size of the format section: an 8-byte pointer and a 2-byte length.
pub(crate) const FORMAT_SECTION_SIZE: usize = size_of::<u64>()  // fmt_ptr
    + size_of::<u16>(); // fmt_len
/// Encoded size of the source section: an 8-byte pointer, a 2-byte length, and
/// a 4-byte line number.
pub(crate) const SOURCE_SECTION_SIZE: usize = size_of::<u64>()  // file_ptr
    + size_of::<u16>()  // file_len
    + size_of::<u32>(); // line
/// Size of the argument count byte that precedes the tags and payloads.
pub(crate) const COUNT_SIZE: usize = size_of::<u8>();

/// Total size of a record's fixed sections, before any arguments: header,
/// format, source, and the count byte. The logging macros hardcode this base
/// (they expand in the caller's crate and cannot read this const); a
/// compile-time assertion in `macros` guards the two against drift.
pub const BASE_RECORD_SIZE: usize =
    HEADER_SIZE + FORMAT_SECTION_SIZE + SOURCE_SECTION_SIZE + COUNT_SIZE;

/// Flag bit: the format-string section is present.
pub(crate) const FLAG_FORMAT: u16 = 0x01;
/// Flag bit: the source-location section is present.
pub(crate) const FLAG_SOURCE: u16 = 0x02;
/// Flag bit: the thread section is present.
pub(crate) const FLAG_THREAD: u16 = 0x04;
/// Flag bit: the process section is present.
pub(crate) const FLAG_PROCESS: u16 = 0x08;
/// Flag bit: the complex-type section is present.
pub(crate) const FLAG_COMPLEX: u16 = 0x10;

/// Largest record the u16 `total_size` header field can frame. A record that
/// would encode larger than this is dropped rather than truncated.
pub(crate) const MAX_RECORD_SIZE: usize = u16::MAX as usize;

/// Assembles a record by writing the fixed sections (header, format, source)
/// into `dst`, then delegating argument encoding to the caller's
/// monomorphized closure.
///
/// The closure receives a mutable slice starting right after the count byte,
/// sized to fit exactly `args_bytes + n_args` bytes (tags + payloads). It is
/// monomorphized per unique argument-type signature, so `Loggable::type_tag()`
/// and `Loggable::encode()` calls inside it resolve to concrete impls with no
/// vtable dispatch.
///
/// # Safety
///
/// `dst` must point to a writable region of at least `total_size` bytes. The
/// caller must guarantee that `write_args` writes exactly `n_args` tag bytes
/// followed by `args_bytes` payload bytes.
// The record fields (header, source location, arg count/size, and the arg-
// writing closure) are genuinely distinct inputs to this monomorphized hot-path
// assembler; bundling them into a struct would add indirection at the single
// call site without making anything clearer.
#[allow(clippy::too_many_arguments)]
#[inline]
pub(crate) fn assemble(
    dst: *mut u8,
    level: Level,
    timestamp: u64,
    fmt: &'static str,
    file: &'static str,
    line: u32,
    n_args: u8,
    total_size: usize,
    write_args: impl FnOnce(&mut [u8]),
) {
    let flags = FLAG_FORMAT | FLAG_SOURCE;
    let total = total_size as u16;
    let level_u8 = level.to_u8();

    debug_assert!(total_size <= MAX_RECORD_SIZE);
    debug_assert!(fmt.len() <= u16::MAX as usize);
    debug_assert!(file.len() <= u16::MAX as usize);

    // SAFETY: The caller guarantees `dst` points to `total_size` writable
    // bytes. Every one of those bytes is written by the `put!`
    // header/section stores and the caller's `write_args` closure, so no
    // uninitialized byte is ever read. The caller's documented contract
    // guarantees `write_args` fills exactly the tags + payloads region.
    unsafe {
        let buf = std::slice::from_raw_parts_mut(dst, total_size);
        let mut pos = 0usize;

        // Writes `$bytes` (a little-endian `[u8; N]`) at `pos` and advances by
        // its own length, so field widths come from the encoding, never a literal.
        macro_rules! put {
            ($bytes:expr) => {{
                let b = $bytes;
                buf[pos..pos + b.len()].copy_from_slice(&b);
                pos += b.len();
            }};
        }

        // Header
        put!([VERSION]);
        put!([LOG_RECORD]);
        put!(total.to_le_bytes());
        put!([level_u8]);
        put!(flags.to_le_bytes());
        put!([0u8]); // _pad
        put!(timestamp.to_le_bytes());

        // Format section
        put!((fmt.as_ptr() as u64).to_le_bytes());
        put!((fmt.len() as u16).to_le_bytes());

        // Source section
        put!((file.as_ptr() as u64).to_le_bytes());
        put!((file.len() as u16).to_le_bytes());
        put!(line.to_le_bytes());

        // Count byte
        put!([n_args]);

        // Delegate to the monomorphized closure for tags + payloads.
        write_args(&mut buf[pos..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::Loggable;

    /// Test helper: wraps [`assemble`] with a `&[&dyn Loggable]` slice for
    /// convenience.  Computes sizes from the slice and delegates to the
    /// real (monomorphized) `assemble` via a closure.
    fn check_assemble(
        scratch: &mut Vec<u8>,
        level: Level,
        timestamp: u64,
        fmt: &'static str,
        file: &'static str,
        line: u32,
        args: &[&dyn Loggable],
    ) -> bool {
        if args.len() > u8::MAX as usize {
            return false;
        }
        if fmt.len() > u16::MAX as usize || file.len() > u16::MAX as usize {
            return false;
        }

        let mut args_bytes = 0usize;
        for arg in args {
            args_bytes += arg.encoded_size();
        }
        let total_size =
            HEADER_SIZE + FORMAT_SECTION_SIZE + SOURCE_SECTION_SIZE + 1 + args.len() + args_bytes;
        if total_size > MAX_RECORD_SIZE {
            return false;
        }

        let n_args = args.len() as u8;
        scratch.clear();
        scratch.reserve(total_size);
        assemble(
            scratch.as_mut_ptr(),
            level,
            timestamp,
            fmt,
            file,
            line,
            n_args,
            total_size,
            |buf| {
                let mut pos = 0usize;
                for arg in args {
                    buf[pos] = arg.type_tag();
                    pos += 1;
                }
                for arg in args {
                    let s = arg.encoded_size();
                    arg.encode(&mut buf[pos..pos + s]);
                    pos += s;
                }
            },
        );
        // SAFETY: `assemble` wrote exactly `total_size` bytes into the
        // Vec's allocation (reserved above); the region is initialized
        // and the Vec's capacity is sufficient.
        unsafe { scratch.set_len(total_size) };
        true
    }

    // Reads a little-endian u16 at `offset`.
    fn read_u16(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
    }

    // Reads a little-endian u32 at `offset`.
    fn read_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    // Reads a little-endian u64 at `offset`.
    fn read_u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
    }

    #[test]
    fn header_fields_are_written() {
        let mut buf = Vec::new();
        let ok = check_assemble(&mut buf, Level::Warn, 0xABCD, "hi", "f.rs", 7, &[]);
        assert!(ok);

        assert_eq!(buf[0], VERSION);
        assert_eq!(buf[1], LOG_RECORD);
        assert_eq!(read_u16(&buf, 2) as usize, buf.len());
        assert_eq!(buf[4], Level::Warn.to_u8());
        assert_eq!(read_u16(&buf, 5), FLAG_FORMAT | FLAG_SOURCE);
        assert_eq!(buf[7], 0);
        assert_eq!(read_u64(&buf, 8), 0xABCD);
    }

    #[test]
    fn format_and_source_sections_reference_the_static_strs() {
        let fmt = "value {}";
        let file = "src/x.rs";
        let mut buf = Vec::new();
        check_assemble(&mut buf, Level::Info, 0, fmt, file, 42, &[&1u64]);

        // Format section starts right after the header.
        let fmt_ptr = read_u64(&buf, HEADER_SIZE);
        let fmt_len = read_u16(&buf, HEADER_SIZE + 8);
        assert_eq!(fmt_ptr, fmt.as_ptr() as u64);
        assert_eq!(fmt_len as usize, fmt.len());

        // Source section follows the 10-byte format section.
        let src = HEADER_SIZE + FORMAT_SECTION_SIZE;
        assert_eq!(read_u64(&buf, src), file.as_ptr() as u64);
        assert_eq!(read_u16(&buf, src + 8) as usize, file.len());
        assert_eq!(read_u32(&buf, src + 10), 42);
    }

    #[test]
    fn arguments_are_tag_grouped_then_payload_grouped() {
        let mut buf = Vec::new();
        // Two args: u16 (tag 0x06, 2 bytes) then bool (tag 0x0A, 1 byte).
        check_assemble(
            &mut buf,
            Level::Info,
            0,
            "{} {}",
            "f",
            1,
            &[&0x1234u16, &true],
        );

        let args_at = HEADER_SIZE + FORMAT_SECTION_SIZE + SOURCE_SECTION_SIZE;
        assert_eq!(buf[args_at], 2); // count
        assert_eq!(buf[args_at + 1], 0x06); // u16 tag
        assert_eq!(buf[args_at + 2], 0x0A); // bool tag
        // Payloads follow the two tags.
        assert_eq!(read_u16(&buf, args_at + 3), 0x1234);
        assert_eq!(buf[args_at + 5], 1); // bool true
    }

    #[test]
    fn total_size_matches_buffer_length() {
        let mut buf = Vec::new();
        check_assemble(&mut buf, Level::Error, 0, "{}", "f", 1, &[&"hello"]);
        assert_eq!(read_u16(&buf, 2) as usize, buf.len());
    }

    #[test]
    fn rejects_more_than_255_arguments() {
        let args: Vec<&dyn Loggable> = (0..256).map(|_| &1u8 as &dyn Loggable).collect();
        let mut buf = Vec::new();
        assert!(!check_assemble(
            &mut buf,
            Level::Info,
            0,
            "x",
            "f",
            1,
            &args
        ));
    }

    #[test]
    fn rejects_record_larger_than_u16_total_size() {
        // A string argument just large enough to push total_size past u16::MAX.
        let big = "x".repeat(u16::MAX as usize);
        let mut buf = Vec::new();
        assert!(!check_assemble(
            &mut buf,
            Level::Info,
            0,
            "{}",
            "f",
            1,
            &[&big.as_str()]
        ));
    }

    #[test]
    fn zero_args_writes_count_zero() {
        let mut buf = Vec::new();
        check_assemble(&mut buf, Level::Info, 0, "static", "f", 1, &[]);
        let args_at = HEADER_SIZE + FORMAT_SECTION_SIZE + SOURCE_SECTION_SIZE;
        assert_eq!(buf[args_at], 0);
        assert_eq!(buf.len(), args_at + 1);
    }
}
