//! Floor baselines + ticklog multi-thread scaling.

use std::io;
use std::sync::{Arc, Barrier};

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use ticklog::{info, Level, LogSink};

struct NullSink;

impl LogSink for NullSink {
    fn accept(&mut self, _line: &[u8], _level: Level) -> io::Result<()> {
        Ok(())
    }
}

const RECORDS_PER_THREAD: u64 = 1_000;

/// Thread counts to sweep. Override with
/// `TICKLOG_SCALING_THREADS=1,2,4,8,16,32,64,128`; defaults to `[1, 2, 4, 8]`
/// when unset so existing runs are unchanged.
fn thread_counts() -> Vec<u32> {
    match std::env::var("TICKLOG_SCALING_THREADS") {
        Ok(s) => {
            let v: Vec<u32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if v.is_empty() {
                vec![1, 2, 4, 8]
            } else {
                v
            }
        }
        Err(_) => vec![1, 2, 4, 8],
    }
}

// -- Floor baseline (thread overhead) ---------------------------------

fn bench_baseline_scaling(c: &mut Criterion) {
    let mut g = c.benchmark_group("scaling_baseline");

    for n_threads in thread_counts() {
        let total_calls = RECORDS_PER_THREAD * n_threads as u64;
        g.throughput(Throughput::Elements(total_calls));
        g.sample_size(30);

        g.bench_function(format!("{}_threads", n_threads), |b| {
            b.iter_custom(|iters| {
                let barrier = Arc::new(Barrier::new(n_threads as usize));
                let mut handles = Vec::new();
                let per_thread = RECORDS_PER_THREAD * iters;

                for _ in 0..n_threads {
                    let b = Arc::clone(&barrier);
                    handles.push(std::thread::spawn(move || {
                        b.wait();
                        for i in 0..per_thread {
                            criterion::black_box(i);
                        }
                    }));
                }

                let start = std::time::Instant::now();
                for h in handles {
                    h.join().unwrap();
                }
                start.elapsed()
            });
        });
    }

    g.finish();
}

// -- ticklog scaling --------------------------------------------------

fn bench_ticklog_scaling(c: &mut Criterion) {
    let guard = ticklog::builder()
        .sink(NullSink)
        .max_level(Level::Trace)
        .build()
        .expect("ticklog build");
    std::mem::forget(guard);

    let mut g = c.benchmark_group("scaling_ticklog");

    for n_threads in thread_counts() {
        let total_calls = RECORDS_PER_THREAD * n_threads as u64;
        g.throughput(Throughput::Elements(total_calls));
        g.sample_size(30);

        g.bench_function(format!("{}_threads", n_threads), |b| {
            b.iter_custom(|iters| {
                let barrier = Arc::new(Barrier::new(n_threads as usize));
                let mut handles = Vec::new();
                let per_thread = RECORDS_PER_THREAD * iters;

                for _ in 0..n_threads {
                    let b = Arc::clone(&barrier);
                    handles.push(std::thread::spawn(move || {
                        let _ = ticklog::warm_up();
                        b.wait();
                        for i in 0..per_thread {
                            info!("x={}", i);
                        }
                    }));
                }

                let start = std::time::Instant::now();
                for h in handles {
                    h.join().unwrap();
                }
                start.elapsed()
            });
        });
    }

    g.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .significance_level(0.01)
        .noise_threshold(0.02)
        .sample_size(20);
    targets = bench_baseline_scaling, bench_ticklog_scaling
}
criterion_main!(benches);
