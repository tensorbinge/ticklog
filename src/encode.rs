//! Type-tagged binary encoding of primitive and string values.
//!
//! Every value is encoded as a one-byte type tag (0x00..=0x0B) followed by its
//! little-endian payload. Primitives are fixed-size; strings are length-prefixed
//! with a u16 LE length.

use core::mem::size_of;

/// Bytes a string's length prefix occupies: a u16 LE count before the UTF-8.
const STR_LEN_PREFIX: usize = size_of::<u16>();

/// Type tag for `u64`.
pub(crate) const TAG_U64: u8 = 0x00;
/// Type tag for `i64`.
pub(crate) const TAG_I64: u8 = 0x01;
/// Type tag for `f64`.
pub(crate) const TAG_F64: u8 = 0x02;
/// Type tag for `u32`.
pub(crate) const TAG_U32: u8 = 0x03;
/// Type tag for `i32`.
pub(crate) const TAG_I32: u8 = 0x04;
/// Type tag for `f32`.
pub(crate) const TAG_F32: u8 = 0x05;
/// Type tag for `u16`.
pub(crate) const TAG_U16: u8 = 0x06;
/// Type tag for `i16`.
pub(crate) const TAG_I16: u8 = 0x07;
/// Type tag for `u8`.
pub(crate) const TAG_U8: u8 = 0x08;
/// Type tag for `i8`.
pub(crate) const TAG_I8: u8 = 0x09;
/// Type tag for `bool`.
pub(crate) const TAG_BOOL: u8 = 0x0A;
/// Type tag for `&str`, `String`, and `&String`.
pub(crate) const TAG_STR: u8 = 0x0B;

/// Number of distinct type tags (`0x00..=0x0B`). Every per-tag table
/// ([`FIXED_SIZES`] here and `FORMATTERS` in the drain) is sized to this
/// constant, so adding a tag extends them together rather than silently
/// desyncing their lengths.
pub(crate) const TAG_COUNT: usize = 12;

/// Fixed encoded payload size for each type tag.
///
/// Indexed by tag value (0x00..=0x0B). Each entry is the number of bytes the
/// payload occupies after the tag byte. Strings (0x0B) return 0; their size
/// comes from a u16 length prefix. One row per tag: a new tag needs a row here
/// plus a `TAG_*` const, a `Loggable` impl, and a `FORMATTERS` row in the drain.
pub(crate) static FIXED_SIZES: [usize; TAG_COUNT] = [
    8, // 0x00: u64
    8, // 0x01: i64
    8, // 0x02: f64
    4, // 0x03: u32
    4, // 0x04: i32
    4, // 0x05: f32
    2, // 0x06: u16
    2, // 0x07: i16
    1, // 0x08: u8
    1, // 0x09: i8
    1, // 0x0A: bool
    0, // 0x0B: &str/String/&String (variable-length, length-prefixed)
];

/// A value that can be encoded as a self-delimiting byte sequence with a type tag.
///
/// Primitives encode as their little-endian byte representation. Strings encode
/// as `[len: u16 LE][utf8 bytes]`.
///
/// Crate-internal: the macros reach it only through the `pub` [`LoggableArgs`],
/// which never names this trait. Kept as a trait so complex/user types can be
/// supported later behind a sealed public API.
pub(crate) trait Loggable: Send {
    /// Number of bytes this value occupies when encoded.
    fn encoded_size(&self) -> usize;

    /// Writes the encoded representation of this value into `buf`.
    ///
    /// `buf` is guaranteed to be at least `encoded_size()` bytes long.
    fn encode(&self, buf: &mut [u8]);

    /// Type tag byte for this value (0x00..=0x0B).
    fn type_tag(&self) -> u8;
}

mod sealed {
    /// Sealing supertrait: implemented only for the argument cons-list types in
    /// this module, so downstream crates cannot implement [`super::LoggableArgs`].
    pub trait Sealed {}
    impl Sealed for () {}
    impl<H: super::Loggable, T: super::LoggableArgs> Sealed for (&H, T) {}
}

