//! Shared constants for ecosystem comparison benches, so every candidate
//! runs the identical record shapes.
//!
//! `mod`-included by several bench binaries, so not every binary references
//! every constant (`dead_code`), and `TEST_F64` is a deliberate arbitrary
//! payload rather than pi (`approx_constant`).
#![allow(dead_code, clippy::approx_constant)]

/// Test value for `u64` benchmarks -- defeats constant folding via `black_box`.
pub const TEST_U64: u64 = 42;

/// Test value for `f64` benchmarks.
pub const TEST_F64: f64 = 3.14159;

/// Test value for `bool` benchmarks.
pub const TEST_BOOL: bool = true;

/// Test value for `&str` benchmarks.
pub const TEST_STR: &str = "hello world";
