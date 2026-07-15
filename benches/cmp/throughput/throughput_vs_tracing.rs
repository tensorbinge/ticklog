//! Rust ecosystem comparison: `tracing` throughput.

use std::io;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};

const ONE_U64_BYTES: u64 = 50;

fn bench_tracing_throughput(c: &mut Criterion) {
    let (non_blocking, _guard) = tracing_appender::non_blocking(io::sink());
    let subscriber = tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("set tracing subscriber");

    let mut g = c.benchmark_group("throughput_tracing");

    g.throughput(Throughput::Elements(1u64));
    g.bench_function("per_call", |b| {
        b.iter(|| tracing::info!("x={}", criterion::black_box(1u64)));
    });

    g.throughput(Throughput::Bytes(ONE_U64_BYTES));
    g.bench_function("per_byte", |b| {
        b.iter(|| tracing::info!("x={}", criterion::black_box(1u64)));
    });

    g.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .significance_level(0.01)
        .noise_threshold(0.02)
        .sample_size(200);
    targets = bench_tracing_throughput
}
criterion_main!(benches);
