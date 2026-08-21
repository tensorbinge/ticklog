//! Initialization of the logging system.
//!
//! [`crate::configure!`] initializes logging and returns a [`Guard`]. Logging stops
//! when the guard is dropped.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::affinity;
use crate::drain::Drain;
use crate::error::TicklogError;
use crate::format::Template;
use crate::guard::Guard;
use crate::ring::MIN_RING_SIZE;
use crate::sink::LogSink;
use crate::thread_buf::{REGISTRY, set_ring_size};
use crate::timestamp;

/// Minimum valid timezone offset in seconds east of UTC (UTC-12:00).
const MIN_TZ_OFFSET: i32 = -43_200;
/// Maximum valid timezone offset in seconds east of UTC (UTC+14:00).
const MAX_TZ_OFFSET: i32 = 50_400;

/// Default log-line pattern used when `configure!` does not specify `format:`.
pub(crate) const DEFAULT_LINE_PATTERN: &str = "{timestamp} {level} {file}:{line} {message}";

/// What a logging thread does when its buffer is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Backpressure {
    /// Discard the record and return immediately, never blocking the caller.
    /// This is the default.
    Drop,
    /// Spin until space frees up. Never drops records, but burns CPU while the
    /// buffer stays full. Useful for debugging and tests.
    Block,
}

/// Rejects ring sizes that are not a power of two or cannot admit a
/// max-size record. See [`crate::ring::MIN_RING_SIZE`] for the bound.
fn validate_ring_size(ring_size: usize) -> Result<(), TicklogError> {
    if !ring_size.is_power_of_two() || ring_size < MIN_RING_SIZE {
        return Err(TicklogError::InvalidRingSize(ring_size));
    }
    Ok(())
}

/// Rejects timezone offsets outside the valid range of
/// [-43200, 50400] seconds (UTC-12:00 to UTC+14:00).
fn validate_timezone_offset(offset: i32) -> Result<(), TicklogError> {
    if !(MIN_TZ_OFFSET..=MAX_TZ_OFFSET).contains(&offset) {
        return Err(TicklogError::InvalidTimezoneOffset(offset));
    }
    Ok(())
}

