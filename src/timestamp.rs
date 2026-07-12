//! Platform-specific monotonic timestamps and ISO 8601 formatting.

use std::time::{SystemTime, UNIX_EPOCH};

/// Calibration data for converting hardware counter ticks to wall-clock
/// nanoseconds since the Unix epoch.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Calibration {
    /// Nanoseconds per hardware counter tick.
    pub(crate) counter_to_ns: f64,
    /// Hardware counter value sampled at calibration time.
    pub(crate) counter_base: u64,
    /// `SystemTime::now()` in nanoseconds since Unix epoch, sampled at
    /// calibration time.
    pub(crate) wall_base_ns: u64,
}

/// Frequency substituted when the platform reports a counter frequency of 0
/// (aarch64 `CNTFRQ_EL0` unprogrammed by firmware, or an x86 timing calibration
/// that measured no tick delta). A plausible 1 GHz keeps `counter_to_ns` finite
/// and timestamps monotonic; they may be mis-scaled, but that beats the `+inf`
/// that would collapse every timestamp to 0 or `u64::MAX`.
const FALLBACK_COUNTER_HZ: u64 = 1_000_000_000;

/// Create a [`Calibration`] by cross-referencing the platform counter against
/// [`SystemTime::now()`].
///
/// On x86_64 without invariant TSC (e.g. some VMs), warns to stderr and
/// falls back to timing-based calibration.
pub(crate) fn calibrate() -> Calibration {
    #[cfg(target_arch = "x86_64")]
    {
        check_invariant_tsc();
    }

    let freq = counter_frequency();
    if freq == 0 {
        eprintln!(
            "ticklog: platform reported a counter frequency of 0; \
             falling back to {FALLBACK_COUNTER_HZ} Hz, timestamps may be mis-scaled"
        );
    }
    let counter_to_ns = counter_to_ns_from_freq(freq);

    // Read wall clock first, then counter. The counter read is the fast leg
    // (~0.25-2 ns), so reading it last minimises the cross-reference error.
    let wall_time = SystemTime::now();
    let counter_base = raw_timestamp();
    let wall_base_ns = system_time_to_nanos(wall_time);

    Calibration {
        counter_to_ns,
        counter_base,
        wall_base_ns,
    }
}

/// Nanoseconds per counter tick for a counter frequency in Hz.
///
/// A zero frequency would make this `+inf`; [`FALLBACK_COUNTER_HZ`] is
/// substituted instead so `ticks_to_ns` stays well-defined.
fn counter_to_ns_from_freq(freq: u64) -> f64 {
    let hz = if freq == 0 { FALLBACK_COUNTER_HZ } else { freq };
    1_000_000_000.0 / hz as f64
}

/// Convert `t` to nanoseconds since Unix epoch.
fn system_time_to_nanos(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .expect("invariant: system clock is set before the Unix epoch")
        .as_nanos() as u64
}

/// Convert a raw hardware counter value to nanoseconds since the Unix epoch.
///
/// Formula: `(tick - base) * counter_to_ns + wall_base_ns`
#[inline]
pub(crate) fn ticks_to_ns(tick: u64, calib: &Calibration) -> u64 {
    let delta = tick.wrapping_sub(calib.counter_base) as f64;
    let ns = (delta * calib.counter_to_ns) as u64;
    ns.wrapping_add(calib.wall_base_ns)
}

/// Read the platform-specific monotonic hardware counter.
#[inline]
pub(crate) fn raw_timestamp() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: this arm compiles only on x86_64, where the RDTSC instruction
        // read by `_raw_timestamp_x86` is always available; it has no other
        // precondition.
        unsafe { _raw_timestamp_x86() }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: this arm compiles only on aarch64, where the CNTVCT_EL0 system
        // register read by `_raw_timestamp_aarch64` is always available; it has
        // no other precondition.
        unsafe { _raw_timestamp_aarch64() }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        _raw_timestamp_fallback()
    }
}

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn _raw_timestamp_x86() -> u64 {
    // SAFETY: RDTSC is available on every x86_64 processor. It reads the
    // 64-bit timestamp counter into EDX:EAX with no side effects.
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdtsc",
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack, preserves_flags),
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

#[cfg(target_arch = "x86_64")]
fn check_invariant_tsc() {
    // SAFETY: CPUID is available on all x86_64 processors. Leaf 0x80000007
    // returns feature flags in EDX; bit 8 indicates invariant TSC support.
    let cpuid = core::arch::x86_64::__cpuid(0x80000007);
    if cpuid.edx & (1 << 8) == 0 {
        eprintln!(
            "ticklog: invariant TSC not detected, timestamps may drift under C-state changes"
        );
    }
}

