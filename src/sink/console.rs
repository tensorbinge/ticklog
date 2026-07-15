//! [`ConsoleSink`]: writes log lines to stdout or stderr, optionally colored by
//! level.
//!
//! This is the default sink installed by [`configure!`]. Colors are
//! applied per level via ANSI SGR codes; [`ColorMode::Auto`] (the default)
//! enables them only when the target stream is a terminal, so redirected output
//! stays plain.

use std::io::{self, IsTerminal, Write};

use super::LogSink;
use crate::level::Level;

/// ANSI SGR reset sequence, appended after a colored line.
const RESET: &[u8] = b"\x1b[0m";

/// Returns the ANSI SGR color-set sequence for `level`.
///
/// The mapping is: ERROR bright red, WARN yellow, INFO green, DEBUG cyan,
/// TRACE bright black (gray). The whole line is wrapped in this code and
/// [`RESET`].
fn color_code(level: Level) -> &'static [u8] {
    match level {
        Level::Error => b"\x1b[91m", // bright red
        Level::Warn => b"\x1b[33m",  // yellow
        Level::Info => b"\x1b[32m",  // green
        Level::Debug => b"\x1b[36m", // cyan
        Level::Trace => b"\x1b[90m", // bright black (gray)
    }
}

/// When [`ConsoleSink`] applies ANSI colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorMode {
    /// Color only when the target stream is a terminal (the default).
    #[default]
    Auto,
    /// Always color, even when the stream is redirected to a file or pipe.
    Always,
    /// Never color.
    Never,
}

/// Resolves a [`ColorMode`] against a stream's terminal status into a concrete
/// on/off decision.
fn resolve(mode: ColorMode, stream: &Stream) -> bool {
    match mode {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => stream.is_terminal(),
    }
}

/// Renders one log line into `out`: an optional color prefix, the line bytes,
/// an optional reset, and a trailing newline. `out` is cleared first.
///
/// Kept separate from any real stream so the coloring logic is unit-testable
/// without touching stdout/stderr.
fn render(line: &[u8], level: Level, color: bool, out: &mut Vec<u8>) {
    out.clear();
    if color {
        out.extend_from_slice(color_code(level));
        out.extend_from_slice(line);
        out.extend_from_slice(RESET);
    } else {
        out.extend_from_slice(line);
    }
    out.push(b'\n');
}

/// The output stream a [`ConsoleSink`] targets. Holds the global stdout/stderr
/// handle so terminal detection and writes both go to the same stream.
enum Stream {
    Stdout(io::Stdout),
    Stderr(io::Stderr),
}

impl Stream {
    fn is_terminal(&self) -> bool {
        match self {
            Stream::Stdout(s) => s.is_terminal(),
            Stream::Stderr(s) => s.is_terminal(),
        }
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Stream::Stdout(s) => s.lock().write_all(bytes),
            Stream::Stderr(s) => s.lock().write_all(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Stdout(s) => s.lock().flush(),
            Stream::Stderr(s) => s.lock().flush(),
        }
    }
}

/// A [`LogSink`] that writes to stdout or stderr, optionally coloring each line
/// by its level.
///
/// ```
/// use ticklog::{ColorMode, ConsoleSink};
///
/// // Default: stderr, colored only when stderr is a terminal.
/// let sink = ConsoleSink::stderr();
///
/// // Force color off (e.g. when writing to a file you'll grep later).
/// let plain = ConsoleSink::stdout().with_color(ColorMode::Never);
/// ```
pub struct ConsoleSink {
    stream: Stream,
    mode: ColorMode,
    /// Resolved color decision, computed from `mode` and the stream's terminal
    /// status at construction (and whenever [`with_color`](Self::with_color)
    /// runs), so the terminal check does not repeat on every line.
    use_color: bool,
    /// Reused render scratch, so a colored line costs no per-record allocation.
    buf: Vec<u8>,
}

impl ConsoleSink {
    fn with_stream(stream: Stream) -> Self {
        let use_color = resolve(ColorMode::Auto, &stream);
        Self {
            stream,
            mode: ColorMode::Auto,
            use_color,
            buf: Vec::new(),
        }
    }

    /// Creates a console sink writing to standard output.
    pub fn stdout() -> Self {
        Self::with_stream(Stream::Stdout(io::stdout()))
    }

    /// Creates a console sink writing to standard error. This is the default
    /// sink installed by [`configure!`][crate::configure!].
    pub fn stderr() -> Self {
        Self::with_stream(Stream::Stderr(io::stderr()))
    }

    /// Sets when ANSI colors are applied. Default: [`ColorMode::Auto`].
    pub fn with_color(mut self, mode: ColorMode) -> Self {
        self.mode = mode;
        self.use_color = resolve(mode, &self.stream);
        self
    }
}

impl LogSink for ConsoleSink {
    fn accept(&mut self, line: &[u8], level: Level) -> io::Result<()> {
        render(line, level, self.use_color, &mut self.buf);
        self.stream.write_all(&self.buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_plain_is_line_plus_newline() {
        let mut out = Vec::new();
        render(b"hello", Level::Info, false, &mut out);
        assert_eq!(out, b"hello\n");
    }

    #[test]
    fn render_colored_wraps_line_in_code_and_reset() {
        let mut out = Vec::new();
        render(b"boom", Level::Error, true, &mut out);
        let mut expected = Vec::new();
        expected.extend_from_slice(b"\x1b[91m");
        expected.extend_from_slice(b"boom");
        expected.extend_from_slice(RESET);
        expected.push(b'\n');
        assert_eq!(out, expected);
    }

    #[test]
    fn render_clears_prior_contents() {
        let mut out = b"stale".to_vec();
        render(b"fresh", Level::Info, false, &mut out);
        assert_eq!(out, b"fresh\n");
    }

    #[test]
    fn color_code_is_distinct_per_level() {
        let codes = [
            color_code(Level::Error),
            color_code(Level::Warn),
            color_code(Level::Info),
            color_code(Level::Debug),
            color_code(Level::Trace),
        ];
        for (i, a) in codes.iter().enumerate() {
            for b in &codes[i + 1..] {
                assert_ne!(a, b, "two levels share a color code");
            }
        }
    }

    #[test]
    fn resolve_always_and_never_ignore_terminal() {
        let stream = Stream::Stdout(io::stdout());
        assert!(resolve(ColorMode::Always, &stream));
        assert!(!resolve(ColorMode::Never, &stream));
    }

    #[test]
    fn resolve_auto_follows_terminal_status() {
        let stream = Stream::Stdout(io::stdout());
        assert_eq!(resolve(ColorMode::Auto, &stream), stream.is_terminal());
    }

    #[test]
    fn default_color_mode_is_auto() {
        assert_eq!(ColorMode::default(), ColorMode::Auto);
    }

    #[test]
    fn with_color_overrides_resolution() {
        let sink = ConsoleSink::stdout().with_color(ColorMode::Always);
        assert_eq!(sink.mode, ColorMode::Always);
        assert!(sink.use_color);

        let sink = ConsoleSink::stdout().with_color(ColorMode::Never);
        assert!(!sink.use_color);
    }

    #[test]
    fn new_sink_defaults_to_auto_mode() {
        let sink = ConsoleSink::stderr();
        assert_eq!(sink.mode, ColorMode::Auto);
    }
}
