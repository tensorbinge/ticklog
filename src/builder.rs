//! Initialization of the logging system.
//!
//! [`Builder`] collects the sink and formatting options; [`Builder::build`]
//! claims the global ring registry, spawns the drain thread, and returns a
//! [`Guard`] that shuts the drain down on drop. The global level ceiling and
//! backpressure policy that the logging macros consult also live here, set once
//! by `build()`.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::affinity;
use crate::drain::{Drain, LogMetadata};
use crate::error::TicklogError;
use crate::guard::Guard;
use crate::level::Level;
use crate::sink::{ConsoleSink, LogSink};
use crate::thread_buf::REGISTRY;
use crate::timestamp;

/// Minimum valid timezone offset in seconds east of UTC (UTC-12:00).
const MIN_TZ_OFFSET: i32 = -43_200;
/// Maximum valid timezone offset in seconds east of UTC (UTC+14:00).
const MAX_TZ_OFFSET: i32 = 50_400;

/// Global level ceiling consulted by the logging macros. Starts at 0 so every
/// record is discarded until `build()` raises it; a record whose level has a
/// numeric value above the ceiling is dropped before the ring write.
static MAX_LEVEL: AtomicU8 = AtomicU8::new(0);

/// Global backpressure policy consulted by the logging macros when a ring is
/// full. Stored as the `Backpressure` discriminant; 0 == `Backpressure::Drop`.
static BACKPRESSURE: AtomicU8 = AtomicU8::new(0);

/// Sets the global level ceiling. Called once by [`Builder::build`].
///
/// Stored Relaxed: it is written once at startup, after the registry and drain
/// are ready, and read Relaxed by the macros on the hot path.
fn set_max_level(level: Level) {
    MAX_LEVEL.store(level.to_u8(), Ordering::Relaxed);
}

/// Lowers the global level ceiling back to 0 (discard everything). Called by
/// [`Guard`]'s drop after the drain thread is gone, so producers short-circuit
/// before writing into a ring no drain will consume.
///
/// Stored Relaxed, mirroring [`set_max_level`]: the guard's own join provides
/// the ordering a caller needs to observe the drain has stopped.
pub(crate) fn disable_logging() {
    MAX_LEVEL.store(0, Ordering::Relaxed);
}

/// Sets the global backpressure policy. Called once by [`Builder::build`].
fn set_backpressure(policy: Backpressure) {
    BACKPRESSURE.store(policy as u8, Ordering::Relaxed);
}

/// Returns whether a record at `level` passes the global level ceiling.
///
/// A record is emitted when its numeric level is at or below the ceiling; the
/// ceiling starts at 0, so every record is discarded until [`Builder::build`]
/// raises it. Read Relaxed on the hot path; the ceiling changes once, at
/// startup, before any producer can pass this check.
pub(crate) fn level_enabled(level: Level) -> bool {
    level.to_u8() <= MAX_LEVEL.load(Ordering::Relaxed)
}

/// Returns the configured policy for a full ring buffer. Read Relaxed on the
/// hot path; set once by [`Builder::build`].
pub(crate) fn backpressure() -> Backpressure {
    if BACKPRESSURE.load(Ordering::Relaxed) == Backpressure::Block as u8 {
        Backpressure::Block
    } else {
        Backpressure::Drop
    }
}

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

/// Collects configuration for the logging system. Create one with [`builder`],
/// then call [`Builder::build`].
pub struct Builder {
    sink: Box<dyn LogSink>,
    max_level: Level,
    backpressure: Backpressure,
    timezone_offset: i32,
    drain_affinity: Option<Vec<usize>>,
}

/// Creates a [`Builder`] with default configuration.
///
/// The sink defaults to a [`ConsoleSink`] writing to stderr, the conventional
/// logging stream; it colors output by level when stderr is a terminal and
/// stays plain when redirected. Override it with [`Builder::sink`]. Other
/// defaults: level ceiling [`Level::Info`], [`Backpressure::Drop`], and UTC
/// timestamps.
pub fn builder() -> Builder {
    Builder {
        sink: Box::new(ConsoleSink::stderr()),
        max_level: Level::Info,
        backpressure: Backpressure::Drop,
        timezone_offset: 0,
        drain_affinity: None,
    }
}