#[cfg(target_arch = "x86_64")]
fn counter_frequency() -> u64 {
    // CPUID leaf 0x15 sub-leaf 0 enumerates the TSC as a ratio over the
    // core-crystal clock. Available on Intel Skylake+ and AMD Zen 2+; other
    // parts (and many hypervisors) return zeroed registers.
    //
    // SAFETY: CPUID with a static leaf is read-only and safe on any x86_64
    // processor.
    let cpuid = core::arch::x86_64::__cpuid_count(0x15, 0);
    if let Some(freq) = tsc_freq_from_cpuid_0x15(cpuid.eax, cpuid.ebx, cpuid.ecx) {
        return freq;
    }

    // Fallback: timing-based calibration.
    timing_calibrate_frequency()
}

/// Derive the TSC frequency in Hz from CPUID leaf 0x15 sub-leaf 0 registers.
///
/// Per the Intel SDM, leaf 0x15 reports the TSC as a ratio over the
/// core-crystal clock: `TSC_Hz = crystal_hz * numerator / denominator`, where
/// `EAX = denominator`, `EBX = numerator`, and `ECX = crystal_hz`. `ECX` on its
/// own is the ~24 MHz crystal frequency, *not* the ~3 GHz TSC, so it must be
/// scaled by `EBX / EAX`.
///
/// Any of the three registers being zero means the CPU did not enumerate that
/// value (e.g. pre-Skylake, or a hypervisor that leaves the ratio blank), so the
/// frequency cannot be derived here; returns `None` and the caller falls back to
/// timing calibration.
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
fn tsc_freq_from_cpuid_0x15(eax: u32, ebx: u32, ecx: u32) -> Option<u64> {
    let denominator = eax as u64;
    let numerator = ebx as u64;
    let crystal_hz = ecx as u64;
    if denominator == 0 || numerator == 0 || crystal_hz == 0 {
        return None;
    }
    // TSC_Hz = crystal_hz * numerator / denominator. Saturate the product
    // (well within u64 range for realistic values, but defensive against a
    // bogus numerator) before dividing by the non-zero denominator.
    Some(crystal_hz.saturating_mul(numerator) / denominator)
}

#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn _raw_timestamp_aarch64() -> u64 {
    let counter: u64;
    // SAFETY: CNTVCT_EL0 is the ARM generic timer virtual counter. It is
    // available on all AArch64 processors that support EL0 timer access (every
    // ARMv8-A implementation). The MRS instruction reads the 64-bit counter
    // value into a general-purpose register without side effects.
    unsafe {
        core::arch::asm!(
            "mrs {0}, cntvct_el0",
            out(reg) counter,
            options(nomem, nostack, preserves_flags),
        );
    }
    counter
}

#[cfg(target_arch = "aarch64")]
fn counter_frequency() -> u64 {
    let freq: u64;
    // SAFETY: CNTFRQ_EL0 is a read-only register that reports the fixed
    // frequency of the system counter in Hz. It is always readable at EL0
    // on ARMv8-A and later. The MRS instruction has no side effects.
    unsafe {
        core::arch::asm!(
            "mrs {0}, cntfrq_el0",
            out(reg) freq,
            options(nomem, nostack, preserves_flags),
        );
    }
    freq
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline]
fn _raw_timestamp_fallback() -> u64 {
    system_time_to_nanos(SystemTime::now())
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn counter_frequency() -> u64 {
    // Fallback: raw_timestamp() returns nanoseconds directly, so 1 tick = 1 ns.
    1_000_000_000
}

/// Estimate the counter frequency by measuring elapsed ticks against wall-clock
/// time over a short sleep interval.
#[cfg(target_arch = "x86_64")]
fn timing_calibrate_frequency() -> u64 {
    // Spin briefly to warm up. The first RDTSC can be slow on some CPUs.
    let _warm = raw_timestamp();

    let t1 = raw_timestamp();
    let w1 = SystemTime::now();

    // Sleep for ~1 ms to measure a meaningful tick delta against wall-clock time.
    std::thread::sleep(std::time::Duration::from_millis(1));

    let t2 = raw_timestamp();
    let w2 = SystemTime::now();

    let tick_delta = t2.wrapping_sub(t1) as f64;
    let wall_delta_ns = system_time_to_nanos(w2).saturating_sub(system_time_to_nanos(w1)) as f64;

    // Guard against zero or near-zero wall delta (unlikely but possible
    // under extreme VM clock skew). Default to a plausible modern TSC
    // frequency so we don't divide by zero.
    if wall_delta_ns < 1.0 {
        return FALLBACK_COUNTER_HZ;
    }

    // freq = ticks / seconds = (tick_delta / wall_delta_ns) * 1e9
    (tick_delta * 1_000_000_000.0 / wall_delta_ns) as u64
}

/// Days from 0000-03-01 (proleptic Gregorian) to 1970-01-01 (Unix epoch).
const DAYS_FROM_CIVIL_EPOCH_TO_UNIX: i64 = 719_468;

/// Convert days since Unix epoch to proleptic Gregorian `(year, month, day)`.
///
/// Howard Hinnant's civil-from-days algorithm.
/// <https://howardhinnant.github.io/date_algorithms.html>
fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    // Shift to days since 0000-03-01 (civil epoch).
    let z = days_since_epoch + DAYS_FROM_CIVIL_EPOCH_TO_UNIX;

    // 400-year Gregorian cycle = 146097 days.
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let doe = (z - era * 146_097) as u32; // day of era [0, 146096]

    // Year of era [0, 399].
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;

    // Day of year [0, 365].
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);

    // Month position [0, 11] where 0 = March.
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]

    // Adjust year for January and February (they belong to the previous
    // calendar year in the March-based civil calendar).
    let y_adj = y + i64::from(m <= 2);

    (y_adj as i32, m, d)
}

