//! Statistical correctness test for geometric sampling.
//!
//! With Bernstein-corrected weights, `Profile.entries[*].bytes_total`
//! summed across all entries should be an unbiased estimator of the
//! true total bytes allocated. We verify this empirically: run a known
//! workload several times, check that the mean estimated total
//! converges to the truth within Monte-Carlo bounds.
//!
//! Single-process, single-thread by design — that's enough to exercise
//! the math, and avoids contention with whatever other tests in the
//! workspace need exclusive access to the global allocator.

use culpert::{Config, MockSpanContext, TrackingAllocator};
use std::alloc::System;
use std::num::NonZeroU64;
use std::sync::OnceLock;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

static MOCK: OnceLock<MockSpanContext> = OnceLock::new();

// Workload knobs. ALLOC_SIZE / RATE_BYTES = ~5 so each alloc has a
// substantial but not certain sampling probability — exactly the regime
// where naive counter-mod sampling shows the worst bias and geometric
// sampling should excel.
const ALLOC_BYTES: usize = 50 * 1024; // 50 KiB
const ALLOC_COUNT: usize = 4_000; //  → 4000 × 50 KiB = ~200 MiB true total per trial
const TRIALS: usize = 30;
const RATE_BYTES: u64 = 10 * 1024; // 10 KiB → mean ~5 samples per alloc

const TRUE_TOTAL: u64 = (ALLOC_BYTES as u64) * (ALLOC_COUNT as u64);

#[test]
fn bytes_total_is_unbiased_under_geometric_sampling() {
    let mock = MOCK.get_or_init(MockSpanContext::new);
    culpert::install(
        mock.clone(),
        Config {
            rate_bytes: RATE_BYTES,
            stack_depth: 4,
            buffer_capacity: 1 << 18,
            ..Default::default()
        },
    );

    // Long-lived span so every alloc is attributed; saves having to
    // re-enter on each trial.
    let id = NonZeroU64::new(1).unwrap();
    let _guard = Box::leak(Box::new(mock.enter(id, "bench_span", None)));

    // Drain any startup-noise samples so the first trial isn't polluted.
    let _ = culpert::snapshot();

    let mut estimates = Vec::with_capacity(TRIALS);
    for _ in 0..TRIALS {
        // Workload: fixed-size allocations of ALLOC_BYTES, dropped immediately.
        for _ in 0..ALLOC_COUNT {
            let v: Vec<u8> = Vec::with_capacity(ALLOC_BYTES);
            std::hint::black_box(&v);
        }

        let profile = culpert::snapshot();
        let total_bytes: u64 = profile.entries.iter().map(|e| e.bytes_total).sum();
        estimates.push(total_bytes);
    }

    let mean = estimates.iter().copied().sum::<u64>() as f64 / TRIALS as f64;
    let true_total = TRUE_TOTAL as f64;
    let relative_error = (mean - true_total).abs() / true_total;

    eprintln!(
        "geometric sampling: true_total = {true_total:.0} bytes, mean estimate = {mean:.0} bytes \
         over {TRIALS} trials, relative error = {:.3}%",
        100.0 * relative_error
    );

    // 5% tolerance is well above the expected Monte-Carlo error.
    //
    // Standard error per trial is approximately
    //   sqrt(rate × true_total) ≈ sqrt(10 KiB × 200 MiB) ≈ 1.4 MiB.
    // Standard error of the mean across TRIALS = 30 trials is
    //   1.4 MiB / sqrt(30) ≈ 260 KiB ≈ 0.13% of true_total.
    // 5% is a ~40σ tolerance — only a real systematic bias would trip
    // it (a flake budget of 1 in ~10^350).
    assert!(
        relative_error < 0.05,
        "mean estimate {mean:.0} differs from true total {true_total:.0} by {:.3}% (>5% tolerance)",
        100.0 * relative_error,
    );
}
