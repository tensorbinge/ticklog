//! Format spec parsing and type-driven value formatting.
//!
//! The format spec grammar:
//!
//! ```text
//! spec  := [name ':'] [fill] [align] ['0'] [width] ['.' precision] ['#'] [type]
//! fill  := any ASCII char except '+' | '-' | '0' | '#'
//! align := '<' | '>' | '^'
//! width := decimal digit+
//! precision := decimal digit+
//! type  := '?' | 'x' | 'X' | 'o' | 'b' | 'e' | 'E'
//! ```

/// Horizontal alignment for width-padded values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Align {
    /// No alignment specified. Behaves like `Left` for width-padding.
    #[default]
    None,
    /// Align left (pad on the right).
    Left,
    /// Align right (pad on the left).
    Right,
    /// Align center (pad evenly on both sides).
    Center,
}

/// Parsed format spec for a single placeholder.
///
/// The default value (all fields at their defaults) represents `{}`: Display
/// formatting with no width, alignment, or precision.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FormatSpec {
    /// Fill character inserted for width padding. Default `' '`.
    pub fill: u8,
    /// Horizontal alignment. Default `Align::None`.
    pub align: Align,
    /// `true` when the `0` flag is present. Forces fill to `'0'` and align
    /// to `Right` unless an explicit alignment was also given.
    pub zero_fill: bool,
    /// Minimum field width in characters. `None` means no padding.
    pub width: Option<u16>,
    /// Precision. The meaning depends on the type: minimum digits for integers,
    /// decimal places for floats, maximum characters for strings.
    pub precision: Option<u16>,
    /// `true` when the `#` (alternate) flag is present. Adds `0x`/`0o`/`0b`
    /// prefix for hex/octal/binary integers.
    pub alternate: bool,
    /// The type specifier character. `None` means Display (`{}`).
    pub type_char: Option<u8>,
}

impl Default for FormatSpec {
    fn default() -> Self {
        Self {
            fill: b' ',
            align: Align::None,
            zero_fill: false,
            width: None,
            precision: None,
            alternate: false,
            type_char: None,
        }
    }
}

/// Syntactically validates a format string and returns the number of `{}`
/// placeholders, excluding `{{` / `}}` escapes.
///
/// Rejects: unclosed `{`, unmatched `}`, sign flags (`+`, `-`),
/// positional params (`{0}`, `{1:$}`), unsupported type chars, and trailing
/// garbage after the type char.
pub(crate) const fn validate_fmt(fmt: &str) -> Result<usize, &'static str> {
    let bytes = fmt.as_bytes();
    let mut i = 0;
    let mut count = 0;

    while i < bytes.len() {
        if bytes[i] == b'{' {
            // Escaped brace: {{
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                i += 2;
                continue;
            }
            i += 1; // skip '{'

            // Save the start of the name to detect positional params.
            let name_start = i;

            // Scan for ':' or '}'. Everything before the first ':' is a
            // cosmetic name and is ignored.
            while i < bytes.len() && bytes[i] != b':' && bytes[i] != b'}' {
                i += 1;
            }

            // Reject positional params: the name (before ':' or '}') may not
            // consist solely of decimal digits.
            if name_start < i && is_all_digits(bytes, name_start, i) {
                return Err("positional params not supported");
            }

            if i >= bytes.len() {
                return Err("unclosed '{': missing '}'");
            }

            // ':' found: parse the format spec after it.
            if bytes[i] == b':' {
                i += 1; // skip ':'
                i = match parse_spec_const(bytes, i) {
                    Ok(pos) => pos,
                    Err(e) => return Err(e),
                };
            }

            // Expect the closing '}'.
            if i >= bytes.len() {
                return Err("unclosed '{': missing '}'");
            }
            if bytes[i] != b'}' {
                return Err("trailing characters after format spec");
            }
            i += 1; // skip '}'
            count += 1;
        } else if bytes[i] == b'}' {
            // Escaped brace: }}
            if i + 1 < bytes.len() && bytes[i + 1] == b'}' {
                i += 2;
                continue;
            }
            return Err("unmatched '}': use '}}' to escape a literal brace");
        } else {
            // Literal text outside braces.
            i += 1;
        }
    }

    Ok(count)
}

/// Validates a format string and its argument count at compile time.
///
/// The logging macros call this in a `const` context, so a malformed format
/// string or a mismatch between the number of `{}` placeholders and the number
/// of arguments aborts compilation with a message pointing at the macro call
/// site. Syntax is checked internally; per-type specifier validity is
/// resolved at runtime by the drain.
pub const fn check_fmt(fmt: &str, n_args: usize) {
    match validate_fmt(fmt) {
        Ok(placeholders) => {
            if placeholders != n_args {
                panic!(
                    "ticklog: the number of arguments does not match the number of \
                     placeholders in the format string"
                );
            }
        }
        Err(msg) => panic!("{}", msg),
    }
}

