//! Cross-language benchmark harness for ticklog.
//!
//! Self-measures call-site latency using the same hardware counter as every
//! other candidate (RDTSC on x86_64, CNTVCT_EL0 on aarch64). Reads
//! `--ns-per-tick` from the pre-calibration step and writes per-configuration
//! percentiles plus throughput as JSON to `--output`.

use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process;
use std::sync::{Arc, Barrier};
use std::thread;
use serde::Serialize;
use ticklog::{info, Level, LogSink};

// Constants (must match the design doc)
/// Number of log calls between counter reads.
const BATCH: usize = 1000;

/// Number of batch-average samples per (workload, thread_count) config.
const SAMPLES: usize = 10_000;

/// Total log messages per config: SAMPLES * BATCH.
const TOTAL_MESSAGES: u64 = (SAMPLES * BATCH) as u64;

// Platform counter
/// Read the platform-specific monotonic hardware counter.
///
/// On x86_64: `RDTSC` (invariant TSC).
/// On aarch64: `CNTVCT_EL0` (ARM Generic Timer).
/// Fallback: `clock_gettime(CLOCK_MONOTONIC)` via libc.
#[inline]
fn read_counter() -> u64 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        _read_counter_x86()
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        _read_counter_aarch64()
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        _read_counter_fallback()
    }
}

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn _read_counter_x86() -> u64 {
    let lo: u32;
    let hi: u32;
    // SAFETY: RDTSC is available on every x86_64 processor. It reads the
    // 64-bit timestamp counter into EDX:EAX with no side effects.
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

#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn _read_counter_aarch64() -> u64 {
    let counter: u64;
    // SAFETY: CNTVCT_EL0 is the ARM Generic Timer virtual counter. It is
    // available at EL0 on every ARMv8-A implementation. MRS reads the
    // 64-bit counter with no side effects.
    unsafe {
        core::arch::asm!(
            "mrs {0}, cntvct_el0",
            out(reg) counter,
            options(nomem, nostack, preserves_flags),
        );
    }
    counter
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("cross-lang bench requires x86_64 or aarch64");

// Null sink
struct NullSink;

impl LogSink for NullSink {
    fn accept(&mut self, _line: &[u8], _level: Level) -> io::Result<()> {
        Ok(())
    }
}

// Workload definitions
/// Identifies a workload shape.
#[derive(Clone, Copy)]
enum Workload {
    SingleInt,
    Mixed,
    String_,
}

impl Workload {
    fn all() -> &'static [Workload] {
        &[Workload::SingleInt, Workload::Mixed, Workload::String_]
    }

    fn name(&self) -> &'static str {
        match self {
            Workload::SingleInt => "single_int",
            Workload::Mixed => "mixed",
            Workload::String_ => "string",
        }
    }

    /// Execute one batch of BATCH log calls. `call_index` varies across
    /// the run so the compiler cannot constant-fold the log site.
    fn run_batch(&self, call_index: u64) {
        match self {
            Workload::SingleInt => self.batch_single_int(call_index),
            Workload::Mixed => self.batch_mixed(),
            Workload::String_ => self.batch_string(),
        }
    }

    fn batch_single_int(&self, call_index: u64) {
        for i in 0..BATCH {
            let v = call_index * (BATCH as u64) + i as u64;
            info!("x={}", std::hint::black_box(v));
        }
    }

    fn batch_mixed(&self) {
        for _ in 0..BATCH {
            info!(
                "{} {} {}",
                std::hint::black_box(42u64),
                std::hint::black_box(3.14159f64),
                std::hint::black_box("hello world"),
            );
        }
    }

    fn batch_string(&self) {
        for _ in 0..BATCH {
            info!("{}", std::hint::black_box("hello world"));
        }
    }
}

