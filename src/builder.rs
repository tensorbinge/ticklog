//! Initialization of the logging system.
//!
//! [`configure!`] initializes logging and returns a [`Guard`]. Logging stops
//! when the guard is dropped.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::affinity;
use crate::drain::{Drain, LogMetadata};
use crate::error::TicklogError;
use crate::guard::Guard;
use crate::sink::LogSink;
use crate::thread_buf::REGISTRY;
use crate::timestamp;

/// Minimum valid timezone offset in seconds east of UTC (UTC-12:00).
const MIN_TZ_OFFSET: i32 = -43_200;
/// Maximum valid timezone offset in seconds east of UTC (UTC+14:00).
const MAX_TZ_OFFSET: i32 = 50_400;

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

/// Initializes the logging system and returns a [`Guard`].
///
/// Every field is optional:
///
/// ```no_run
/// # use ticklog::{ConsoleSink, Level, Backpressure};
/// let _guard = ticklog::configure! {
///     sink: ConsoleSink::stderr(),
///     max_level: Level::Trace,
///     backpressure: Backpressure::Drop,
///     timezone_offset: 3600,
///     drain_affinity: Some(vec![0]),
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
        $crate::__private::__configure_rt(
            Box::new($crate::configure!(__pick sink { $($key : $val ,)* })),
            $crate::configure!(__pick timezone_offset { $($key : $val ,)* }),
            $crate::configure!(__pick drain_affinity { $($key : $val ,)* }),
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
}

/// Runtime portion of [`configure!`]: spawns the drain, calibrates the clock,
/// claims the ring registry.
#[doc(hidden)]
pub fn __configure_rt(
    sink: Box<dyn LogSink>,
    timezone_offset: i32,
    drain_affinity: Option<Vec<usize>>,
) -> Result<Guard, TicklogError> {
    if !(MIN_TZ_OFFSET..=MAX_TZ_OFFSET).contains(&timezone_offset) {
        return Err(TicklogError::InvalidTimezoneOffset(timezone_offset));
    }

    REGISTRY
        .set(Mutex::new(Vec::new()))
        .map_err(|_| TicklogError::AlreadyInitialized)?;

    let calibration = timestamp::calibrate();

    let shutdown = Arc::new(AtomicBool::new(false));
    let drain = Drain::new(
        sink,
        timezone_offset,
        LogMetadata::default(),
        Arc::clone(&shutdown),
        calibration,
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
    fn configure_rt_rejects_out_of_range_timezone_offset() {
        assert!(matches!(
            __configure_rt(Box::new(ConsoleSink::stderr()), 50_401, None),
            Err(TicklogError::InvalidTimezoneOffset(50_401))
        ));
        assert!(matches!(
            __configure_rt(Box::new(ConsoleSink::stderr()), -43_201, None),
            Err(TicklogError::InvalidTimezoneOffset(-43_201))
        ));
    }

    #[test]
    fn configure_rt_already_initialized() {
        let _ = REGISTRY.set(Mutex::new(Vec::new()));
        let result = __configure_rt(Box::new(ConsoleSink::stderr()), 0, None);
        assert!(matches!(result, Err(TicklogError::AlreadyInitialized)));
    }
}
