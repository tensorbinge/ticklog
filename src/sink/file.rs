//! [`FileSink`]: writes log lines to a single file through a buffered writer.
//!
//! This sink does not rotate, compress, or retire files; it is a plain
//! append-or-truncate file target. For rotation, wrap a rotating writer in a
//! [`WriterSink`](super::WriterSink).

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;

use super::LogSink;
use crate::level::Level;

/// Capacity of the [`BufWriter`] wrapping the file. Log lines accumulate here
/// and are written to the OS in one syscall when the buffer fills or the drain
/// flushes, keeping the file target off the syscall-per-record path.
const FILE_BUF_CAPACITY: usize = 64 * 1024;

/// A [`LogSink`] that writes lines to a single file, buffered.
///
/// Lines are held in a 64 KiB buffer and written to the OS when it fills or
/// when the drain flushes (on idle and at shutdown), so a burst of records
/// costs far fewer syscalls than one write each.
///
/// ```no_run
/// use ticklog::FileSink;
///
/// // Append to an existing log, creating it if absent (the default).
/// let sink = FileSink::new("app.log").unwrap();
///
/// // Or start fresh each run.
/// let sink = FileSink::truncate("app.log").unwrap();
/// ```
pub struct FileSink {
    writer: BufWriter<File>,
}

impl FileSink {
    fn from_file(file: File) -> Self {
        Self {
            writer: BufWriter::with_capacity(FILE_BUF_CAPACITY, file),
        }
    }

    /// Opens `path` for appending, creating it if it does not exist. Existing
    /// contents are preserved and new lines are added at the end.
    ///
    /// # Errors
    ///
    /// Returns any [`io::Error`] from opening the file (e.g. a missing parent
    /// directory or insufficient permissions).
    pub fn new<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self::from_file(file))
    }

    /// Opens `path` for writing, truncating it if it exists and creating it if
    /// it does not. Any prior contents are discarded.
    ///
    /// # Errors
    ///
    /// Returns any [`io::Error`] from opening the file.
    pub fn truncate<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        Ok(Self::from_file(file))
    }
}

impl LogSink for FileSink {
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
    use std::fs;
    use std::io::Read;

    /// Returns a unique temp path under the OS temp dir for one test. The file
    /// itself is created by the sink; the caller removes it at the end.
    fn temp_path(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("ticklog_filesink_{tag}.log"));
        // Start from a clean slate in case a prior run left the file behind.
        let _ = fs::remove_file(&p);
        p
    }

    fn read_to_string(path: &Path) -> String {
        let mut s = String::new();
        File::open(path)
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        s
    }

    #[test]
    fn accept_writes_line_and_newline_after_flush() {
        let path = temp_path("write");
        let mut sink = FileSink::new(&path).unwrap();
        sink.accept(b"hello world", Level::Info).unwrap();
        // Buffered: nothing guaranteed on disk until flush.
        sink.flush().unwrap();
        assert_eq!(read_to_string(&path), "hello world\n");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn flush_persists_the_tail() {
        let path = temp_path("flush_tail");
        let mut sink = FileSink::new(&path).unwrap();
        for i in 0..3 {
            sink.accept(format!("line {i}").as_bytes(), Level::Info).unwrap();
        }
        sink.flush().unwrap();
        assert_eq!(read_to_string(&path), "line 0\nline 1\nline 2\n");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn new_appends_to_existing_content() {
        let path = temp_path("append");
        {
            let mut sink = FileSink::new(&path).unwrap();
            sink.accept(b"first", Level::Info).unwrap();
            sink.flush().unwrap();
        }
        {
            // Re-opening with new() must not clobber the earlier line.
            let mut sink = FileSink::new(&path).unwrap();
            sink.accept(b"second", Level::Info).unwrap();
            sink.flush().unwrap();
        }
        assert_eq!(read_to_string(&path), "first\nsecond\n");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn truncate_discards_existing_content() {
        let path = temp_path("truncate");
        {
            let mut sink = FileSink::new(&path).unwrap();
            sink.accept(b"old", Level::Info).unwrap();
            sink.flush().unwrap();
        }
        {
            let mut sink = FileSink::truncate(&path).unwrap();
            sink.accept(b"new", Level::Info).unwrap();
            sink.flush().unwrap();
        }
        assert_eq!(read_to_string(&path), "new\n");
        fs::remove_file(&path).unwrap();
    }
}