/// Parse the body of a format spec (after `:`), advancing `i` past the parsed
/// components. Returns the new position or an error string.
const fn parse_spec_const(bytes: &[u8], mut i: usize) -> Result<usize, &'static str> {
    // Guard against empty spec body.
    if i >= bytes.len() {
        return Err("unclosed '{': missing '}'");
    }
    if bytes[i] == b'}' {
        return Ok(i); // empty spec; nothing to parse
    }

    // [fill] [align]: need 2-char lookahead for fill+align.
    if i + 1 < bytes.len() && is_align_char(bytes[i + 1]) {
        // Second char is an alignment marker; first is the fill character.
        if !is_valid_fill(bytes[i]) {
            return Err("invalid fill character");
        }
        i += 2;
    } else if is_align_char(bytes[i]) {
        i += 1; // alignment without explicit fill
    }

    if i < bytes.len() && bytes[i] == b'}' {
        return Ok(i);
    }

    // ['0']: zero-fill flag.
    if i < bytes.len() && bytes[i] == b'0' {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'}' {
        return Ok(i);
    }

    // [width]: decimal digit sequence.
    if i < bytes.len() && bytes[i] >= b'0' && bytes[i] <= b'9' {
        while i < bytes.len() && bytes[i] >= b'0' && bytes[i] <= b'9' {
            i += 1;
        }
    }
    if i < bytes.len() && bytes[i] == b'}' {
        return Ok(i);
    }

    // ['.' precision]: dot followed by decimal digits.
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        if i >= bytes.len() || bytes[i] < b'0' || bytes[i] > b'9' {
            return Err("expected digits after '.'");
        }
        while i < bytes.len() && bytes[i] >= b'0' && bytes[i] <= b'9' {
            i += 1;
        }
    }
    if i < bytes.len() && bytes[i] == b'}' {
        return Ok(i);
    }

    // ['#']: alternate flag.
    if i < bytes.len() && bytes[i] == b'#' {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'}' {
        return Ok(i);
    }

    // [type]: single type character.
    if i < bytes.len() {
        match bytes[i] {
            b'?' | b'x' | b'X' | b'o' | b'b' | b'e' | b'E' => {
                i += 1;
            }
            b'}' => {
                // No type char; valid.
            }
            b'+' | b'-' => {
                return Err("sign flags not supported");
            }
            _ => {
                return Err("unsupported type character in format spec");
            }
        }
    }

    Ok(i)
}

/// Returns `true` if every byte in `bytes[start..end]` is an ASCII decimal
/// digit.
const fn is_all_digits(bytes: &[u8], start: usize, end: usize) -> bool {
    let mut j = start;
    while j < end {
        if bytes[j] < b'0' || bytes[j] > b'9' {
            return false;
        }
        j += 1;
    }
    true
}

/// Returns `true` if `c` is a valid alignment character.
const fn is_align_char(c: u8) -> bool {
    c == b'<' || c == b'>' || c == b'^'
}

/// Returns `true` if `c` is a valid fill character.
/// Fill may be any ASCII character except `+`, `-`, `0`, and `#`.
const fn is_valid_fill(c: u8) -> bool {
    c != b'+' && c != b'-' && c != b'0' && c != b'#'
}

/// Parses the content between `{` and `}` into a [`FormatSpec`].
///
/// Assumes well-formed input.
pub(crate) fn parse_spec(spec_content: &str) -> FormatSpec {
    let bytes = spec_content.as_bytes();
    let mut pos = 0;

    // Skip optional cosmetic name before ':'.
    while pos < bytes.len() && bytes[pos] != b':' {
        pos += 1;
    }
    if pos < bytes.len() && bytes[pos] == b':' {
        pos += 1; // skip ':'
    } else {
        // No ':': the entire content is a cosmetic name; empty spec.
        return FormatSpec::default();
    }

    parse_spec_body(&bytes[pos..])
}

/// Parse the format spec after the optional name and colon.
fn parse_spec_body(spec: &[u8]) -> FormatSpec {
    let mut fs = FormatSpec::default();
    let len = spec.len();
    let mut i = 0;

    if i >= len {
        return fs;
    }

    // [fill] [align]
    if i + 1 < len && is_align_char(spec[i + 1]) {
        fs.fill = spec[i];
        fs.align = match spec[i + 1] {
            b'<' => Align::Left,
            b'>' => Align::Right,
            b'^' => Align::Center,
            _ => Align::None,
        };
        i += 2;
    } else if is_align_char(spec[i]) {
        fs.align = match spec[i] {
            b'<' => Align::Left,
            b'>' => Align::Right,
            b'^' => Align::Center,
            _ => Align::None,
        };
        i += 1;
    }

    // ['0']
    if i < len && spec[i] == b'0' {
        fs.zero_fill = true;
        i += 1;
    }

    // [width]
    if i < len && spec[i] >= b'0' && spec[i] <= b'9' {
        let mut w: u16 = 0;
        while i < len && spec[i] >= b'0' && spec[i] <= b'9' {
            w = w.saturating_mul(10).saturating_add((spec[i] - b'0') as u16);
            i += 1;
        }
        fs.width = Some(w);
    }

    // ['.' precision]
    if i < len && spec[i] == b'.' {
        i += 1;
        let mut p: u16 = 0;
        while i < len && spec[i] >= b'0' && spec[i] <= b'9' {
            p = p.saturating_mul(10).saturating_add((spec[i] - b'0') as u16);
            i += 1;
        }
        fs.precision = Some(p);
    }

    // ['#']
    if i < len && spec[i] == b'#' {
        fs.alternate = true;
        i += 1;
    }

    // [type]
    if i < len {
        match spec[i] {
            b'?' | b'x' | b'X' | b'o' | b'b' | b'e' | b'E' => {
                fs.type_char = Some(spec[i]);
            }
            _ => {}
        }
    }

    fs
}

