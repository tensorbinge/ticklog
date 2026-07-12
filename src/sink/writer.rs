//! [`WriterSink`]: adapts any [`io::Write`] into a [`LogSink`].
//!
//! This is the escape hatch for destinations that already implement
//! `io::Write`, such as files, sockets, pipes, and rotating-file writers.

use std::io::{self, Write};

use super::LogSink;
use crate::level::Level;

/// A [`LogSink`] that forwards each line, plus a trailing newline, to a wrapped
/// [`io::Write`].
///
/// The wrapper adds no buffering of its own; the wrapped writer is written
/// straight through, so pair it with a `BufWriter` (or a writer that already
/// buffers) if you want batched syscalls.
///
/// ```no_run
/// use std::fs::File;
/// use ticklog::WriterSink;
///
/// let file = File::create("app.log").unwrap();
/// let sink = WriterSink::new(file);
/// ```
pub struct WriterSink<W> {
    writer: W,
}

impl<W: Write + Send + 'static> WriterSink<W> {
    /// Wraps `writer` as a [`LogSink`].
    pub fn new(writer: W) -> Self {
        Self { writer }
    }
}

impl<W: Write + Send + 'static> LogSink for WriterSink<W> {
    fn accept(&mut self, line: &[u8], _level: Level) -> io::Result<()> {
        self.writer.write_all(line)?;
        self.writer.write_all(b"\n")
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn accept_writes_line_and_newline() {
        let mut sink = WriterSink::new(Vec::<u8>::new());
        sink.accept(b"hello", Level::Info).unwrap();
        assert_eq!(sink.writer, b"hello\n");
    }

    #[test]
    fn accept_appends_across_calls() {
        let mut sink = WriterSink::new(Vec::<u8>::new());
        sink.accept(b"a", Level::Info).unwrap();
        sink.accept(b"b", Level::Warn).unwrap();
        assert_eq!(sink.writer, b"a\nb\n");
    }

    #[test]
    fn flush_delegates_to_inner_writer() {
        /// A writer that records whether `flush` was called.
        struct FlushProbe(Arc<Mutex<bool>>);
        impl Write for FlushProbe {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                *self.0.lock().unwrap() = true;
                Ok(())
            }
        }

        let flushed = Arc::new(Mutex::new(false));
        let mut sink = WriterSink::new(FlushProbe(Arc::clone(&flushed)));
        sink.flush().unwrap();
        assert!(*flushed.lock().unwrap());
    }

    #[test]
    fn accept_propagates_write_error() {
        /// A writer that always fails.
        struct BrokenWriter;
        impl Write for BrokenWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "broken"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut sink = WriterSink::new(BrokenWriter);
        let result = sink.accept(b"data", Level::Info);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }
}