// Percentiles
/// Nearest-rank percentile. `p` in (0, 1], e.g. 0.50 for p50.
/// `samples` must be sorted ascending and non-empty.
fn percentile(samples: &[f64], p: f64) -> f64 {
    assert!(!samples.is_empty() && p > 0.0 && p <= 1.0);
    let n = samples.len() as f64;
    let rank = (n * p).ceil() as usize;
    let idx = rank.saturating_sub(1).min(samples.len() - 1);
    samples[idx]
}

// Single-threaded measurement
/// Run one (workload, thread_count) configuration and return the
/// measured percentiles and throughput.
fn measure_config(cfg: &Config, workload: Workload, n_threads: usize) -> ConfigResult {
    let samples_per_thread = SAMPLES / n_threads;
    let barrier = Arc::new(Barrier::new(n_threads));
    let mut handles = Vec::with_capacity(n_threads);

    let wall_start = std::time::Instant::now();

    for thread_idx in 0..n_threads {
        let b = Arc::clone(&barrier);
        let wl = workload;
        let ns_per_tick = cfg.ns_per_tick;
        let producer_core = cfg.producer_core;

        handles.push(thread::spawn(move || {
            // Pin the producer before it touches the ring. With N threads the
            // cores fan out from the base so each producer owns one core.
            if let Some(base) = producer_core {
                ticklog::pin_thread(&[base + thread_idx]);
            }
            // Register this thread's ring buffer before measurement.
            let _ = ticklog::warm_up();
            b.wait();

            let mut latencies = Vec::with_capacity(samples_per_thread);

            for batch_i in 0..samples_per_thread {
                let call_index = (thread_idx * samples_per_thread + batch_i) as u64;

                let t0 = read_counter();
                wl.run_batch(call_index);
                let t1 = read_counter();

                let ticks = t1.wrapping_sub(t0);
                let ns = ticks as f64 * ns_per_tick;
                let per_call_ns = ns / BATCH as f64;
                latencies.push(per_call_ns);

            }

            latencies
        }));
    }

    // Collect per-thread latency vectors.
    let mut all_latencies = Vec::with_capacity(SAMPLES);
    for h in handles {
        match h.join() {
            Ok(v) => all_latencies.extend(v),
            Err(_) => {
                eprintln!("ticklog harness: thread panicked");
                process::exit(1);
            }
        }
    }

    let wall_duration_s = wall_start.elapsed().as_secs_f64();
    let throughput = TOTAL_MESSAGES as f64 / wall_duration_s;

    all_latencies.sort_by(|a, b| a.partial_cmp(b).expect("invariant: latency is finite"));

    let p50 = percentile(&all_latencies, 0.50);
    let p95 = percentile(&all_latencies, 0.95);
    let p99 = percentile(&all_latencies, 0.99);
    let p999 = percentile(&all_latencies, 0.999);
    let max = *all_latencies.last().expect("invariant: at least one sample");

    ConfigResult {
        workload: workload.name().to_string(),
        threads: n_threads,
        throughput: throughput.round() as u64,
        p50: round2(p50),
        p95: round2(p95),
        p99: round2(p99),
        p999: round2(p999),
        max: round2(max),
    }
}

/// Round to 2 decimal places for JSON output.
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

// CLI config
struct Config {
    ns_per_tick: f64,
    output: PathBuf,
    /// Thread counts to benchmark. Defaults to single-thread.
    thread_counts: Vec<usize>,
    /// Base core for producer threads. Thread `i` pins to `base + i`.
    /// `None` leaves producers unpinned.
    producer_core: Option<usize>,
    /// Core for the ticklog drain thread. `None` leaves it unpinned.
    backend_core: Option<usize>,
}

/// Parse a comma-separated core/thread list such as "1,2,4" into a Vec.
fn parse_usize_list(s: &str, flag: &str) -> Vec<usize> {
    s.split(',')
        .map(|part| {
            part.trim().parse::<usize>().unwrap_or_else(|_| {
                eprintln!("error: {flag} expects comma-separated integers, got '{s}'");
                process::exit(1);
            })
        })
        .collect()
}