/// Applies width and alignment padding to an already-formatted value string
/// and pushes the result into `buf`.
///
/// If the formatted value is shorter than `spec.width`, it is padded with
/// `spec.fill` according to `spec.align`. The `0` flag overrides fill to `'0'`
/// and default alignment to right.
pub(crate) fn format_with_spec(value_str: &str, spec: &FormatSpec, buf: &mut Vec<u8>) {
    let width = spec.width.unwrap_or(0) as usize;
    let fill = if spec.zero_fill { b'0' } else { spec.fill };
    let align = if spec.zero_fill && spec.align == Align::None {
        Align::Right
    } else {
        spec.align
    };

    let val_bytes = value_str.as_bytes();
    if val_bytes.len() >= width {
        buf.extend_from_slice(val_bytes);
        return;
    }

    let padding = width - val_bytes.len();
    match align {
        Align::None | Align::Left => {
            buf.extend_from_slice(val_bytes);
            for _ in 0..padding {
                buf.push(fill);
            }
        }
        Align::Right => {
            for _ in 0..padding {
                buf.push(fill);
            }
            buf.extend_from_slice(val_bytes);
        }
        Align::Center => {
            let left = padding / 2;
            for _ in 0..left {
                buf.push(fill);
            }
            buf.extend_from_slice(val_bytes);
            for _ in 0..(padding - left) {
                buf.push(fill);
            }
        }
    }
}

/// Formats a `u64` value according to `spec` and appends it to `buf`.
///
/// Precision zero-pads the digits; width and alignment are applied via
/// [`format_with_spec`].
pub(crate) fn format_u64(value: u64, spec: &FormatSpec, buf: &mut Vec<u8>) {
    format_unsigned_impl(value, spec, buf);
}

/// Formats an `i64` value according to `spec` and appends it to `buf`.
pub(crate) fn format_i64(value: i64, spec: &FormatSpec, buf: &mut Vec<u8>) {
    format_signed_impl(value, spec, buf);
}

/// Formats a `f64` value according to `spec` and appends it to `buf`.
pub(crate) fn format_f64(value: f64, spec: &FormatSpec, buf: &mut Vec<u8>) {
    format_float_impl(value, spec, buf);
}

/// Formats a `u32` value according to `spec` and appends it to `buf`.
#[inline]
pub(crate) fn format_u32(value: u32, spec: &FormatSpec, buf: &mut Vec<u8>) {
    format_unsigned_impl(value as u64, spec, buf);
}

/// Formats an `i32` value according to `spec` and appends it to `buf`.
#[inline]
pub(crate) fn format_i32(value: i32, spec: &FormatSpec, buf: &mut Vec<u8>) {
    format_signed_impl(value as i64, spec, buf);
}

/// Formats an `f32` value according to `spec` and appends it to `buf`.
#[inline]
pub(crate) fn format_f32(value: f32, spec: &FormatSpec, buf: &mut Vec<u8>) {
    format_float_impl(value as f64, spec, buf);
}

/// Formats a `u16` value according to `spec` and appends it to `buf`.
#[inline]
pub(crate) fn format_u16(value: u16, spec: &FormatSpec, buf: &mut Vec<u8>) {
    format_unsigned_impl(value as u64, spec, buf);
}

/// Formats an `i16` value according to `spec` and appends it to `buf`.
#[inline]
pub(crate) fn format_i16(value: i16, spec: &FormatSpec, buf: &mut Vec<u8>) {
    format_signed_impl(value as i64, spec, buf);
}

/// Formats a `u8` value according to `spec` and appends it to `buf`.
#[inline]
pub(crate) fn format_u8(value: u8, spec: &FormatSpec, buf: &mut Vec<u8>) {
    format_unsigned_impl(value as u64, spec, buf);
}

/// Formats an `i8` value according to `spec` and appends it to `buf`.
#[inline]
pub(crate) fn format_i8(value: i8, spec: &FormatSpec, buf: &mut Vec<u8>) {
    format_signed_impl(value as i64, spec, buf);
}

