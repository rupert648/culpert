//! Overhead bench: same as `tracking_on`, but with
//! `StackCaptureStrategy::FramePointer` instead of the default
//! Backtrace.
//!
//! Comparing the resulting numbers against `tracking_on` directly
//! quantifies the speedup from switching to the frame-pointer walk:
//! every sampled allocation does ~2 memory loads per stack frame
//! instead of a Call Frame Information table lookup + DWARF expression
//! evaluation. We expect a ~10× improvement on realistic workloads.
//!
//! Requires the build to have been compiled with
//! `RUSTFLAGS="-C force-frame-pointers=yes"` to see the full effect.
//! Without that, the walk terminates at the first frame-pointer-less
//! function and you get a shallow stack — the numbers will still be
//! faster (less work per sample) but the captures less useful.

use criterion::{criterion_group, criterion_main};
use culpert::{Config, MockSpanContext, StackCaptureStrategy, TrackingAllocator};
use std::alloc::System;
use std::num::NonZeroU64;
use std::sync::OnceLock;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

static MOCK: OnceLock<MockSpanContext> = OnceLock::new();

fn setup() {
    let mock = MOCK.get_or_init(MockSpanContext::new);
    culpert::install(
        mock.clone(),
        Config {
            stack_capture_strategy: StackCaptureStrategy::FramePointer,
            ..Default::default()
        },
    );

    let id = NonZeroU64::new(1).unwrap();
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
