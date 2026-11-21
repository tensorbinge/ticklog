//! Shared support for the integration tests.

use std::io;
use std::sync::{Arc, Mutex};

use ticklog::{Level, LogSink};

/// A sink that records every accepted line, for assertions after shutdown.
pub struct CaptureSink {
    /// Every line the drain delivered, in arrival order.
    pub lines: Arc<Mutex<Vec<String>>>,
}

impl LogSink for CaptureSink {
    fn accept(&mut self, line: &[u8], _level: Level) -> io::Result<()> {
        self.lines
            .lock()
            .unwrap()
            .push(String::from_utf8_lossy(line).into_owned());
        Ok(())
    }
}
