//! Producer hot-path latency of a single `info!` call, isolated from
//! the drain by a null sink.

#[path = "common/affinity.rs"]
mod affinity;

#[path = "common/sink.rs"]
mod sink;

use criterion::{Criterion, criterion_group, criterion_main};
use ticklog::info;

fn bench_single_record(c: &mut Criterion) {
    affinity::pin_producer_from_env();

    let drain_affinity = affinity::drain_core_from_env().map(|c| vec![c]);
    sink::init_ticklog(drain_affinity);

    c.bench_function("info_one_u64", |b| {
        b.iter(|| info!("x={}", criterion::black_box(1u64)));
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .significance_level(0.01)
        .noise_threshold(0.02)
        .sample_size(200);
    targets = bench_single_record
}
criterion_main!(benches);
