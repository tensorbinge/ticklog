//! Rust ecosystem comparison: `env_logger` throughput.

use std::io;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};

const ONE_U64_BYTES: u64 = 50;

fn bench_env_logger_throughput(c: &mut Criterion) {
    let sink = Box::new(io::sink());
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Trace)
        .target(env_logger::Target::Pipe(sink))
        .init();

    let mut g = c.benchmark_group("throughput_env_logger");

    g.throughput(Throughput::Elements(1u64));
    g.bench_function("per_call", |b| {
        b.iter(|| log::info!("x={}", criterion::black_box(1u64)));
    });

    g.throughput(Throughput::Bytes(ONE_U64_BYTES));
    g.bench_function("per_byte", |b| {
        b.iter(|| log::info!("x={}", criterion::black_box(1u64)));
    });

    g.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .significance_level(0.01)
        .noise_threshold(0.02)
        .sample_size(200);
    targets = bench_env_logger_throughput
}
criterion_main!(benches);
