//! Floor baselines (`black_box`, `write!(io::sink())`) and ticklog
//! hot-path latency through a null sink.

#[path = "../../common/workloads.rs"]
mod workloads;

#[path = "../../common/affinity.rs"]
mod affinity;

use std::io::{self, Write};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use ticklog::{Level, LogSink, info};

struct NullSink;

impl LogSink for NullSink {
    fn accept(&mut self, _line: &[u8], _level: Level) -> io::Result<()> {
        Ok(())
    }
}

// -- Floor baselines --------------------------------------------------

fn bench_baselines(c: &mut Criterion) {
    let mut g = c.benchmark_group("baseline");
    g.throughput(Throughput::Elements(1u64));

    g.bench_function("noop_u64", |b| {
        b.iter(|| criterion::black_box(workloads::TEST_U64));
    });
    g.bench_function("noop_str", |b| {
        b.iter(|| criterion::black_box(workloads::TEST_STR));
    });
    g.bench_function("format_u64", |b| {
        b.iter(|| {
            let _ = criterion::black_box(write!(
                io::sink(),
                "x={}",
                criterion::black_box(workloads::TEST_U64),
            ));
        });
    });
    g.bench_function("format_str", |b| {
        b.iter(|| {
            let _ = criterion::black_box(write!(
                io::sink(),
                "{}",
                criterion::black_box(workloads::TEST_STR),
            ));
        });
    });

    g.finish();
}

// -- ticklog hot-path -------------------------------------------------

fn bench_ticklog(c: &mut Criterion) {
    affinity::pin_producer_from_env();

    let drain_affinity = affinity::drain_core_from_env().map(|c| vec![c]);
    let guard = ticklog::configure! {
        sink: NullSink,
        max_level: Level::Trace,
        drain_affinity: drain_affinity,
    }
    .expect("ticklog build");
    std::mem::forget(guard);

    let mut g = c.benchmark_group("ticklog");
    g.throughput(Throughput::Elements(1u64));

    g.bench_function("empty", |b| {
        b.iter(|| info!("heartbeat"));
    });
    g.bench_function("one_u64", |b| {
        b.iter(|| info!("x={}", criterion::black_box(workloads::TEST_U64)));
    });
    g.bench_function("one_f64", |b| {
        b.iter(|| info!("x={}", criterion::black_box(workloads::TEST_F64)));
    });
    g.bench_function("one_bool", |b| {
        b.iter(|| info!("flag={}", criterion::black_box(workloads::TEST_BOOL)));
    });
    g.bench_function("one_str", |b| {
        b.iter(|| info!("{}", criterion::black_box(workloads::TEST_STR)));
    });
    g.bench_function("mixed_u64_f64_str", |b| {
        b.iter(|| {
            info!(
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
    targets = bench_baselines, bench_ticklog
}
criterion_main!(benches);
