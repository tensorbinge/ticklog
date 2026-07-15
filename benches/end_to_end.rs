//! End-to-end throughput: produce N records, block until the drain has
//! formatted and delivered every one, then report wall-clock time.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use ticklog::{info, Level, LogSink};

/// Counts delivered records and runs formatted bytes through `black_box`.
struct CountingSink {
    delivered: Arc<AtomicU64>,
}

impl LogSink for CountingSink {
    fn accept(&mut self, line: &[u8], _level: Level) -> io::Result<()> {
        std::hint::black_box(line);
        self.delivered.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

fn bench_end_to_end(c: &mut Criterion) {
    let delivered = Arc::new(AtomicU64::new(0));
    let sink = CountingSink {
        delivered: Arc::clone(&delivered),
    };

    let guard = ticklog::configure! {
        sink: sink,
        max_level: Level::Trace,
    }
    .expect("ticklog build");
    std::mem::forget(guard);

    const BATCH: u64 = 1_000;

    let mut g = c.benchmark_group("end_to_end");
    g.throughput(Throughput::Elements(BATCH));

    g.bench_function("pipeline", |b| {
        b.iter_custom(|iters| {
            let start = std::time::Instant::now();
            for _ in 0..iters {
                delivered.store(0, Ordering::Relaxed);
                for i in 0..BATCH {
                    info!("x={}", i);
                }
                // Spin until the drain has delivered every record.
                while delivered.load(Ordering::Acquire) < BATCH {
                    std::hint::spin_loop();
                }
            }
            start.elapsed()
        });
    });

    g.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .significance_level(0.01)
        .noise_threshold(0.02)
        .sample_size(50);
    targets = bench_end_to_end
}
criterion_main!(benches);