/// A cons-list of [`Loggable`] references: `()` is the empty list and `(&H, T)`
/// prepends one argument to the tail `T`. A whole record's arguments form one
/// such list, sized and encoded by walking it in order. Each length is its own
/// type, so the list is variadic with no argument-count ceiling.
///
/// Public only so the logging macros can name it through `$crate::__private`; it
/// is `#[doc(hidden)]` and sealed (see [`sealed::Sealed`]), so it is not part of
/// the stable API and carries no semver guarantee.
#[doc(hidden)]
pub trait LoggableArgs: sealed::Sealed {
    /// Total encoded payload size of the arguments (the sum of each
    /// [`Loggable::encoded_size`]).
    fn args_encoded_size(&self) -> usize;

    /// Writes each argument's type tag at `buf[*tag]` and encoded payload at
    /// `buf[*pay]`, advancing both cursors. Tags occupy `buf[0..n_args]` and
    /// payloads follow.
    fn write_args(&self, buf: &mut [u8], tag: &mut usize, pay: &mut usize);
}

impl LoggableArgs for () {
    #[inline(always)]
    fn args_encoded_size(&self) -> usize {
        0
    }

    #[inline(always)]
    fn write_args(&self, _buf: &mut [u8], _tag: &mut usize, _pay: &mut usize) {}
}

impl<H: Loggable, T: LoggableArgs> LoggableArgs for (&H, T) {
    #[inline(always)]
    fn args_encoded_size(&self) -> usize {
        self.0.encoded_size().wrapping_add(self.1.args_encoded_size())
    }

    #[inline(always)]
    fn write_args(&self, buf: &mut [u8], tag: &mut usize, pay: &mut usize) {
        buf[*tag] = self.0.type_tag();
        *tag += 1;
        let s = self.0.encoded_size();
        self.0.encode(&mut buf[*pay..*pay + s]);
        *pay += s;
        self.1.write_args(buf, tag, pay);
    }
}

/// Generates `Loggable` impls for a primitive numeric type and its reference.
///
/// Produces both `impl Loggable for $t` and `impl Loggable for &$t`. The
/// reference impl delegates to the value impl by dereference.
macro_rules! impl_loggable_primitive {
    ($t:ty, $tag:ident, $size:literal) => {
        impl $crate::encode::Loggable for $t {
            #[inline(always)]
            fn encoded_size(&self) -> usize {
                $size
            }

            #[inline(always)]
            fn encode(&self, buf: &mut [u8]) {
                buf[..$size].copy_from_slice(&self.to_le_bytes());
            }

            #[inline(always)]
            fn type_tag(&self) -> u8 {
                $crate::encode::$tag
            }
        }

        impl $crate::encode::Loggable for &$t {
            #[inline(always)]
            fn encoded_size(&self) -> usize {
                $size
            }

            #[inline(always)]
            fn encode(&self, buf: &mut [u8]) {
                buf[..$size].copy_from_slice(&(*self).to_le_bytes());
            }

            #[inline(always)]
            fn type_tag(&self) -> u8 {
                $crate::encode::$tag
            }
        }
    };
}

impl_loggable_primitive!(u64, TAG_U64, 8);
impl_loggable_primitive!(i64, TAG_I64, 8);
impl_loggable_primitive!(f64, TAG_F64, 8);
impl_loggable_primitive!(u32, TAG_U32, 4);
impl_loggable_primitive!(i32, TAG_I32, 4);
impl_loggable_primitive!(f32, TAG_F32, 4);
impl_loggable_primitive!(u16, TAG_U16, 2);
impl_loggable_primitive!(i16, TAG_I16, 2);
impl_loggable_primitive!(u8, TAG_U8, 1);
impl_loggable_primitive!(i8, TAG_I8, 1);

// bool impls: manual because `bool` does not implement `to_le_bytes`.

impl Loggable for bool {
    #[inline(always)]
    fn encoded_size(&self) -> usize {
        1
    }

    #[inline(always)]
    fn encode(&self, buf: &mut [u8]) {
        buf[0] = *self as u8;
    }

    #[inline(always)]
    fn type_tag(&self) -> u8 {
        TAG_BOOL
    }
}