/// Initializes the logging system and returns a [`Guard`].
///
/// Every field is optional. The `format` key accepts a pattern string with
/// `{field}` placeholders (`timestamp`, `level`, `file`, `line`,
/// `thread_name`, `thread_id`, `message`) with `std::fmt`-style format
/// specs (`:<8`, `:>10`, `:#x`, `.precision`, etc.).
///
/// ```no_run
/// # use ticklog::{ConsoleSink, Level, Backpressure};
/// let _guard = ticklog::configure! {
///     sink: ConsoleSink::stderr(),
///     max_level: Level::Trace,
///     backpressure: Backpressure::Drop,
///     ring_size: 4 * 1024 * 1024,
///     timezone_offset: 3600,
///     drain_affinity: Some(vec![0]),
///     format: "{timestamp} [{level:>5}] {file}:{line} {message}",
/// }
/// .unwrap();
/// ```
#[macro_export]
macro_rules! configure {
    ($($key:ident : $val:expr),* $(,)?) => {{
        // `#[macro_export]` hoists these bridge macros to the calling crate's
        // root regardless of the surrounding block, so the logging macros can
        // resolve them without a path. The trade-off is that two sibling crates
        // each calling `configure!` would collide on these names at link time.
        // That cannot happen in practice because `configure!` is one-shot per
        // process and panics on a second call.
        #[allow(non_local_definitions)]
        #[macro_export]
        macro_rules! __ticklog_max_level {
            () => { $crate::configure!(__pick max_level { $($key : $val ,)* }) };
        }
        #[allow(non_local_definitions)]
        #[macro_export]
        macro_rules! __ticklog_backpressure {
            () => { $crate::configure!(__pick backpressure { $($key : $val ,)* }) };
        }
        #[allow(non_local_definitions)]
        #[macro_export]
        macro_rules! __ticklog_ring_size {
            () => { $crate::configure!(__pick ring_size { $($key : $val ,)* }) };
        }
        $crate::__private::__configure_rt(
            Box::new($crate::configure!(__pick sink { $($key : $val ,)* })),
            $crate::configure!(__pick timezone_offset { $($key : $val ,)* }),
            $crate::configure!(__pick drain_affinity { $($key : $val ,)* }),
            $crate::configure!(__pick format { $($key : $val ,)* }),
            $crate::configure!(__pick ring_size { $($key : $val ,)* }),
        )
    }};

    // __pick max_level
    (__pick max_level { max_level: $val:expr, $($rest:tt)* }) => { $val };
    (__pick max_level { $_other:ident : $_val:expr, $($rest:tt)* }) => {
        $crate::configure!(__pick max_level { $($rest)* })
    };
    (__pick max_level { }) => { $crate::Level::Info };

    // __pick backpressure
    (__pick backpressure { backpressure: $val:expr, $($rest:tt)* }) => { $val };
    (__pick backpressure { $_other:ident : $_val:expr, $($rest:tt)* }) => {
        $crate::configure!(__pick backpressure { $($rest)* })
    };
    (__pick backpressure { }) => { $crate::Backpressure::Drop };

    // __pick ring_size
    // The picked value is wrapped in a const block so a non-constant
    // expression (e.g. a runtime variable) is a compile error.
    (__pick ring_size { ring_size: $val:expr, $($rest:tt)* }) => { const { $val } };
    (__pick ring_size { $_other:ident : $_val:expr, $($rest:tt)* }) => {
        $crate::configure!(__pick ring_size { $($rest)* })
    };
    (__pick ring_size { }) => { $crate::__private::DEFAULT_RING_SIZE };

    // __pick sink
    (__pick sink { sink: $val:expr, $($rest:tt)* }) => { $val };
    (__pick sink { $_other:ident : $_val:expr, $($rest:tt)* }) => {
        $crate::configure!(__pick sink { $($rest)* })
    };
    (__pick sink { }) => { $crate::ConsoleSink::stderr() };

    // __pick timezone_offset
    (__pick timezone_offset { timezone_offset: $val:expr, $($rest:tt)* }) => { $val };
    (__pick timezone_offset { $_other:ident : $_val:expr, $($rest:tt)* }) => {
        $crate::configure!(__pick timezone_offset { $($rest)* })
    };
    (__pick timezone_offset { }) => { 0i32 };

    // __pick drain_affinity
    (__pick drain_affinity { drain_affinity: $val:expr, $($rest:tt)* }) => { $val };
    (__pick drain_affinity { $_other:ident : $_val:expr, $($rest:tt)* }) => {
        $crate::configure!(__pick drain_affinity { $($rest)* })
    };
    (__pick drain_affinity { }) => { None::<Vec<usize>> };

    // __pick format
    (__pick format { format: $val:expr, $($rest:tt)* }) => { $val };
    (__pick format { $_other:ident : $_val:expr, $($rest:tt)* }) => {
        $crate::configure!(__pick format { $($rest)* })
    };
    (__pick format { }) => { "" };
}

