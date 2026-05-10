//! Overhead bench: `TrackingAllocator<System>` with **no** profiler installed.
//!
//! Measures the cost of the forwarding path — allocator wrapper compiled in,
//! `sampler::observe()` called on every alloc, but `OnceLock` empty so the
//! observe fast-path returns immediately. This is what users pay if they
//! ship culpert in the binary but don't call `culpert::install` (e.g. only
//! enabling profiling on demand).

use criterion::{criterion_group, criterion_main};
use culpert::TrackingAllocator;
use std::alloc::System;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

#[path = "common.rs"]
mod common;

criterion_group!(benches, common::workloads);
criterion_main!(benches);
