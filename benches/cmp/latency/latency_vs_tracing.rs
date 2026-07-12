//! Rust ecosystem comparison: `tracing` + `tracing-subscriber` + non-blocking writer.

#[path = "../../common/workloads.rs"]
mod workloads;

use std::io;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};

fn bench_tracing(c: &mut Criterion) {
    let (non_blocking, _guard) = tracing_appender::non_blocking(io::sink());
    let subscriber = tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("set tracing subscriber");

    let mut g = c.benchmark_group("tracing");
    g.throughput(Throughput::Elements(1u64));

    g.bench_function("empty", |b| {
        b.iter(|| tracing::info!("heartbeat"));
    });
    g.bench_function("one_u64", |b| {
        b.iter(|| tracing::info!("x={}", criterion::black_box(workloads::TEST_U64)));
    });
    g.bench_function("one_f64", |b| {
        b.iter(|| tracing::info!("x={}", criterion::black_box(workloads::TEST_F64)));
    });
    g.bench_function("one_bool", |b| {
        b.iter(|| tracing::info!("flag={}", criterion::black_box(workloads::TEST_BOOL)));
    });
    g.bench_function("one_str", |b| {
        b.iter(|| tracing::info!("{}", criterion::black_box(workloads::TEST_STR)));
    });
    g.bench_function("mixed_u64_f64_str", |b| {
        b.iter(|| {
            tracing::info!(
                "{} {} {}",
                criterion::black_box(workloads::TEST_U64),
                criterion::black_box(workloads::TEST_F64),
                criterion::black_box(workloads::TEST_STR),
            )
        });
    });

    g.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .significance_level(0.01)
        .noise_threshold(0.02)
        .sample_size(200);
    targets = bench_tracing
}
criterion_main!(benches);
