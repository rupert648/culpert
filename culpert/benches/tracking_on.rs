//! Overhead bench: `TrackingAllocator<System>` with the profiler installed.
//!
//! Measures the cost of the full sampling path: every alloc decrements a
//! per-thread counter, sampled allocs capture a stack trace and push a
//! `RawSample` into the per-thread buffer. Default `Config` (1-in-512 KiB).
//!
//! Compared against `baseline` and `tracking_off`, the delta from
//! `tracking_off` is the cost of *enabling* profiling.

use criterion::{criterion_group, criterion_main};
use culpert::{Config, MockSpanContext, TrackingAllocator};
use std::alloc::System;
use std::num::NonZeroU64;
use std::sync::OnceLock;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

static MOCK: OnceLock<MockSpanContext> = OnceLock::new();

fn setup() {
    let mock = MOCK.get_or_init(MockSpanContext::new);
    culpert::install(mock.clone(), Config::default());

    // Enter a single span for the whole benchmark process. Every sampled
    // allocation gets attributed to it; this models the realistic case of
    // a request handler under load (we're not benchmarking the no-span path).
    let id = NonZeroU64::new(1).unwrap();
    // Leak the guard — we want the span on the stack for the entire run.
    let guard = mock.enter(id, "bench_span", None);
    Box::leak(Box::new(guard));
}

fn benches(c: &mut criterion::Criterion) {
    setup();
    common::workloads(c);
}

#[path = "common.rs"]
mod common;

criterion_group!(b, benches);
criterion_main!(b);
