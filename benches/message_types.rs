//! Encode cost per `Loggable` argument type: empty (fixed overhead),
//! u64, f64, bool, &str, and a mixed multi-arg record.

#[path = "common/sink.rs"]
mod sink;

use criterion::{Criterion, criterion_group, criterion_main};
use ticklog::info;

fn bench_message_types(c: &mut Criterion) {
    sink::init_ticklog(None);

    let mut g = c.benchmark_group("message_types");

    g.bench_function("empty", |b| {
        b.iter(|| info!("heartbeat"));
    });
    g.bench_function("one_u64", |b| {
        b.iter(|| info!("x={}", criterion::black_box(1u64)));
    });
    g.bench_function("one_f64", |b| {
        b.iter(|| info!("x={}", criterion::black_box(3.5f64)));
    });
    g.bench_function("one_bool", |b| {
        b.iter(|| info!("flag={}", criterion::black_box(true)));
    });
    g.bench_function("one_str", |b| {
        b.iter(|| info!("{}", criterion::black_box("a short message")));
    });
    g.bench_function("mixed_u64_f64_str", |b| {
        b.iter(|| {
            info!(
                "{} {} {}",
                criterion::black_box(1u64),
                criterion::black_box(2.5f64),
                criterion::black_box("ok"),
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
    targets = bench_message_types
}
criterion_main!(benches);
