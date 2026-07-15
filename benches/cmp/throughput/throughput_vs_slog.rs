//! Rust ecosystem comparison: `slog` throughput.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use slog::Drain;

const ONE_U64_BYTES: u64 = 50;

fn bench_slog_throughput(c: &mut Criterion) {
    let drain = slog_async::Async::new(slog::Discard)
        .chan_size(1_048_576)
        .build()
        .fuse();
    let logger = slog::Logger::root(drain, slog::o!());

    let mut g = c.benchmark_group("throughput_slog");

    g.throughput(Throughput::Elements(1u64));
    g.bench_function("per_call", |b| {
        b.iter(|| slog::info!(logger, "x={}", criterion::black_box(1u64)));
    });

    g.throughput(Throughput::Bytes(ONE_U64_BYTES));
    g.bench_function("per_byte", |b| {
        b.iter(|| slog::info!(logger, "x={}", criterion::black_box(1u64)));
    });

    g.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .significance_level(0.01)
        .noise_threshold(0.02)
        .sample_size(200);
    targets = bench_slog_throughput
}
criterion_main!(benches);
