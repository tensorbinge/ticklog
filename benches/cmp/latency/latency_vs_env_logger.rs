//! Rust ecosystem comparison: `env_logger` over the `log` facade.

#[path = "../../common/workloads.rs"]
mod workloads;

use std::io;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};

fn bench_env_logger(c: &mut Criterion) {
    let sink = Box::new(io::sink());
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Trace)
        .target(env_logger::Target::Pipe(sink))
        .init();

    let mut g = c.benchmark_group("env_logger");
    g.throughput(Throughput::Elements(1u64));

    g.bench_function("empty", |b| {
        b.iter(|| log::info!("heartbeat"));
    });
    g.bench_function("one_u64", |b| {
        b.iter(|| log::info!("x={}", criterion::black_box(workloads::TEST_U64)));
    });
    g.bench_function("one_f64", |b| {
        b.iter(|| log::info!("x={}", criterion::black_box(workloads::TEST_F64)));
    });
    g.bench_function("one_bool", |b| {
        b.iter(|| log::info!("flag={}", criterion::black_box(workloads::TEST_BOOL)));
    });
    g.bench_function("one_str", |b| {
        b.iter(|| log::info!("{}", criterion::black_box(workloads::TEST_STR)));
    });
    g.bench_function("mixed_u64_f64_str", |b| {
        b.iter(|| {
            log::info!(
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
    targets = bench_env_logger
}
criterion_main!(benches);