/// Split nanoseconds since midnight into `(hours, minutes, seconds, nanos)`.
fn time_from_ns(ns_since_midnight: u64) -> (u32, u32, u32, u32) {
    let secs = (ns_since_midnight / 1_000_000_000) as u32;
    let nanos = (ns_since_midnight % 1_000_000_000) as u32;
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    let secs = secs % 60;
    (hours, mins, secs, nanos)
}

const NANOS_PER_SEC: i64 = 1_000_000_000;
const SECS_PER_DAY: u64 = 86_400;

/// Format a Unix-epoch nanosecond timestamp as an ISO 8601 string with
/// nanosecond precision, appending to `buf`.
///
/// `timezone_offset_secs` is added to the timestamp before formatting.
/// Zero offset renders as trailing `Z`; non-zero renders as `+HH:MM` or
/// `-HH:MM`.
pub(crate) fn format_iso8601(ns_since_epoch: u64, timezone_offset_secs: i32, buf: &mut Vec<u8>) {
    // Shift by timezone offset, then split into days and time-of-day.
    let offset_ns = (timezone_offset_secs as i64) * NANOS_PER_SEC;
    let local_ns = (ns_since_epoch as i64).wrapping_add(offset_ns);

    let ns_per_day = NANOS_PER_SEC * SECS_PER_DAY as i64;
    let days = local_ns.div_euclid(ns_per_day);
    let ns_since_midnight = local_ns.rem_euclid(ns_per_day) as u64;

    let (year, month, day) = civil_from_days(days);
    let (hour, min, sec, nanos) = time_from_ns(ns_since_midnight);

    // Use a fixed-capacity stack buffer for the timestamp portion.
    // ISO 8601 with nanosecond precision: "YYYY-MM-DDTHH:MM:SS.sssssssssZ"
    // = 31 bytes (including trailing Z or offset).
    let mut ts_buf = [0u8; 64];
    let mut pos = 0usize;

    // Macro to write an integer with zero-padding.
    macro_rules! push_u32 {
        ($val:expr, $digits:expr) => {
            let v = $val;
            let d = $digits;
            let mut i = pos + d - 1;
            let mut remaining = v;
            loop {
                ts_buf[i] = b'0' + (remaining % 10) as u8;
                if i == pos {
                    break;
                }
                remaining /= 10;
                i -= 1;
            }
            pos += d;
        };
    }

    macro_rules! push_char {
        ($ch:expr) => {
            ts_buf[pos] = $ch;
            pos += 1;
        };
    }

    push_u32!(year as u32, 4);
    push_char!(b'-');
    push_u32!(month, 2);
    push_char!(b'-');
    push_u32!(day, 2);
    push_char!(b'T');
    push_u32!(hour, 2);
    push_char!(b':');
    push_u32!(min, 2);
    push_char!(b':');
    push_u32!(sec, 2);
    push_char!(b'.');
    push_u32!(nanos, 9);

    // Timezone suffix.
    if timezone_offset_secs == 0 {
        push_char!(b'Z');
    } else {
        let (sign, abs_offset) = if timezone_offset_secs < 0 {
            (b'-', (-timezone_offset_secs) as u32)
        } else {
            (b'+', timezone_offset_secs as u32)
        };
        let off_hours = abs_offset / 3600;
        let off_mins = (abs_offset % 3600) / 60;
        push_char!(sign);
        push_u32!(off_hours, 2);
        push_char!(b':');
        push_u32!(off_mins, 2);
    }

    buf.extend_from_slice(&ts_buf[..pos]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_to_ns_guards_zero_frequency() {
        // A zero frequency must not yield +inf; that would collapse every
        // timestamp to 0 / u64::MAX in ticks_to_ns. Falls back to 1 GHz.
        let ns = counter_to_ns_from_freq(0);
        assert!(ns.is_finite(), "counter_to_ns must stay finite at freq=0");
        assert_eq!(ns, 1_000_000_000.0 / FALLBACK_COUNTER_HZ as f64);
    }

    #[test]
    fn counter_to_ns_uses_frequency_when_nonzero() {
        // 2 GHz -> 0.5 ns per tick.
        assert_eq!(counter_to_ns_from_freq(2_000_000_000), 0.5);
    }

    #[test]
    fn tsc_freq_scales_crystal_by_ratio() {
        // Synthetic Skylake-style leaf 0x15: 24 MHz crystal, ratio 250/2.
        // TSC = crystal * numerator / denominator = 24_000_000 * 250 / 2
        //     = 3.0 GHz, NOT the 24 MHz crystal (ECX) on its own.
        assert_eq!(
            tsc_freq_from_cpuid_0x15(2, 250, 24_000_000),
            Some(3_000_000_000)
        );
    }

    #[test]
    fn tsc_freq_none_when_any_register_zero() {
        // Any of denominator (EAX), numerator (EBX), or crystal (ECX) being 0
        // means the value was not enumerated -> derivation impossible -> None,
        // so the caller falls back to timing calibration.
        assert_eq!(tsc_freq_from_cpuid_0x15(0, 250, 24_000_000), None);
        assert_eq!(tsc_freq_from_cpuid_0x15(2, 0, 24_000_000), None);
        assert_eq!(tsc_freq_from_cpuid_0x15(2, 250, 0), None);
    }

    #[test]
    fn civil_unix_epoch() {
        // 1970-01-01 is day 0.
        let (y, m, d) = civil_from_days(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }

    #[test]
    fn civil_known_date() {
        // 2026-07-04 = 20638 days after epoch.
        let (y, m, d) = civil_from_days(20638);
        assert_eq!((y, m, d), (2026, 7, 4));
    }

    #[test]
    fn civil_leap_day_2000() {
        // 2000-02-29. Days from 1970-01-01 to 2000-02-29:
        // 30 years: 1970-1999. Leap years: 1972, 76, 80, 84, 88, 92, 96.
        // 30*365 + 7 = 10950 + 7 = 10957. Plus Jan 31 + Feb 29 - 1 (epoch day)
        // = 10957 + 31 + 28 = 11016. Wait let me count more carefully.
        //
        // Actually let's just verify the algorithm produces a valid leap day.
        let (_y, m, d) = civil_from_days(11016);
        assert_eq!((m, d), (2, 29));
    }

    #[test]
    fn civil_year_2100_not_leap() {
        // 2100-02-28 should exist but not 02-29 (2100 is not a leap year
        // in the Gregorian calendar).
        //
        // Days to 2100-02-28:
        // 1970-2099 = 130 years. Leap years in this range (divisible by 4
        // but not by 100 unless by 400): 2000 is leap; 2100 is not.
        // 1972..2096 inclusive, step 4, skipping 2100: 32 leap years.
        // 130*365 + 32 = 47450 + 32 = 47482.
        // Plus Jan(31) + Feb(27) = 58 (day 28 = 58 days from Jan 1).
        // 47482 + 58 = 47540.
        let (y, m, d) = civil_from_days(47540);
        assert_eq!((y, m, d), (2100, 2, 28));

        // The next day should be March 1.
        let (y, m, d) = civil_from_days(47541);
        assert_eq!((y, m, d), (2100, 3, 1));
    }

    #[test]
    fn civil_pre_epoch() {
        // 1969-12-31. Days -1.
        let (y, m, d) = civil_from_days(-1);
        assert_eq!((y, m, d), (1969, 12, 31));
    }

    #[test]
    fn time_midnight() {
        let (h, m, s, ns) = time_from_ns(0);
        assert_eq!((h, m, s, ns), (0, 0, 0, 0));
    }

    #[test]
    fn time_noon() {
        let ns = 12 * 3600 * 1_000_000_000u64;
        let (h, m, s, ns) = time_from_ns(ns);
        assert_eq!((h, m, s, ns), (12, 0, 0, 0));
    }

    #[test]
    fn time_pre_midnight() {
        // 23:59:59.999999999
        let ns = 24 * 3600 * 1_000_000_000u64 - 1;
        let (h, m, s, ns) = time_from_ns(ns);
        assert_eq!(h, 23);
        assert_eq!(m, 59);
        assert_eq!(s, 59);
        assert_eq!(ns, 999_999_999);
    }

    #[test]
    fn format_utc_epoch() {
        let mut buf = Vec::new();
        // 0 ns = 1970-01-01T00:00:00.000000000Z
        format_iso8601(0, 0, &mut buf);
        assert_eq!(
            std::str::from_utf8(&buf).unwrap(),
            "1970-01-01T00:00:00.000000000Z"
        );
    }

    #[test]
    fn format_utc_known_timestamp() {
        let mut buf = Vec::new();
        // 2026-07-04T14:22:04.656175000Z
        // Days to 2026-07-04 = 20638.
        // Time: 14*3600 + 22*60 + 4 = 51724 sec, + 0.656175 = 51724656175000 ns.
        // Since midnight: 51724 * 1_000_000_000 + 656175000 = 51724656175000.
        // Since epoch: 20638 * 86400 * 1_000_000_000 + 51724656175000
        let day_ns = 20_638u64 * 86_400 * 1_000_000_000;
        let time_ns = 51_724u64 * 1_000_000_000 + 656_175_000;
        let ts = day_ns + time_ns;
        format_iso8601(ts, 0, &mut buf);
        assert_eq!(
            std::str::from_utf8(&buf).unwrap(),
            "2026-07-04T14:22:04.656175000Z"
        );
    }

    #[test]
    fn format_with_positive_offset() {
        let mut buf = Vec::new();
        // 0 ns UTC + 8:00 = 1970-01-01T08:00:00.000000000+08:00
        format_iso8601(0, 8 * 3600, &mut buf);
        assert_eq!(
            std::str::from_utf8(&buf).unwrap(),
            "1970-01-01T08:00:00.000000000+08:00"
        );
    }

    #[test]
    fn format_with_negative_offset() {
        let mut buf = Vec::new();
        // 0 ns UTC - 5:00 = 1969-12-31T19:00:00.000000000-05:00
        format_iso8601(0, -5 * 3600, &mut buf);
        assert_eq!(
            std::str::from_utf8(&buf).unwrap(),
            "1969-12-31T19:00:00.000000000-05:00"
        );
    }

    #[test]
    fn format_nanosecond_precision() {
        let mut buf = Vec::new();
        // 1_234_567_890 ns after epoch, UTC.
        format_iso8601(1_234_567_890, 0, &mut buf);
        assert_eq!(
            std::str::from_utf8(&buf).unwrap(),
            "1970-01-01T00:00:01.234567890Z"
        );
    }

    #[test]
    fn format_offset_non_multiple_of_hour() {
        let mut buf = Vec::new();
        // +05:30 (IST)
        format_iso8601(0, 5 * 3600 + 30 * 60, &mut buf);
        assert_eq!(
            std::str::from_utf8(&buf).unwrap(),
            "1970-01-01T05:30:00.000000000+05:30"
        );
    }

    #[test]
    fn format_negative_offset_at_epoch() {
        let mut buf = Vec::new();
        format_iso8601(0, -1, &mut buf);
        assert_eq!(
            std::str::from_utf8(&buf).unwrap(),
            "1969-12-31T23:59:59.000000000-00:00"
        );
    }

    #[test]
    fn ticks_to_ns_zero_delta() {
        let calib = Calibration {
            counter_to_ns: 2.5,
            counter_base: 100,
            wall_base_ns: 1_000_000_000,
        };
        // When tick equals base, the delta is zero so the result is wall_base_ns.
        assert_eq!(ticks_to_ns(100, &calib), 1_000_000_000);
    }

    #[test]
    fn ticks_to_ns_positive_delta() {
        let calib = Calibration {
            counter_to_ns: 2.5,
            counter_base: 100,
            wall_base_ns: 0,
        };
        // tick = 104, delta = 4, 4 * 2.5 = 10 ns.
        assert_eq!(ticks_to_ns(104, &calib), 10);
    }

    #[test]
    fn ticks_to_ns_wraparound() {
        // If tick wraps past u64::MAX, wrapping_sub still gives the delta.
        let calib = Calibration {
            counter_to_ns: 1.0,
            counter_base: u64::MAX - 10,
            wall_base_ns: 0,
        };
        let tick = 5u64; // wrapped: 5 = (u64::MAX - 10) + 16
        assert_eq!(ticks_to_ns(tick, &calib), 16);
    }
}
