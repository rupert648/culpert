//! Baseline overhead bench: pure System allocator.
//!
//! This binary uses `std::alloc::System` directly as the global allocator
//! so we have a reference point uncontaminated by `TrackingAllocator`'s
//! forwarding overhead.

use criterion::{criterion_group, criterion_main};

#[path = "common.rs"]
mod common;

criterion_group!(benches, common::workloads);
criterion_main!(benches);