impl Loggable for &bool {
    #[inline(always)]
    fn encoded_size(&self) -> usize {
        1
    }

    #[inline(always)]
    fn encode(&self, buf: &mut [u8]) {
        buf[0] = **self as u8;
    }

    #[inline(always)]
    fn type_tag(&self) -> u8 {
        TAG_BOOL
    }
}

// String impls: all share tag 0x0B. Encoding: [len: u16 LE][utf8 bytes].

impl Loggable for &str {
    #[inline]
    fn encoded_size(&self) -> usize {
        STR_LEN_PREFIX + self.len()
    }

    #[inline]
    fn encode(&self, buf: &mut [u8]) {
        let len = (self.len() as u16).to_le_bytes();
        buf[..len.len()].copy_from_slice(&len);
        buf[len.len()..][..self.len()].copy_from_slice(self.as_bytes());
    }

    #[inline(always)]
    fn type_tag(&self) -> u8 {
        TAG_STR
    }
}

impl Loggable for String {
    #[inline]
    fn encoded_size(&self) -> usize {
        STR_LEN_PREFIX + self.len()
    }

    #[inline]
    fn encode(&self, buf: &mut [u8]) {
        self.as_str().encode(buf)
    }

    #[inline(always)]
    fn type_tag(&self) -> u8 {
        TAG_STR
    }
}

impl Loggable for &String {
    #[inline]
    fn encoded_size(&self) -> usize {
        STR_LEN_PREFIX + self.len()
    }

    #[inline]
    fn encode(&self, buf: &mut [u8]) {
        self.as_str().encode(buf)
    }

    #[inline(always)]
    fn type_tag(&self) -> u8 {
        TAG_STR
    }
}

#[cfg(test)]
mod tests {
    // Several tests below deliberately spell out `&value` receivers to document
    // that a borrowed loggable tags identically to its owned form; the explicit
    // borrow is intentional here, not a needless one.
    #![allow(clippy::needless_borrow)]

    use super::*;

    // Helper: encode a value and return the filled portion of the buffer.
    fn encode_to_vec(v: &impl Loggable) -> Vec<u8> {
        let size = v.encoded_size();
        let mut buf = vec![0u8; size];
        v.encode(&mut buf);
        buf
    }

    #[test]
    fn tag_u64_is_0x00() {
        assert_eq!(0u64.type_tag(), 0x00);
        assert_eq!((&42u64).type_tag(), 0x00);
    }

    #[test]
    fn tag_i64_is_0x01() {
        assert_eq!(0i64.type_tag(), 0x01);
        assert_eq!((&-1i64).type_tag(), 0x01);
    }

    #[test]
    fn tag_f64_is_0x02() {
        assert_eq!(0f64.type_tag(), 0x02);
        assert_eq!((&std::f64::consts::PI).type_tag(), 0x02);
    }

    #[test]
    fn tag_u32_is_0x03() {
        assert_eq!(0u32.type_tag(), 0x03);
    }

    #[test]
    fn tag_i32_is_0x04() {
        assert_eq!(0i32.type_tag(), 0x04);
    }

    #[test]
    fn tag_f32_is_0x05() {
        assert_eq!(0f32.type_tag(), 0x05);
    }

    #[test]
    fn tag_u16_is_0x06() {
        assert_eq!(0u16.type_tag(), 0x06);
    }

    #[test]
    fn tag_i16_is_0x07() {
        assert_eq!(0i16.type_tag(), 0x07);
    }

    #[test]
    fn tag_u8_is_0x08() {
        assert_eq!(0u8.type_tag(), 0x08);
    }

    #[test]
    fn tag_i8_is_0x09() {
        assert_eq!(0i8.type_tag(), 0x09);
    }

    #[test]
    fn tag_bool_is_0x0a() {
        assert_eq!(true.type_tag(), 0x0A);
        assert_eq!(false.type_tag(), 0x0A);
        assert_eq!((&true).type_tag(), 0x0A);
    }

