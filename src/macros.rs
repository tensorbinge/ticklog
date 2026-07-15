//! The `trace!`, `debug!`, `info!`, `warn!`, and `error!` logging macros and
//! the runtime entry point they expand into.
//!
//! Each macro checks the compile-time level ceiling, and only if the record
//! passes does it evaluate its arguments and call [`dispatch`]. Format-string
//! syntax and the placeholder-to-argument count are checked at compile time via
//! [`check_fmt`](crate::format::check_fmt); per-type specifier validity is
//! resolved at runtime by the drain.

use crate::builder::Backpressure;
use crate::level::Level;
use crate::record;
use crate::thread_buf::with_thread_buf;
use crate::timestamp;

/// The logging macros expand in the caller's crate, where `$crate` is `ticklog`
/// but `crate::record` is unreachable, so they hardcode the fixed-section base
/// record size below. This assertion keeps that literal honest: if the record
/// layout constants change, the build fails here instead of silently mis-sizing
/// every record.
const _: () = assert!(record::BASE_RECORD_SIZE == 41);

/// Monomorphized dispatch: timestamps, assembles the record via
/// [`assemble`](crate::record::assemble), and writes it to the thread's ring
/// buffer.
///
/// The `write_args` closure is monomorphized per unique argument-type
/// signature: the macro expands each `Loggable::type_tag()` /
/// `Loggable::encode()` call as a direct (non-vtable) invocation against the
/// concrete type. `policy` is a compile-time constant from
/// [`configure!`](crate::configure!) and is branch-folded away.
///
/// Not part of the public API.
#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
#[inline(always)]
pub fn dispatch(
    level: Level,
    fmt: &'static str,
    file: &'static str,
    line: u32,
    n_args: u8,
    total_size: usize,
    policy: Backpressure,
    write_args: impl FnOnce(&mut [u8]),
) {
    // Drop, don't truncate, a record too large for the u16 `total_size` field.
    //
    // ticklog targets ultra-low-latency hot paths (e.g. trade execution) where
    // keeping the producer thread alive and honoring timing guarantees outranks
    // capturing every byte of a pathological payload. Truncating would tax every
    // record with a wider string encoding and a branchier decode just to rescue
    // the rare case of logging a huge buffer; a general-purpose backend that
    // prizes debugging context over a jitter spike would choose the opposite, but
    // we do not, by design. The drop is intentionally silent and uncounted.
    if total_size > record::MAX_RECORD_SIZE {
        return;
    }

    with_thread_buf(|tb| {
        if let Some(slot) = tb.ring.reserve(total_size, policy) {
            let timestamp = timestamp::raw_timestamp();
            record::assemble(
                slot.ptr, level, timestamp, fmt, file, line, n_args, total_size, write_args,
            );
            tb.ring.publish(slot);
        }
    });
}

/// Expands one logging macro. Not part of the public API; use the level
/// macros (`trace!`, `debug!`, `info!`, `warn!`, `error!`).
///
/// The format string is validated at compile time, then the argument values
/// are evaluated only if the level passes the compile-time ceiling.
/// `file!()` and `line!()` capture the outer macro's call site.
///
/// Argument encoding uses monomorphized dispatch: each `Loggable` method
/// call resolves to a concrete impl at compile time with no vtable overhead.
#[doc(hidden)]
#[macro_export]
macro_rules! __ticklog_log {
    // Zero-argument arm: no args to encode, just the fixed record sections.
    ($level:expr, $fmt:literal $(,)?) => {{
        const _: () = $crate::__private::check_fmt($fmt, 0);
        if $level <= __ticklog_max_level!() {
            const __TOTAL: usize = $crate::__private::BASE_RECORD_SIZE;
            $crate::__private::dispatch(
                $level, $fmt, file!(), line!(), 0u8, __TOTAL,
                __ticklog_backpressure!(),
                |_buf| {},
            );
        }
    }};
    // Argument arm: monomorphized per-arg encoding, no vtable.
    ($level:expr, $fmt:literal, $($arg:expr),+ $(,)?) => {{
        const _: () = $crate::__private::check_fmt(
            $fmt,
            <[&str]>::len(&[$(stringify!($arg)),*]),
        );
        if $level <= __ticklog_max_level!() {
            // Arg count is known at compile time: the same value that
            // check_fmt validates.
            const __N_ARGS: u8 =
                <[&str]>::len(&[$(stringify!($arg)),*]) as u8;
            const __BASE: usize = $crate::__private::BASE_RECORD_SIZE;
            // Evaluate each argument expression exactly once, into a cons-list of
            // references. Every later use reads these bindings, so a
            // side-effecting argument runs once rather than once per use.
            let __args = $crate::__ticklog_cons!($($arg),+);
            let __total_size: usize = __BASE
                .wrapping_add(__N_ARGS as usize)
                .wrapping_add($crate::__private::LoggableArgs::args_encoded_size(&__args));
            $crate::__private::dispatch(
                $level, $fmt, file!(), line!(), __N_ARGS, __total_size,
                __ticklog_backpressure!(),
                |__buf: &mut [u8]| {
                    // Tags fill buf[0..n_args]; payloads follow.
                    let mut __tag: usize = 0;
                    let mut __pay: usize = __N_ARGS as usize;
                    $crate::__private::LoggableArgs::write_args(
                        &__args, __buf, &mut __tag, &mut __pay,
                    );
                },
            );
        }
    }};
}

/// Builds a cons-list of references to the given expressions, evaluating each
/// exactly once: `__ticklog_cons!(a, b)` expands to `(&a, (&b, ()))`.
///
/// Not part of the public API; an implementation detail of the logging macros.
#[doc(hidden)]
#[macro_export]
macro_rules! __ticklog_cons {
    () => { () };
    ($head:expr $(, $tail:expr)*) => {
        (&$head, $crate::__ticklog_cons!($($tail),*))
    };
}

/// Logs a message at [`Level::Trace`].
///
/// Takes a format string literal and positional arguments, e.g.
/// `trace!("state = {}", state)`. The record is discarded unless trace-level
/// logging is enabled.
#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => { $crate::__ticklog_log!($crate::Level::Trace, $($arg)*) };
}

/// Logs a message at [`Level::Debug`].
///
/// Takes a format string literal and positional arguments, e.g.
/// `debug!("value = {}", value)`.
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => { $crate::__ticklog_log!($crate::Level::Debug, $($arg)*) };
}

/// Logs a message at [`Level::Info`].
///
/// Takes a format string literal and positional arguments, e.g.
/// `info!("listening on {}", port)`.
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::__ticklog_log!($crate::Level::Info, $($arg)*) };
}

/// Logs a message at [`Level::Warn`].
///
/// Takes a format string literal and positional arguments, e.g.
/// `warn!("retry {} of {}", n, max)`.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::__ticklog_log!($crate::Level::Warn, $($arg)*) };
}

/// Logs a message at [`Level::Error`].
///
/// Takes a format string literal and positional arguments, e.g.
/// `error!("connection failed: {}", err)`.
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::__ticklog_log!($crate::Level::Error, $($arg)*) };
}