fn parse_usize(s: &str, flag: &str) -> usize {
    s.parse::<usize>().unwrap_or_else(|_| {
        eprintln!("error: {flag} must be an integer, got '{s}'");
        process::exit(1);
    })
}

fn parse_args() -> Config {
    let args: Vec<String> = env::args().collect();
    let mut ns_per_tick = None;
    let mut output = None;
    let mut thread_counts = None;
    let mut producer_core = None;
    let mut backend_core = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--ns-per-tick" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --ns-per-tick requires a value");
                    process::exit(1);
                }
                ns_per_tick = Some(args[i].parse::<f64>().unwrap_or_else(|_| {
                    eprintln!("error: --ns-per-tick must be a float, got '{}'", args[i]);
                    process::exit(1);
                }));
            }
            "--output" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --output requires a path");
                    process::exit(1);
                }
                output = Some(PathBuf::from(&args[i]));
            }
            "--threads" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --threads requires a value");
                    process::exit(1);
                }
                thread_counts = Some(parse_usize_list(&args[i], "--threads"));
            }
            "--producer-core" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --producer-core requires a value");
                    process::exit(1);
                }
                producer_core = Some(parse_usize(&args[i], "--producer-core"));
            }
            "--backend-core" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --backend-core requires a value");
                    process::exit(1);
                }
                backend_core = Some(parse_usize(&args[i], "--backend-core"));
            }
            other => {
                eprintln!("error: unknown flag '{}'", other);
                eprintln!(
                    "usage: harness --ns-per-tick <float> --output <path.json> \
                     [--threads <n,...>] [--producer-core <n>] [--backend-core <n>]"
                );
                process::exit(1);
            }
        }
        i += 1;
    }

    let ns_per_tick = ns_per_tick.expect("--ns-per-tick is required");
    let output = output.expect("--output is required");

    Config {
        ns_per_tick,
        output,
        thread_counts: thread_counts.unwrap_or_else(|| vec![1]),
        producer_core,
        backend_core,
    }
}

// JSON output types
#[derive(Serialize)]
struct ConfigResult {
    workload: String,
    threads: usize,
    throughput: u64,
    p50: f64,
    p95: f64,
    p99: f64,
    p999: f64,
    max: f64,
}

#[derive(Serialize)]
struct Output {
    candidate: String,
    os: String,
    arch: String,
    clock: String,
    ns_per_tick: f64,
    batch_size: usize,
    total_messages: u64,
    samples: usize,
    results: Vec<ConfigResult>,
}

// main
fn main() {
    let cfg = parse_args();

    // Init ticklog once with a null sink. The Guard is intentionally
    // leaked so the drain runs for the lifetime of the process.
    let drain_affinity = cfg.backend_core.map(|c| vec![c]);
    let guard = ticklog::configure! {
        sink: NullSink,
        max_level: Level::Trace,
        drain_affinity: drain_affinity,
    }
    .expect("ticklog build");
    std::mem::forget(guard);

    let mut results = Vec::new();

    for &n_threads in &cfg.thread_counts {
        for &wl in Workload::all() {
            eprintln!(
                "  {} threads={} ...",
                wl.name(),
                n_threads
            );
            results.push(measure_config(&cfg, wl, n_threads));
        }
    }

    let clock_name = if cfg!(target_arch = "x86_64") {
        "rdtsc"
    } else if cfg!(target_arch = "aarch64") {
        "cntvct_el0"
    } else {
        "fallback"
    };

    let output = Output {
        candidate: "ticklog".to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        clock: clock_name.to_string(),
        ns_per_tick: cfg.ns_per_tick,
        batch_size: BATCH,
        total_messages: TOTAL_MESSAGES,
        samples: SAMPLES,
        results,
    };

    let json = serde_json::to_string_pretty(&output).expect("JSON serialize");
    fs::write(&cfg.output, json).expect("write output file");

    eprintln!("done -> {}", cfg.output.display());
}