    #[test]
    fn tag_str_is_0x0b() {
        assert_eq!("hello".type_tag(), 0x0B);
        assert_eq!(String::from("hello").type_tag(), 0x0B);
        assert_eq!((&String::from("hello")).type_tag(), 0x0B);
    }

    #[test]
    fn fixed_sizes_length_is_12() {
        assert_eq!(FIXED_SIZES.len(), 12);
    }

    #[test]
    fn fixed_sizes_u64_is_8() {
        assert_eq!(FIXED_SIZES[TAG_U64 as usize], 8);
    }

    #[test]
    fn fixed_sizes_i64_is_8() {
        assert_eq!(FIXED_SIZES[TAG_I64 as usize], 8);
    }

    #[test]
    fn fixed_sizes_f64_is_8() {
        assert_eq!(FIXED_SIZES[TAG_F64 as usize], 8);
    }

    #[test]
    fn fixed_sizes_u32_is_4() {
        assert_eq!(FIXED_SIZES[TAG_U32 as usize], 4);
    }

    #[test]
    fn fixed_sizes_i32_is_4() {
        assert_eq!(FIXED_SIZES[TAG_I32 as usize], 4);
    }

    #[test]
    fn fixed_sizes_f32_is_4() {
        assert_eq!(FIXED_SIZES[TAG_F32 as usize], 4);
    }

    #[test]
    fn fixed_sizes_u16_is_2() {
        assert_eq!(FIXED_SIZES[TAG_U16 as usize], 2);
    }

    #[test]
    fn fixed_sizes_i16_is_2() {
        assert_eq!(FIXED_SIZES[TAG_I16 as usize], 2);
    }

    #[test]
    fn fixed_sizes_u8_is_1() {
        assert_eq!(FIXED_SIZES[TAG_U8 as usize], 1);
    }

    #[test]
    fn fixed_sizes_i8_is_1() {
        assert_eq!(FIXED_SIZES[TAG_I8 as usize], 1);
    }

    #[test]
    fn fixed_sizes_bool_is_1() {
        assert_eq!(FIXED_SIZES[TAG_BOOL as usize], 1);
    }

    #[test]
    fn fixed_sizes_str_is_0() {
        assert_eq!(FIXED_SIZES[TAG_STR as usize], 0);
    }

    #[test]
    fn roundtrip_u64() {
        let val: u64 = 0xDEAD_BEEF_CAFE_BABE;
        let buf = encode_to_vec(&val);
        assert_eq!(buf.len(), 8);
        let decoded = u64::from_le_bytes(buf[..8].try_into().unwrap());
        assert_eq!(decoded, val);
    }

    #[test]
    fn roundtrip_u64_max() {
        let val = u64::MAX;
        let buf = encode_to_vec(&val);
        assert_eq!(u64::from_le_bytes(buf[..8].try_into().unwrap()), val);
    }

    #[test]
    fn roundtrip_u64_zero() {
        let val: u64 = 0;
        let buf = encode_to_vec(&val);
        assert_eq!(u64::from_le_bytes(buf[..8].try_into().unwrap()), val);
    }

    #[test]
    fn roundtrip_i64() {
        let val: i64 = -9_223_372_036_854_775_808;
        let buf = encode_to_vec(&val);
        assert_eq!(buf.len(), 8);
        let decoded = i64::from_le_bytes(buf[..8].try_into().unwrap());
        assert_eq!(decoded, val);
    }

    #[test]
    fn roundtrip_i64_negative() {
        let val: i64 = -1;
        let buf = encode_to_vec(&val);
        assert_eq!(i64::from_le_bytes(buf[..8].try_into().unwrap()), val);
    }

    #[test]
    fn roundtrip_f64() {
        let val = std::f64::consts::PI;
        let buf = encode_to_vec(&val);
        assert_eq!(buf.len(), 8);
        let decoded = f64::from_le_bytes(buf[..8].try_into().unwrap());
        assert_eq!(decoded, val);
    }

    #[test]
    fn roundtrip_f64_negative_zero() {
        let val = -0.0f64;
        let buf = encode_to_vec(&val);
        assert!(f64::from_le_bytes(buf[..8].try_into().unwrap()).is_sign_negative());
    }

