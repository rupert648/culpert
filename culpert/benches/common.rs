//! Shared workloads for the overhead-bench triplet
//! (baseline / tracking_off / tracking_on). Each bench file `#[path]`-includes
//! this module and runs the same set of workloads under a different
//! global-allocator setup, so direct cross-bench comparisons are meaningful.

use criterion::{Criterion, Throughput};

/// Allocate `count` `Vec<u8>`s of `size` bytes each, drop them, repeat.
/// Stresses the alloc/dealloc fast path in the allocator.
fn alloc_drop(count: usize, size: usize) {
    for _ in 0..count {
        let v: Vec<u8> = Vec::with_capacity(size);
        std::hint::black_box(&v);
    }
}

/// Push `count` `u64`s into a fresh Vec, forcing several reallocs as it
/// grows. Stresses the realloc path.
fn vec_grow(count: usize) {
    let mut v: Vec<u64> = Vec::new();
    for i in 0..count {
        v.push(i as u64);
    }
    std::hint::black_box(&v);
}

/// Mixed: small + large allocs interleaved. Closer to a real workload's
/// allocation distribution.
fn mixed(count: usize) {
    for i in 0..count {
        let small: Vec<u8> = Vec::with_capacity(64);
        let big: Vec<u64> = Vec::with_capacity(if i % 8 == 0 { 4096 } else { 32 });
        std::hint::black_box(&small);
        std::hint::black_box(&big);
    }
}

/// Realistic-ish: a small alloc followed by a chunk of CPU-bound work.
/// Models a request handler doing real work between allocations rather
/// than allocating in a tight loop. The CPU-to-alloc ratio dominates the
/// real-world overhead numbers users will see.
fn alloc_and_work(count: usize) {
    let mut acc: u64 = 0;
    for i in 0..count {
        let v: Vec<u8> = Vec::with_capacity(256);
        std::hint::black_box(&v);
        // ~1 µs of CPU work per allocation: a hash-like loop the compiler
        // can't easily eliminate.
        for j in 0..256u64 {
            acc = acc.wrapping_add((j ^ i as u64).wrapping_mul(0x100000001b3));
        }
    }
    std::hint::black_box(acc);
}

pub fn workloads(c: &mut Criterion) {
    let mut g = c.benchmark_group("alloc");

    // 200 × 64-byte allocs: many small allocs, no sampling expected at 512 KiB.
    g.throughput(Throughput::Elements(200));
    g.bench_function("small_x200", |b| b.iter(|| alloc_drop(200, 64)));

    // 200 × 4 KiB allocs.
    g.bench_function("medium_x200", |b| b.iter(|| alloc_drop(200, 4 * 1024)));

    // 50 × 1 MiB allocs: every alloc crosses the 512 KiB sample threshold.
    g.bench_function("large_x50_1MiB", |b| b.iter(|| alloc_drop(50, 1024 * 1024)));

    // Vec growth.
    g.bench_function("vec_grow_10k", |b| b.iter(|| vec_grow(10_000)));

    // Mixed.
    g.bench_function("mixed_x500", |b| b.iter(|| mixed(500)));

    // Realistic: alloc + computation, more representative of real services.
    g.bench_function("alloc_and_work_x200", |b| b.iter(|| alloc_and_work(200)));

    g.finish();
}
