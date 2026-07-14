//! Temporary benchmark: measure scratch-buffer + copy overhead in isolation.
//! Run: cargo bench --bench scratch_overhead
//!
//! Buffers are created once and reused across iterations, matching how
//! `ThreadBuf.scratch` and the ring buffer are reused in production.

use criterion::{criterion_group, criterion_main, Criterion};

// const HEADER_BYTES: usize = 41;
const SMALL_RECORD: usize = 41; // zero-arg info!("")
const TYPICAL_RECORD: usize = 128; // info!("x={}", val) with a few args

/// Pre-allocated buffers, reused across iterations (like ThreadBuf and the ring).
struct Reusable {
    scratch: Vec<u8>,
    dest: Vec<u8>,
}

impl Reusable {
    fn new() -> Self {
        Self {
            scratch: Vec::with_capacity(4096),
            dest: vec![0u8; 4096],
        }
    }
}

/// Current flow: assemble into scratch, then copy_nonoverlapping to dest.
/// Models what `record::assemble(&mut tb.scratch, ...)` + `ring.write_record(&tb.scratch, ...)` does.
fn scratch_then_copy(state: &mut Reusable, len: usize) {
    let scratch = &mut state.scratch;
    let dest = &mut state.dest;

    scratch.clear();
    scratch.reserve(len);
    // Simulate assemble writing len bytes (header + args) into scratch.
    // The real assembly uses copy_from_slice / copy_nonoverlapping for each field;
    // write_bytes approximates the same bulk-write behaviour.
    unsafe {
        let base = scratch.as_mut_ptr();
        std::ptr::write_bytes(base, 0xAA, len);
        scratch.set_len(len);
    }
    // Simulate write_record's copy_nonoverlapping from scratch to ring.
    unsafe {
        std::ptr::copy_nonoverlapping(scratch.as_ptr(), dest.as_mut_ptr(), len);
    }
}

/// Hypothetical direct flow: write len bytes directly into dest, no scratch.
fn direct_to_dest(state: &mut Reusable, len: usize) {
    unsafe {
        std::ptr::write_bytes(state.dest.as_mut_ptr(), 0xAA, len);
    }
}

fn bench_scratch_overhead(c: &mut Criterion) {
    let mut state = Reusable::new();

    c.bench_function("scratch_then_copy_41B", |b| {
        b.iter(|| scratch_then_copy(&mut state, criterion::black_box(SMALL_RECORD)));
    });

    c.bench_function("direct_to_dest_41B", |b| {
        b.iter(|| direct_to_dest(&mut state, criterion::black_box(SMALL_RECORD)));
    });

    c.bench_function("scratch_then_copy_128B", |b| {
        b.iter(|| scratch_then_copy(&mut state, criterion::black_box(TYPICAL_RECORD)));
    });

    c.bench_function("direct_to_dest_128B", |b| {
        b.iter(|| direct_to_dest(&mut state, criterion::black_box(TYPICAL_RECORD)));
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .significance_level(0.01)
        .noise_threshold(0.02)
        .sample_size(200);
    targets = bench_scratch_overhead
}
criterion_main!(benches);
