//! Rust ecosystem comparison: `slog` with `slog-async`.

#[path = "../../common/workloads.rs"]
mod workloads;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use slog::Drain;

fn bench_slog(c: &mut Criterion) {
    let drain = slog_async::Async::new(slog::Discard)
        .chan_size(1_048_576)
        .build()
        .fuse();
    let logger = slog::Logger::root(drain, slog::o!());

    let mut g = c.benchmark_group("slog");
    g.throughput(Throughput::Elements(1u64));

    g.bench_function("empty", |b| {
        b.iter(|| slog::info!(logger, "heartbeat"));
    });
    g.bench_function("one_u64", |b| {
        b.iter(|| slog::info!(logger, "x={}", criterion::black_box(workloads::TEST_U64)));
    });
    g.bench_function("one_f64", |b| {
        b.iter(|| slog::info!(logger, "x={}", criterion::black_box(workloads::TEST_F64)));
    });
    g.bench_function("one_bool", |b| {
        b.iter(|| {
            slog::info!(
                logger,
                "flag={}",
                criterion::black_box(workloads::TEST_BOOL)
            )
        });
    });
    g.bench_function("one_str", |b| {
        b.iter(|| slog::info!(logger, "{}", criterion::black_box(workloads::TEST_STR)));
    });
    g.bench_function("mixed_u64_f64_str", |b| {
        b.iter(|| {
            slog::info!(
                logger,
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
    targets = bench_slog
}
criterion_main!(benches);