    #[test]
    fn roundtrip_u32() {
        let val: u32 = 0xDEAD_BEEF;
        let buf = encode_to_vec(&val);
        assert_eq!(buf.len(), 4);
        let decoded = u32::from_le_bytes(buf[..4].try_into().unwrap());
        assert_eq!(decoded, val);
    }

    #[test]
    fn roundtrip_i32() {
        let val: i32 = -2_147_483_648;
        let buf = encode_to_vec(&val);
        assert_eq!(buf.len(), 4);
        let decoded = i32::from_le_bytes(buf[..4].try_into().unwrap());
        assert_eq!(decoded, val);
    }

    #[test]
    fn roundtrip_f32() {
        let val = std::f32::consts::PI;
        let buf = encode_to_vec(&val);
        assert_eq!(buf.len(), 4);
        let decoded = f32::from_le_bytes(buf[..4].try_into().unwrap());
        assert_eq!(decoded, val);
    }

    #[test]
    fn roundtrip_u16() {
        let val: u16 = 0xBEEF;
        let buf = encode_to_vec(&val);
        assert_eq!(buf.len(), 2);
        let decoded = u16::from_le_bytes(buf[..2].try_into().unwrap());
        assert_eq!(decoded, val);
    }

    #[test]
    fn roundtrip_i16() {
        let val: i16 = -32_768;
        let buf = encode_to_vec(&val);
        assert_eq!(buf.len(), 2);
        let decoded = i16::from_le_bytes(buf[..2].try_into().unwrap());
        assert_eq!(decoded, val);
    }