impl Default for Builder {
    /// Same as [`builder`].
    fn default() -> Self {
        builder()
    }
}

impl Builder {
    /// Overrides the output sink. Defaults to a [`ConsoleSink`] on stderr.
    pub fn sink<S: LogSink>(mut self, sink: S) -> Self {
        self.sink = Box::new(sink);
        self
    }

    /// Sets the maximum log level. More verbose records are discarded on the
    /// calling thread before they cost anything. Default: [`Level::Info`].
    pub fn max_level(mut self, level: Level) -> Self {
        self.max_level = level;
        self
    }

    /// Sets what happens when a logging thread's buffer is full. Default:
    /// [`Backpressure::Drop`].
    pub fn backpressure(mut self, policy: Backpressure) -> Self {
        self.backpressure = policy;
        self
    }

    /// Sets the timezone offset in seconds east of UTC, applied to timestamp
    /// formatting only. Default: 0 (UTC).
    pub fn timezone_offset(mut self, seconds: i32) -> Self {
        self.timezone_offset = seconds;
        self
    }

    /// Pins the drain thread to the given set of logical CPUs.
    ///
    /// An empty slice is a no-op. On Linux every core in the set is included
    /// in the CPU affinity mask; on macOS only the first core is used (as a
    /// Mach affinity tag). Default: no affinity (the OS migrates the drain
    /// freely).
    ///
    /// When multiple drain threads are supported (in the future) all of them
    /// will share this affinity set.
    pub fn drain_affinity(mut self, cores: &[usize]) -> Self {
        if cores.is_empty() {
            self.drain_affinity = None;
        } else {
            self.drain_affinity = Some(cores.to_vec());
        }
        self
    }

    /// Initializes the logging system and returns a [`Guard`]. Dropping the
    /// guard shuts logging down. May be called only once per process.
    ///
    /// # Errors
    ///
    /// Returns [`TicklogError::InvalidTimezoneOffset`] if the configured offset
    /// is outside `[-43200, 50400]` seconds, [`TicklogError::AlreadyInitialized`]
    /// if `build()` has already run in this process, or
    /// [`TicklogError::DrainSpawnFailed`] if the drain thread cannot be spawned.
    pub fn build(self) -> Result<Guard, TicklogError> {
        // Reject an out-of-range timezone offset before claiming any global
        // state, so a bad config never consumes the one-shot registry.
        if !(MIN_TZ_OFFSET..=MAX_TZ_OFFSET).contains(&self.timezone_offset) {
            return Err(TicklogError::InvalidTimezoneOffset(self.timezone_offset));
        }

        // Claim the registry. OnceLock::set is atomic: the first caller wins,
        // and any later call gets Err here and returns before spawning a drain.
        REGISTRY
            .set(Mutex::new(Vec::new()))
            .map_err(|_| TicklogError::AlreadyInitialized)?;

        // Sample the counter-to-wall-clock mapping once, up front.
        let calibration = timestamp::calibrate();

        // Shared shutdown flag: the Guard sets it on drop; the drain reads it.
        let shutdown = Arc::new(AtomicBool::new(false));
        let drain = Drain::new(
            self.sink,
            self.timezone_offset,
            LogMetadata::default(),
            Arc::clone(&shutdown),
            calibration,
        );

        let drain_affinity = self.drain_affinity.clone();
        let handle = thread::Builder::new()
            .name("ticklog-drain".to_string())
            .spawn(move || {
                if let Some(ref cores) = drain_affinity {
                    affinity::pin_thread(cores);
                }
                let mut drain = drain;
                drain.run();
            })
            .map_err(TicklogError::DrainSpawnFailed)?;

        // Enable logging only now that the drain is running. Had the spawn
        // above failed, MAX_LEVEL would stay 0 and every macro would discard,
        // so records never accumulate in a ring no drain will consume. Setting
        // these last in program order also means the registry and drain are
        // ready before any producer can pass the level check.
        set_max_level(self.max_level);
        set_backpressure(self.backpressure);

        Ok(Guard::new(handle, shutdown))
    }
}

