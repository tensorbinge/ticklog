//! Floor baselines + ticklog throughput (calls/s and bytes/s).

#[path = "../../common/workloads.rs"]
mod workloads;

use std::io;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use ticklog::{Level, LogSink, info};

struct NullSink;

impl LogSink for NullSink {
    fn accept(&mut self, _line: &[u8], _level: Level) -> io::Result<()> {
        Ok(())
    }
}

/// Approximate bytes for `x=42\n` in ticklog binary format.
const ONE_U64_BYTES: u64 = 50;

// -- Floor baselines --------------------------------------------------

fn bench_baseline_throughput(c: &mut Criterion) {
    let mut g = c.benchmark_group("throughput_baseline");

    g.throughput(Throughput::Elements(1u64));
    g.bench_function("per_call", |b| {
        b.iter(|| criterion::black_box(1u64));
    });

    g.throughput(Throughput::Bytes(std::mem::size_of::<u64>() as u64));
    g.bench_function("per_byte", |b| {
        b.iter(|| criterion::black_box(1u64));
    });

    g.finish();
}

// -- ticklog throughput -----------------------------------------------

fn bench_ticklog_throughput(c: &mut Criterion) {
    let guard = ticklog::configure! {
        sink: NullSink,
        max_level: Level::Trace,
    }
    .expect("ticklog build");
    std::mem::forget(guard);

    let mut g = c.benchmark_group("throughput_ticklog");

    g.throughput(Throughput::Elements(1u64));
    g.bench_function("per_call", |b| {
        b.iter(|| info!("x={}", criterion::black_box(1u64)));
    });

    g.throughput(Throughput::Bytes(ONE_U64_BYTES));
    g.bench_function("per_byte", |b| {
        b.iter(|| info!("x={}", criterion::black_box(1u64)));
    });

    g.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .significance_level(0.01)
        .noise_threshold(0.02)
        .sample_size(200);
    targets = bench_baseline_throughput, bench_ticklog_throughput
}
criterion_main!(benches);