    #[test]
    fn roundtrip_u8() {
        let val: u8 = 0xAB;
        let buf = encode_to_vec(&val);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0], val);
    }

    #[test]
    fn roundtrip_i8() {
        let val: i8 = -128;
        let buf = encode_to_vec(&val);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0] as i8, val);
    }

    #[test]
    fn roundtrip_bool_true() {
        let buf = encode_to_vec(&true);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0], 1);
    }

    #[test]
    fn roundtrip_bool_false() {
        let buf = encode_to_vec(&false);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0], 0);
    }

    #[test]
    fn roundtrip_ref_u64() {
        let val: u64 = 42;
        let buf = encode_to_vec(&(&val));
        assert_eq!(u64::from_le_bytes(buf[..8].try_into().unwrap()), 42);
    }

    #[test]
    fn roundtrip_ref_bool() {
        let val = true;
        let buf = encode_to_vec(&(&val));
        assert_eq!(buf[0], 1);
    }

    #[test]
    fn roundtrip_str_empty() {
        let buf = encode_to_vec(&"");
        // 2 bytes for len prefix, 0 bytes for data.
        assert_eq!(buf.len(), 2);
        let len = u16::from_le_bytes(buf[..2].try_into().unwrap());
        assert_eq!(len, 0);
    }

    #[test]
    fn roundtrip_str_hello() {
        let val = "Hello, world!";
        let buf = encode_to_vec(&val);
        assert_eq!(buf.len(), 2 + val.len());
        let len = u16::from_le_bytes(buf[..2].try_into().unwrap());
        assert_eq!(len as usize, val.len());
        let decoded = std::str::from_utf8(&buf[2..]).unwrap();
        assert_eq!(decoded, val);
    }

    #[test]
    fn roundtrip_str_unicode() {
        let val = "こんにちは世界"; // 7 Japanese characters, 21 UTF-8 bytes
        let buf = encode_to_vec(&val);
        assert_eq!(buf.len(), 2 + 21);
        let len = u16::from_le_bytes(buf[..2].try_into().unwrap());
        assert_eq!(len as usize, 21);
        let decoded = std::str::from_utf8(&buf[2..]).unwrap();
        assert_eq!(decoded, val);
    }

    #[test]
    fn roundtrip_string_owned() {
        let val = String::from("owned string");
        let buf = encode_to_vec(&val);
        let len = u16::from_le_bytes(buf[..2].try_into().unwrap());
        assert_eq!(len as usize, val.len());
        assert_eq!(&buf[2..], val.as_bytes());
    }

    #[test]
    fn roundtrip_ref_string() {
        let val = String::from("via ref");
        let buf = encode_to_vec(&&val);
        let len = u16::from_le_bytes(buf[..2].try_into().unwrap());
        assert_eq!(len as usize, val.len());
        assert_eq!(&buf[2..], val.as_bytes());
    }

    #[test]
    fn str_tag_is_consistent_across_variants() {
        let s: &str = "hello";
        let owned = String::from("hello");
        assert_eq!(s.type_tag(), owned.type_tag());
        assert_eq!(s.type_tag(), (&owned).type_tag());
    }

    #[test]
    fn str_encoded_size_matches() {
        assert_eq!("".encoded_size(), 2);
        assert_eq!("hi".encoded_size(), 4);
        let s = String::from("hello");
        assert_eq!(s.encoded_size(), "hello".encoded_size());
    }

    #[test]
    fn u64_is_little_endian() {
        let val: u64 = 0x0102_0304_0506_0708;
        let buf = encode_to_vec(&val);
        assert_eq!(buf[0], 0x08); // LSB first
        assert_eq!(buf[7], 0x01); // MSB last
    }

    #[test]
    fn u32_is_little_endian() {
        let val: u32 = 0x0102_0304;
        let buf = encode_to_vec(&val);
        assert_eq!(buf[0], 0x04);
        assert_eq!(buf[3], 0x01);
    }

    #[test]
    fn u16_len_prefix_is_little_endian() {
        let val = "ABC"; // 3 bytes
        let buf = encode_to_vec(&val);
        assert_eq!(buf[0], 0x03); // LSB of 3
        assert_eq!(buf[1], 0x00); // MSB of 3
    }

    #[test]
    fn str_max_len_u16() {
        // A string with u16::MAX bytes should encode correctly.
        let val = "x".repeat(u16::MAX as usize);
        let buf = encode_to_vec(&val.as_str());
        assert_eq!(buf.len(), 2 + u16::MAX as usize);
        let len = u16::from_le_bytes(buf[..2].try_into().unwrap());
        assert_eq!(len, u16::MAX);
    }

    #[test]
    fn loggable_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<&u64>();
        assert_send::<&str>();
        assert_send::<String>();
    }

    #[test]
    fn encoded_size_matches_actual_for_primitives() {
        // Each closure returns true if encoded_size() matches the actual
        // number of bytes written by encode().
        let checks: &[&dyn Fn() -> bool] = &[
            &|| {
                let v: u64 = 42;
                v.encoded_size() == encode_to_vec(&v).len()
            },
            &|| {
                let v: i64 = -1;
                v.encoded_size() == encode_to_vec(&v).len()
            },
            &|| {
                let v: f64 = 1.0;
                v.encoded_size() == encode_to_vec(&v).len()
            },
            &|| {
                let v: u32 = 42;
                v.encoded_size() == encode_to_vec(&v).len()
            },
            &|| {
                let v: i32 = -1;
                v.encoded_size() == encode_to_vec(&v).len()
            },
            &|| {
                let v: f32 = 1.0;
                v.encoded_size() == encode_to_vec(&v).len()
            },
            &|| {
                let v: u16 = 42;
                v.encoded_size() == encode_to_vec(&v).len()
            },
            &|| {
                let v: i16 = -1;
                v.encoded_size() == encode_to_vec(&v).len()
            },
            &|| {
                let v: u8 = 42;
                v.encoded_size() == encode_to_vec(&v).len()
            },
            &|| {
                let v: i8 = -1;
                v.encoded_size() == encode_to_vec(&v).len()
            },
            &|| {
                let v = true;
                v.encoded_size() == encode_to_vec(&v).len()
            },
        ];
        for check in checks {
            assert!(check());
        }
    }

    #[test]
    fn encoded_size_matches_buf_write_len() {
        let v: u64 = 0xABCD;
        assert_eq!(v.encoded_size(), encode_to_vec(&v).len());

        let s = "hello world";
        assert_eq!(s.encoded_size(), encode_to_vec(&s).len());
    }
}
