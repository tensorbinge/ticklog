use std::{error::Error, fmt, io};

/// Errors returned by ticklog operations.
#[derive(Debug)]
#[non_exhaustive]
pub enum TicklogError {
    /// Logger has not been initialized.
    NotInitialized,

    /// Logger has already been initialized.
    AlreadyInitialized,

    /// Drain thread could not be spawned.
    DrainSpawnFailed(io::Error),

    /// Sink write failed.
    SinkWriteFailed(io::Error),

    /// A configured timezone offset was outside the valid range of
    /// [-43200, 50400] seconds (UTC-12:00 to UTC+14:00).
    InvalidTimezoneOffset(i32),
}

impl fmt::Display for TicklogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInitialized => write!(f, "ticklog has not been initialized"),
            Self::AlreadyInitialized => {
                write!(f, "ticklog has already been initialized")
            }
            Self::DrainSpawnFailed(e) => write!(f, "failed to spawn drain thread: {}", e),
            Self::SinkWriteFailed(e) => write!(f, "sink write failed: {}", e),
            Self::InvalidTimezoneOffset(secs) => write!(
                f,
                "invalid timezone offset: {} seconds is outside [-43200, 50400]",
                secs
            ),
        }
    }
}

impl Error for TicklogError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::DrainSpawnFailed(e) => Some(e),
            Self::SinkWriteFailed(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_not_initialized() {
        assert_eq!(
            TicklogError::NotInitialized.to_string(),
            "ticklog has not been initialized"
        );
    }

    #[test]
    fn display_already_initialized() {
        assert_eq!(
            TicklogError::AlreadyInitialized.to_string(),
            "ticklog has already been initialized"
        );
    }

    #[test]
    fn display_drain_spawn_failed() {
        let e = TicklogError::DrainSpawnFailed(io::Error::other("thread boom"));
        assert_eq!(e.to_string(), "failed to spawn drain thread: thread boom");
    }

    #[test]
    fn display_sink_write_failed() {
        let e =
            TicklogError::SinkWriteFailed(io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed"));
        assert_eq!(e.to_string(), "sink write failed: pipe closed");
    }

    #[test]
    fn display_invalid_timezone_offset() {
        let e = TicklogError::InvalidTimezoneOffset(99_999);
        assert_eq!(
            e.to_string(),
            "invalid timezone offset: 99999 seconds is outside [-43200, 50400]"
        );
        assert!(Error::source(&e).is_none());
    }

    #[test]
    fn source_wraps_io_error() {
        let inner = io::Error::other("oops");
        let e = TicklogError::DrainSpawnFailed(inner);
        let source = Error::source(&e).expect("DrainSpawnFailed should have a source");
        assert_eq!(source.to_string(), "oops");

        let inner = io::Error::new(io::ErrorKind::BrokenPipe, "pipe");
        let e = TicklogError::SinkWriteFailed(inner);
        let source = Error::source(&e).expect("SinkWriteFailed should have a source");
        assert_eq!(source.to_string(), "pipe");
    }

    #[test]
    fn source_none_for_unit_variants() {
        assert!(Error::source(&TicklogError::NotInitialized).is_none());
        assert!(Error::source(&TicklogError::AlreadyInitialized).is_none());
    }
}