/// Formats a `bool` value according to `spec` and appends it to `buf`.
pub(crate) fn format_bool(value: bool, spec: &FormatSpec, buf: &mut Vec<u8>) {
    let s = match spec.type_char {
        Some(b'?') => format!("{:?}", value),
        _ => {
            if value {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
    };
    format_with_spec(&s, spec, buf);
}

/// Formats a `&str` value according to `spec` and appends it to `buf`.
pub(crate) fn format_str(value: &str, spec: &FormatSpec, buf: &mut Vec<u8>) {
    let s = match spec.type_char {
        Some(b'?') if spec.alternate => format!("{:#?}", value),
        Some(b'?') => format!("{:?}", value),
        _ => {
            if let Some(p) = spec.precision {
                // Truncate to `p` characters (bytes, for ASCII; chars for
                // proper UTF-8; the spec matrix limits precision to &str).
                let end = value
                    .char_indices()
                    .take(p as usize)
                    .last()
                    .map(|(i, c)| i + c.len_utf8())
                    .unwrap_or(0);
                value[..end].to_string()
            } else {
                value.to_string()
            }
        }
    };
    format_with_spec(&s, spec, buf);
}

fn format_unsigned_impl(value: u64, spec: &FormatSpec, buf: &mut Vec<u8>) {
    let mut s = match spec.type_char {
        Some(b'?') => format!("{:?}", value),
        Some(b'x') => {
            if spec.alternate {
                format!("{:#x}", value)
            } else {
                format!("{:x}", value)
            }
        }
        Some(b'X') => {
            if spec.alternate {
                format!("{:#X}", value)
            } else {
                format!("{:X}", value)
            }
        }
        Some(b'o') => {
            if spec.alternate {
                format!("{:#o}", value)
            } else {
                format!("{:o}", value)
            }
        }
        Some(b'b') => {
            if spec.alternate {
                format!("{:#b}", value)
            } else {
                format!("{:b}", value)
            }
        }
        _ => format!("{}", value),
    };

    // Apply precision: zero-pad digits after any prefix.
    if let Some(p) = spec.precision {
        let p = p as usize;
        let prefix_len =
            if spec.alternate && matches!(spec.type_char, Some(b'x' | b'X' | b'o' | b'b')) {
                2
            } else {
                0
            };
        let digits_len = s.len() - prefix_len;
        if digits_len < p {
            let mut padded = String::with_capacity(s.len() + (p - digits_len));
            padded.push_str(&s[..prefix_len]);
            for _ in 0..(p - digits_len) {
                padded.push('0');
            }
            padded.push_str(&s[prefix_len..]);
            s = padded;
        }
    }

    format_with_spec(&s, spec, buf);
}

fn format_signed_impl(value: i64, spec: &FormatSpec, buf: &mut Vec<u8>) {
    let mut s = match spec.type_char {
        Some(b'?') => format!("{:?}", value),
        _ => format!("{}", value),
    };

    // Apply precision: zero-pad digits after any minus sign.
    if let Some(p) = spec.precision {
        let p = p as usize;
        let prefix_len = if value < 0 { 1 } else { 0 };
        let digits_len = s.len() - prefix_len;
        if digits_len < p {
            let mut padded = String::with_capacity(s.len() + (p - digits_len));
            padded.push_str(&s[..prefix_len]);
            for _ in 0..(p - digits_len) {
                padded.push('0');
            }
            padded.push_str(&s[prefix_len..]);
            s = padded;
        }
    }

    format_with_spec(&s, spec, buf);
}

fn format_float_impl(value: f64, spec: &FormatSpec, buf: &mut Vec<u8>) {
    let s = match spec.type_char {
        Some(b'?') if spec.alternate => format!("{:#?}", value),
        Some(b'?') => format!("{:?}", value),
        Some(b'e') => {
            if let Some(p) = spec.precision {
                format!("{:.prec$e}", value, prec = p as usize)
            } else {
                format!("{:e}", value)
            }
        }
        Some(b'E') => {
            if let Some(p) = spec.precision {
                format!("{:.prec$E}", value, prec = p as usize)
            } else {
                format!("{:E}", value)
            }
        }
        _ => {
            if let Some(p) = spec.precision {
                format!("{:.prec$}", value, prec = p as usize)
            } else {
                format!("{}", value)
            }
        }
    };

    format_with_spec(&s, spec, buf);
}

#[cfg(test)]
mod tests {
    // Float literals in these tests (e.g. 3.14) are deliberate formatting
    // inputs, not attempts to approximate a math constant; the exact value
    // drives the expected output string.
    #![allow(clippy::approx_constant)]

    use super::*;

    #[test]
    fn validate_empty() {
        assert_eq!(validate_fmt(""), Ok(0));
    }

    #[test]
    fn validate_literal_only() {
        assert_eq!(validate_fmt("hello world"), Ok(0));
    }

    #[test]
    fn validate_single_empty_spec() {
        assert_eq!(validate_fmt("{}"), Ok(1));
    }

    #[test]
    fn validate_multiple_specs() {
        assert_eq!(validate_fmt("{} {} {}"), Ok(3));
    }

    #[test]
    fn validate_mixed_literal_and_spec() {
        assert_eq!(validate_fmt("val={} err={:?}"), Ok(2));
    }

    #[test]
    fn validate_escaped_braces() {
        assert_eq!(validate_fmt("{{not a spec}}"), Ok(0));
    }

    #[test]
    fn validate_escaped_open_and_real_close() {
        assert_eq!(validate_fmt("{{ {}"), Ok(1));
    }

    #[test]
    fn validate_escaped_close_brace() {
        assert_eq!(validate_fmt("}} after literal"), Ok(0));
    }

    #[test]
    fn validate_all_type_chars() {
        for c in &["?", "x", "X", "o", "b", "e", "E"] {
            let fmt = format!("{{:{}}}", c);
            assert_eq!(validate_fmt(&fmt), Ok(1), "failed for type char '{}'", c);
        }
    }

    #[test]
    fn validate_alternate_debug() {
        assert_eq!(validate_fmt("{:#?}"), Ok(1));
    }

    #[test]
    fn validate_alternate_hex() {
        assert_eq!(validate_fmt("{:#x}"), Ok(1));
    }

    #[test]
    fn validate_width() {
        assert_eq!(validate_fmt("{:10}"), Ok(1));
    }

    #[test]
    fn validate_width_and_type() {
        assert_eq!(validate_fmt("{:10?}"), Ok(1));
    }

    #[test]
    fn validate_precision() {
        assert_eq!(validate_fmt("{:.5}"), Ok(1));
    }

    #[test]
    fn validate_width_and_precision() {
        assert_eq!(validate_fmt("{:10.5}"), Ok(1));
    }

    #[test]
    fn validate_fill_align() {
        assert_eq!(validate_fmt("{:*>}"), Ok(1));
        assert_eq!(validate_fmt("{:*<}"), Ok(1));
        assert_eq!(validate_fmt("{:*^}"), Ok(1));
    }

    #[test]
    fn validate_align_without_fill() {
        assert_eq!(validate_fmt("{:>}"), Ok(1));
        assert_eq!(validate_fmt("{:<}"), Ok(1));
        assert_eq!(validate_fmt("{:^}"), Ok(1));
    }

    #[test]
    fn validate_zero_fill() {
        assert_eq!(validate_fmt("{:0}"), Ok(1));
        assert_eq!(validate_fmt("{:05}"), Ok(1));
    }

    #[test]
    fn validate_fill_align_width_type() {
        assert_eq!(validate_fmt("{:*>10x}"), Ok(1));
    }

    #[test]
    fn validate_cosmetic_name() {
        assert_eq!(validate_fmt("{name}"), Ok(1));
        assert_eq!(validate_fmt("{name:?}"), Ok(1));
        assert_eq!(validate_fmt("{name:#x}"), Ok(1));
        assert_eq!(validate_fmt("{name:>10}"), Ok(1));
    }

    #[test]
    fn validate_name_with_empty_spec() {
        assert_eq!(validate_fmt("{name:}"), Ok(1));
    }

    #[test]
    fn validate_full_spec() {
        assert_eq!(validate_fmt("{:*>10.5#x}"), Ok(1));
    }

    #[test]
    fn reject_unclosed_brace() {
        assert!(validate_fmt("hello {").is_err());
        assert!(validate_fmt("hello {world").is_err());
    }

    #[test]
    fn reject_unmatched_close() {
        assert!(validate_fmt("hello }").is_err());
    }

    #[test]
    fn reject_sign_flag_plus() {
        assert!(validate_fmt("{:+}").is_err());
    }

    #[test]
    fn reject_sign_flag_minus() {
        assert!(validate_fmt("{:-}").is_err());
    }

    #[test]
    fn reject_invalid_fill_plus() {
        assert!(validate_fmt("{:+>}").is_err());
    }

    #[test]
    fn reject_invalid_fill_minus() {
        assert!(validate_fmt("{:-^}").is_err());
    }

    #[test]
    fn reject_invalid_type_char() {
        assert!(validate_fmt("{:z}").is_err());
    }

    #[test]
    fn reject_positional_param() {
        assert!(validate_fmt("{0}").is_err());
        assert!(validate_fmt("{1:?}").is_err());
    }

    #[test]
    fn reject_trailing_garbage_after_type() {
        assert!(validate_fmt("{:?x}").is_err());
    }

    #[test]
    fn reject_dot_without_digits() {
        assert!(validate_fmt("{:.}").is_err());
        assert!(validate_fmt("{:.x}").is_err());
    }

    #[test]
    fn parse_empty() {
        let fs = parse_spec("");
        assert_eq!(fs.type_char, None);
        assert_eq!(fs.width, None);
        assert_eq!(fs.precision, None);
    }

    #[test]
    fn parse_display() {
        let fs = parse_spec(":");
        assert_eq!(fs.type_char, None);
    }

    #[test]
    fn parse_debug() {
        let fs = parse_spec(":?");
        assert_eq!(fs.type_char, Some(b'?'));
    }

    #[test]
    fn parse_hex() {
        let fs = parse_spec(":x");
        assert_eq!(fs.type_char, Some(b'x'));
    }

    #[test]
    fn parse_alternate_hex() {
        let fs = parse_spec(":#x");
        assert_eq!(fs.type_char, Some(b'x'));
        assert!(fs.alternate);
    }

    #[test]
    fn parse_width() {
        let fs = parse_spec(":10");
        assert_eq!(fs.width, Some(10));
        assert_eq!(fs.type_char, None);
    }

    #[test]
    fn parse_width_and_type() {
        let fs = parse_spec(":10?");
        assert_eq!(fs.width, Some(10));
        assert_eq!(fs.type_char, Some(b'?'));
    }

    #[test]
    fn parse_precision() {
        let fs = parse_spec(":.5");
        assert_eq!(fs.precision, Some(5));
    }

    #[test]
    fn parse_width_and_precision() {
        let fs = parse_spec(":10.5");
        assert_eq!(fs.width, Some(10));
        assert_eq!(fs.precision, Some(5));
    }

    #[test]
    fn parse_fill_align() {
        let fs = parse_spec(":*>");
        assert_eq!(fs.fill, b'*');
        assert_eq!(fs.align, Align::Right);
    }

    #[test]
    fn parse_fill_center() {
        let fs = parse_spec(":*^");
        assert_eq!(fs.fill, b'*');
        assert_eq!(fs.align, Align::Center);
    }

    #[test]
    fn parse_align_left_without_fill() {
        let fs = parse_spec(":<");
        assert_eq!(fs.fill, b' '); // default
        assert_eq!(fs.align, Align::Left);
    }

    #[test]
    fn parse_zero_fill() {
        let fs = parse_spec(":0");
        assert!(fs.zero_fill);
        assert_eq!(fs.width, None);
    }

    #[test]
    fn parse_zero_fill_with_width() {
        let fs = parse_spec(":05");
        assert!(fs.zero_fill);
        assert_eq!(fs.width, Some(5));
    }

    #[test]
    fn parse_cosmetic_name_with_spec() {
        let fs = parse_spec("my_var:?");
        assert_eq!(fs.type_char, Some(b'?'));
    }

    #[test]
    fn parse_name_only_returns_default() {
        let fs = parse_spec("only_a_name");
        assert_eq!(fs.type_char, None);
        assert_eq!(fs.width, None);
    }

    #[test]
    fn format_with_spec_no_width_appends_as_is() {
        let mut buf = Vec::new();
        let spec = FormatSpec::default();
        format_with_spec("hello", &spec, &mut buf);
        assert_eq!(buf, b"hello");
    }

    #[test]
    fn format_with_spec_left_align() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            width: Some(10),
            align: Align::Left,
            ..Default::default()
        };
        format_with_spec("hi", &spec, &mut buf);
        assert_eq!(buf, b"hi        ");
    }

    #[test]
    fn format_with_spec_right_align() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            width: Some(10),
            align: Align::Right,
            ..Default::default()
        };
        format_with_spec("hi", &spec, &mut buf);
        assert_eq!(buf, b"        hi");
    }

    #[test]
    fn format_with_spec_center_align() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            width: Some(9),
            align: Align::Center,
            ..Default::default()
        };
        format_with_spec("hi", &spec, &mut buf);
        assert_eq!(buf, b"   hi    ");
    }

    #[test]
    fn format_with_spec_zero_fill_overrides_fill_and_align() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            width: Some(5),
            zero_fill: true,
            fill: b'*',
            ..Default::default()
        };
        format_with_spec("42", &spec, &mut buf);
        assert_eq!(buf, b"00042");
    }

    #[test]
    fn format_with_spec_exact_width_no_padding() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            width: Some(5),
            align: Align::Right,
            ..Default::default()
        };
        format_with_spec("hello", &spec, &mut buf);
        assert_eq!(buf, b"hello");
    }

    #[test]
    fn format_with_spec_wider_than_width_no_padding() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            width: Some(3),
            ..Default::default()
        };
        format_with_spec("hello", &spec, &mut buf);
        assert_eq!(buf, b"hello");
    }

    #[test]
    fn format_with_spec_custom_fill() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            width: Some(8),
            align: Align::Right,
            fill: b'.',
            ..Default::default()
        };
        format_with_spec("abc", &spec, &mut buf);
        assert_eq!(buf, b".....abc");
    }

    #[test]
    fn format_u64_display() {
        let mut buf = Vec::new();
        let spec = FormatSpec::default();
        format_u64(42, &spec, &mut buf);
        assert_eq!(buf, b"42");
    }

    #[test]
    fn format_u64_debug() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'?'),
            ..Default::default()
        };
        format_u64(42, &spec, &mut buf);
        assert_eq!(buf, b"42");
    }

    #[test]
    fn format_u64_lower_hex() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'x'),
            ..Default::default()
        };
        format_u64(255, &spec, &mut buf);
        assert_eq!(buf, b"ff");
    }

    #[test]
    fn format_u64_upper_hex() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'X'),
            ..Default::default()
        };
        format_u64(255, &spec, &mut buf);
        assert_eq!(buf, b"FF");
    }

    #[test]
    fn format_u64_alternate_hex() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'x'),
            alternate: true,
            ..Default::default()
        };
        format_u64(255, &spec, &mut buf);
        assert_eq!(buf, b"0xff");
    }

    #[test]
    fn format_u64_alternate_upper_hex() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'X'),
            alternate: true,
            ..Default::default()
        };
        format_u64(255, &spec, &mut buf);
        assert_eq!(buf, b"0xFF");
    }

    #[test]
    fn format_u64_octal() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'o'),
            ..Default::default()
        };
        format_u64(8, &spec, &mut buf);
        assert_eq!(buf, b"10");
    }

    #[test]
    fn format_u64_alternate_octal() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'o'),
            alternate: true,
            ..Default::default()
        };
        format_u64(8, &spec, &mut buf);
        assert_eq!(buf, b"0o10");
    }

    #[test]
    fn format_u64_binary() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'b'),
            ..Default::default()
        };
        format_u64(5, &spec, &mut buf);
        assert_eq!(buf, b"101");
    }

    #[test]
    fn format_u64_alternate_binary() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'b'),
            alternate: true,
            ..Default::default()
        };
        format_u64(5, &spec, &mut buf);
        assert_eq!(buf, b"0b101");
    }

    #[test]
    fn format_u64_with_precision_zero_pads() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            precision: Some(5),
            ..Default::default()
        };
        format_u64(42, &spec, &mut buf);
        assert_eq!(buf, b"00042");
    }

    #[test]
    fn format_u64_with_precision_hex() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'x'),
            precision: Some(4),
            ..Default::default()
        };
        format_u64(255, &spec, &mut buf);
        assert_eq!(buf, b"00ff");
    }

    #[test]
    fn format_u64_with_precision_alternate_hex() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'x'),
            alternate: true,
            precision: Some(4),
            ..Default::default()
        };
        format_u64(255, &spec, &mut buf);
        assert_eq!(buf, b"0x00ff");
    }

    #[test]
    fn format_u64_with_width_right() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            width: Some(6),
            align: Align::Right,
            ..Default::default()
        };
        format_u64(42, &spec, &mut buf);
        assert_eq!(buf, b"    42");
    }

    #[test]
    fn format_u64_with_width_and_precision() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            width: Some(6),
            precision: Some(4),
            ..Default::default()
        };
        format_u64(42, &spec, &mut buf);
        assert_eq!(buf, b"0042  ");
    }

    #[test]
    fn format_u64_zero() {
        let mut buf = Vec::new();
        format_u64(0, &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"0");
    }

    #[test]
    fn format_u64_large() {
        let mut buf = Vec::new();
        format_u64(u64::MAX, &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"18446744073709551615");
    }

    #[test]
    fn format_i64_display() {
        let mut buf = Vec::new();
        format_i64(-42, &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"-42");
    }

    #[test]
    fn format_i64_positive() {
        let mut buf = Vec::new();
        format_i64(42, &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"42");
    }

    #[test]
    fn format_i64_debug() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'?'),
            ..Default::default()
        };
        format_i64(-42, &spec, &mut buf);
        assert_eq!(buf, b"-42");
    }

    #[test]
    fn format_i64_with_precision() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            precision: Some(5),
            ..Default::default()
        };
        format_i64(-42, &spec, &mut buf);
        assert_eq!(buf, b"-00042");
    }

    #[test]
    fn format_i64_zero() {
        let mut buf = Vec::new();
        format_i64(0, &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"0");
    }

    #[test]
    fn format_f64_display() {
        let mut buf = Vec::new();
        format_f64(3.14, &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"3.14");
    }

    #[test]
    fn format_f64_debug() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'?'),
            ..Default::default()
        };
        format_f64(3.14, &spec, &mut buf);
        assert_eq!(buf, b"3.14");
    }

    #[test]
    fn format_f64_with_precision() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            precision: Some(2),
            ..Default::default()
        };
        format_f64(3.14159, &spec, &mut buf);
        assert_eq!(buf, b"3.14");
    }

    #[test]
    fn format_f64_lower_exp() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'e'),
            ..Default::default()
        };
        format_f64(3.14, &spec, &mut buf);
        assert_eq!(buf, b"3.14e0");
    }

    #[test]
    fn format_f64_upper_exp() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'E'),
            ..Default::default()
        };
        format_f64(3.14, &spec, &mut buf);
        assert_eq!(buf, b"3.14E0");
    }

    #[test]
    fn format_f64_exp_with_precision() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'e'),
            precision: Some(4),
            ..Default::default()
        };
        format_f64(3.14159, &spec, &mut buf);
        assert_eq!(buf, b"3.1416e0");
    }

    #[test]
    fn format_f64_with_width() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            width: Some(10),
            align: Align::Right,
            ..Default::default()
        };
        format_f64(3.14, &spec, &mut buf);
        assert_eq!(buf, b"      3.14");
    }

    #[test]
    fn format_bool_true() {
        let mut buf = Vec::new();
        format_bool(true, &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"true");
    }

    #[test]
    fn format_bool_false() {
        let mut buf = Vec::new();
        format_bool(false, &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"false");
    }

    #[test]
    fn format_bool_debug() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'?'),
            ..Default::default()
        };
        format_bool(true, &spec, &mut buf);
        assert_eq!(buf, b"true");
    }

    #[test]
    fn format_bool_with_width() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            width: Some(8),
            align: Align::Right,
            ..Default::default()
        };
        format_bool(false, &spec, &mut buf);
        assert_eq!(buf, b"   false");
    }

    #[test]
    fn format_str_display() {
        let mut buf = Vec::new();
        format_str("hello", &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"hello");
    }

    #[test]
    fn format_str_debug() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'?'),
            ..Default::default()
        };
        format_str("hello", &spec, &mut buf);
        assert_eq!(buf, b"\"hello\"");
    }

    #[test]
    fn format_str_alternate_debug() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'?'),
            alternate: true,
            ..Default::default()
        };
        format_str("hello", &spec, &mut buf);
        assert_eq!(buf, b"\"hello\"");
    }

    #[test]
    fn format_str_with_precision_truncates() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            precision: Some(3),
            ..Default::default()
        };
        format_str("hello", &spec, &mut buf);
        assert_eq!(buf, b"hel");
    }

    #[test]
    fn format_str_precision_longer_than_string() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            precision: Some(10),
            ..Default::default()
        };
        format_str("hi", &spec, &mut buf);
        assert_eq!(buf, b"hi");
    }

    #[test]
    fn format_str_precision_zero() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            precision: Some(0),
            ..Default::default()
        };
        format_str("hello", &spec, &mut buf);
        assert_eq!(buf, b"");
    }

    #[test]
    fn format_str_with_width() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            width: Some(10),
            align: Align::Right,
            ..Default::default()
        };
        format_str("hi", &spec, &mut buf);
        assert_eq!(buf, b"        hi");
    }

    #[test]
    fn format_str_utf8_truncation() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            precision: Some(2),
            ..Default::default()
        };
        format_str("a\u{00E9}c", &spec, &mut buf);
        assert_eq!(buf, b"a\xC3\xA9"); // 2 chars: 'a' + 'é' (2 bytes)
    }

    #[test]
    fn format_u32_delegates() {
        let mut buf = Vec::new();
        let spec = FormatSpec {
            type_char: Some(b'x'),
            ..Default::default()
        };
        format_u32(255, &spec, &mut buf);
        assert_eq!(buf, b"ff");
    }

    #[test]
    fn format_i32_negative() {
        let mut buf = Vec::new();
        format_i32(-100, &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"-100");
    }

    #[test]
    fn format_f32_display() {
        let mut buf = Vec::new();
        format_f32(1.5, &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"1.5");
    }

    #[test]
    fn format_u16_display() {
        let mut buf = Vec::new();
        format_u16(65535, &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"65535");
    }

    #[test]
    fn format_i16_negative() {
        let mut buf = Vec::new();
        format_i16(-32768, &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"-32768");
    }

    #[test]
    fn format_u8_display() {
        let mut buf = Vec::new();
        format_u8(255, &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"255");
    }

    #[test]
    fn format_i8_negative() {
        let mut buf = Vec::new();
        format_i8(-128, &FormatSpec::default(), &mut buf);
        assert_eq!(buf, b"-128");
    }

    #[test]
    fn check_fmt_accepts_matching_arity() {
        // Runs the same const fn the macros use, at runtime: no panic on a
        // valid format string whose placeholder count matches the arguments.
        check_fmt("", 0);
        check_fmt("{}", 1);
        check_fmt("{} and {}", 2);
        check_fmt("{{escaped}} {}", 1);
    }

    #[test]
    #[should_panic(expected = "number of arguments")]
    fn check_fmt_rejects_too_few_arguments() {
        check_fmt("{} {}", 1);
    }

    #[test]
    #[should_panic(expected = "number of arguments")]
    fn check_fmt_rejects_too_many_arguments() {
        check_fmt("{}", 2);
    }

    #[test]
    #[should_panic(expected = "unclosed")]
    fn check_fmt_rejects_invalid_syntax() {
        check_fmt("{", 0);
    }
}