/// Runtime portion of [`configure!`]: spawns the drain, calibrates the clock,
/// claims the ring registry.
#[doc(hidden)]
pub fn __configure_rt(
    sink: Box<dyn LogSink>,
    timezone_offset: i32,
    drain_affinity: Option<Vec<usize>>,
    format_str: impl Into<String>,
    ring_size: usize,
) -> Result<Guard, TicklogError> {
    validate_timezone_offset(timezone_offset)?;
    validate_ring_size(ring_size)?;

    // Parse the log-line pattern before claiming any global resources, so an
    // invalid pattern rejects cleanly without leaving side-effects.
    let format_str: String = format_str.into();
    let pattern_str = if format_str.is_empty() {
        DEFAULT_LINE_PATTERN
    } else {
        &format_str
    };
    let line_pattern = Template::parse(pattern_str).map_err(TicklogError::InvalidFormatPattern)?;

    // The configured size is claimed alongside the registry: threads cannot
    // create rings before the registry exists, so every ring sees the value
    // set here.
    set_ring_size(ring_size);
    REGISTRY
        .set(Mutex::new(Vec::new()))
        .map_err(|_| TicklogError::AlreadyInitialized)?;

    let calibration = timestamp::calibrate();

    let shutdown = Arc::new(AtomicBool::new(false));
    let drain = Drain::new(
        sink,
        timezone_offset,
        Arc::clone(&shutdown),
        calibration,
        line_pattern,
        ring_size,
    );

    let drain_affinity_opt = drain_affinity.clone();
    let handle = thread::Builder::new()
        .name("ticklog-drain".to_string())
        .spawn(move || {
            if let Some(ref cores) = drain_affinity_opt {
                affinity::pin_thread(cores);
            }
            let mut drain = drain;
            drain.run();
        })
        .map_err(TicklogError::DrainSpawnFailed)?;

    Ok(Guard::new(handle, shutdown))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::ConsoleSink;

    #[test]
    fn backpressure_discriminants() {
        assert_eq!(Backpressure::Drop as u8, 0);
        assert_eq!(Backpressure::Block as u8, 1);
    }

    #[test]
    fn validate_ring_size_accepts_min_and_larger_powers_of_two() {
        assert!(validate_ring_size(crate::ring::MIN_RING_SIZE).is_ok());
        assert!(validate_ring_size(1_048_576).is_ok());
        assert!(validate_ring_size(64 * 1024 * 1024).is_ok());
    }

    #[test]
    fn validate_ring_size_rejects_non_power_of_two() {
        assert!(matches!(
            validate_ring_size(100_000),
            Err(TicklogError::InvalidRingSize(100_000))
        ));
        assert!(matches!(
            validate_ring_size(0),
            Err(TicklogError::InvalidRingSize(0))
        ));
    }

    #[test]
    fn validate_ring_size_rejects_below_min() {
        assert!(matches!(
            validate_ring_size(65_536),
            Err(TicklogError::InvalidRingSize(65_536))
        ));
    }

    #[test]
    fn validate_timezone_offset_accepts_bounds_and_inside() {
        assert!(validate_timezone_offset(MIN_TZ_OFFSET).is_ok());
        assert!(validate_timezone_offset(MAX_TZ_OFFSET).is_ok());
        assert!(validate_timezone_offset(0).is_ok());
        assert!(validate_timezone_offset(3_600).is_ok());
    }

    #[test]
    fn validate_timezone_offset_rejects_out_of_range() {
        assert!(matches!(
            validate_timezone_offset(50_401),
            Err(TicklogError::InvalidTimezoneOffset(50_401))
        ));
        assert!(matches!(
            validate_timezone_offset(-43_201),
            Err(TicklogError::InvalidTimezoneOffset(-43_201))
        ));
    }

    #[test]
    fn configure_rt_rejects_out_of_range_timezone_offset() {
        // Boundary cases live in the `validate_timezone_offset` unit tests;
        // this checks the wiring through `__configure_rt` once.
        let result = __configure_rt(
            Box::new(ConsoleSink::stderr()),
            50_401,
            None,
            "",
            crate::ring::DEFAULT_RING_SIZE,
        );
        assert!(matches!(
            result,
            Err(TicklogError::InvalidTimezoneOffset(50_401))
        ));
    }

    #[test]
    fn configure_rt_already_initialized() {
        let _ = REGISTRY.set(Mutex::new(Vec::new()));
        let result = __configure_rt(
            Box::new(ConsoleSink::stderr()),
            0,
            None,
            "",
            crate::ring::DEFAULT_RING_SIZE,
        );
        assert!(matches!(result, Err(TicklogError::AlreadyInitialized)));
    }

    #[test]
    fn configure_rt_rejects_invalid_ring_size() {
        let result = __configure_rt(Box::new(ConsoleSink::stderr()), 0, None, "", 100_000);
        assert!(matches!(
            result,
            Err(TicklogError::InvalidRingSize(100_000))
        ));
    }

    #[test]
    fn configure_rt_rejects_invalid_format_pattern() {
        // We need REGISTRY unset for this to reach the parse step; use an
        // invalid pattern to trigger the error.
        let result = __configure_rt(
            Box::new(ConsoleSink::stderr()),
            0,
            None,
            "{unknown_field}",
            crate::ring::DEFAULT_RING_SIZE,
        );
        assert!(matches!(result, Err(TicklogError::InvalidFormatPattern(_))));
    }
}
