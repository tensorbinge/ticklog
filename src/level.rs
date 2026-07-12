use std::fmt;

/// The severity of a log message, ordered from most severe
/// ([`Error`](Level::Error)) to least severe ([`Trace`](Level::Trace)).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(u8)]
pub enum Level {
    /// A serious failure.
    Error = 1,
    /// A recoverable problem or unexpected condition.
    Warn = 2,
    /// A normal, noteworthy event.
    Info = 3,
    /// Detailed information useful while debugging.
    Debug = 4,
    /// Very fine-grained tracing.
    Trace = 5,
}

impl Level {
    /// Returns the static string representation of this level.
    ///
    /// ```
    /// assert_eq!(ticklog::Level::Error.as_str(), "ERROR");
    /// assert_eq!(ticklog::Level::Info.as_str(), "INFO");
    /// ```
    #[inline]
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        }
    }

    /// Returns the `u8` discriminant of this level.
    #[inline]
    pub(crate) fn to_u8(self) -> u8 {
        self as u8
    }

    /// Convert from a `u8` value. Returns `None` for values outside `1..=5`.
    #[inline]
    pub(crate) fn from_u8(b: u8) -> Option<Level> {
        match b {
            1 => Some(Level::Error),
            2 => Some(Level::Warn),
            3 => Some(Level::Info),
            4 => Some(Level::Debug),
            5 => Some(Level::Trace),
            _ => None,
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discriminants() {
        assert_eq!(Level::Error as u8, 1);
        assert_eq!(Level::Warn as u8, 2);
        assert_eq!(Level::Info as u8, 3);
        assert_eq!(Level::Debug as u8, 4);
        assert_eq!(Level::Trace as u8, 5);
    }

    #[test]
    fn ordering() {
        assert!(Level::Error < Level::Warn);
        assert!(Level::Warn < Level::Info);
        assert!(Level::Info < Level::Debug);
        assert!(Level::Debug < Level::Trace);
    }

    #[test]
    fn as_str_uppercase() {
        assert_eq!(Level::Error.as_str(), "ERROR");
        assert_eq!(Level::Warn.as_str(), "WARN");
        assert_eq!(Level::Info.as_str(), "INFO");
        assert_eq!(Level::Debug.as_str(), "DEBUG");
        assert_eq!(Level::Trace.as_str(), "TRACE");
    }

    #[test]
    fn display_matches_as_str() {
        assert_eq!(Level::Error.to_string(), "ERROR");
        assert_eq!(Level::Warn.to_string(), "WARN");
        assert_eq!(Level::Info.to_string(), "INFO");
        assert_eq!(Level::Debug.to_string(), "DEBUG");
        assert_eq!(Level::Trace.to_string(), "TRACE");
    }

    #[test]
    fn u8_roundtrip() {
        for level in [
            Level::Error,
            Level::Warn,
            Level::Info,
            Level::Debug,
            Level::Trace,
        ] {
            assert_eq!(Level::from_u8(level.to_u8()), Some(level));
        }
    }

    #[test]
    fn to_u8_is_discriminant() {
        assert_eq!(Level::Error.to_u8(), 1);
        assert_eq!(Level::Warn.to_u8(), 2);
        assert_eq!(Level::Info.to_u8(), 3);
        assert_eq!(Level::Debug.to_u8(), 4);
        assert_eq!(Level::Trace.to_u8(), 5);
    }

    #[test]
    fn from_u8_rejects_invalid() {
        assert_eq!(Level::from_u8(0), None);
        assert_eq!(Level::from_u8(6), None);
        assert_eq!(Level::from_u8(255), None);
    }
}