/// Starts ticklog with a custom sink and default configuration. Equivalent to
/// `builder().sink(sink).build()`.
///
/// # Errors
///
/// Returns [`TicklogError::AlreadyInitialized`] if `build()` or `init()` has
/// already run, or [`TicklogError::DrainSpawnFailed`] if the drain thread
/// cannot be spawned.
pub fn init<S: LogSink>(sink: S) -> Result<Guard, TicklogError> {
    builder().sink(sink).build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_has_expected_defaults() {
        let b = builder();
        assert!(b.max_level == Level::Info);
        assert_eq!(b.backpressure, Backpressure::Drop);
        assert_eq!(b.timezone_offset, 0);
    }

    #[test]
    fn fluent_setters_update_fields() {
        let b = builder()
            .max_level(Level::Warn)
            .backpressure(Backpressure::Block)
            .timezone_offset(3600)
            .drain_affinity(&[2]);
        assert!(b.max_level == Level::Warn);
        assert_eq!(b.backpressure, Backpressure::Block);
        assert_eq!(b.timezone_offset, 3600);
        assert_eq!(b.drain_affinity.as_deref(), Some(&[2usize][..]));
    }

    #[test]
    fn drain_affinity_empty_is_none() {
        let b = builder().drain_affinity(&[2]).drain_affinity(&[]);
        assert!(b.drain_affinity.is_none());
    }

    #[test]
    fn backpressure_discriminants() {
        assert_eq!(Backpressure::Drop as u8, 0);
        assert_eq!(Backpressure::Block as u8, 1);
    }

    #[test]
    fn set_max_level_stores_level_byte() {
        set_max_level(Level::Warn);
        assert_eq!(MAX_LEVEL.load(Ordering::Relaxed), Level::Warn.to_u8());
        // Restore the disabled default so other tests are unaffected.
        MAX_LEVEL.store(0, Ordering::Relaxed);
    }

    #[test]
    fn drop_guard_disables_logging_ceiling() {
        // Raise the ceiling as build() would, so producers pass the level gate.
        set_max_level(Level::Info);
        assert!(level_enabled(Level::Info));

        // A stand-in drain thread that exits as soon as shutdown is signaled.
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let handle = thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
        });

        let guard = Guard::new(handle, shutdown);
        drop(guard);

        // After the guard drops the drain is gone; logging must short-circuit so
        // producers can't spin forever writing into an undrained ring.
        assert!(!level_enabled(Level::Info));
        assert!(!level_enabled(Level::Error));

        // Restore the disabled default so other tests are unaffected.
        MAX_LEVEL.store(0, Ordering::Relaxed);
    }

    #[test]
    fn set_backpressure_stores_discriminant() {
        set_backpressure(Backpressure::Block);
        assert_eq!(
            BACKPRESSURE.load(Ordering::Relaxed),
            Backpressure::Block as u8
        );
        set_backpressure(Backpressure::Drop);
        assert_eq!(
            BACKPRESSURE.load(Ordering::Relaxed),
            Backpressure::Drop as u8
        );
    }

    #[test]
    fn build_after_registry_claimed_is_already_initialized() {
        // Ensure the registry is claimed (by us or an earlier test), then a
        // fresh build() must observe it and fail before spawning a drain.
        let _ = REGISTRY.set(Mutex::new(Vec::new()));
        let result = builder().build();
        assert!(matches!(result, Err(TicklogError::AlreadyInitialized)));
    }

    #[test]
    fn build_rejects_out_of_range_timezone_offset() {
        // Just past UTC+14:00 and UTC-12:00. Validation runs before the registry
        // is claimed, so these never spawn a drain; safe in the shared process.
        assert!(matches!(
            builder().timezone_offset(50_401).build(),
            Err(TicklogError::InvalidTimezoneOffset(50_401))
        ));
        assert!(matches!(
            builder().timezone_offset(-43_201).build(),
            Err(TicklogError::InvalidTimezoneOffset(-43_201))
        ));
    }
}
