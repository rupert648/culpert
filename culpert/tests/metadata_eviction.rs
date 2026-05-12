//! Asserts that `LocalSpanContext` evicts its metadata cache when
//! `snapshot()` is called, instead of growing monotonically over the
//! process lifetime.
//!
//! Exercises the `SpanContext::on_snapshot` hook end-to-end via the
//! aggregator. The foundations and tracing adapters override the same
//! hook with the same intent; this test uses the local-scope adapter
//! because it's the simplest one to exercise without a tracer.

use culpert::{Config, LocalSpanContext, SpanContext, TrackingAllocator};
use std::alloc::System;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

/// Trial sizes: we enter N unique scopes, run a small workload inside
/// each so they each generate at least one sample, snapshot, then verify
/// the metadata cache is empty (or near-empty — see comments).
const SCOPES_PER_TRIAL: usize = 200;

#[test]
fn metadata_cache_evicts_on_snapshot() {
    let ctx = LocalSpanContext::new();
    culpert::install(
        LocalSpanContext::new(),
        Config {
            rate_bytes: 1024,
            stack_depth: 8,
            buffer_capacity: 1 << 16,
            ..Default::default()
        },
    );

    // Bring the cache to a known empty state.
    let _ = culpert::snapshot();

    // First trial: open SCOPES_PER_TRIAL distinct scopes, allocate inside
    // each one so the SpanContext caches their metadata, then snapshot.
    let scope_ids = run_workload(SCOPES_PER_TRIAL);

    let profile = culpert::snapshot();
    assert!(
        profile.spans.len() >= SCOPES_PER_TRIAL / 2,
        "expected at least {} spans in the profile (saw {}); the test \
         workload may not be triggering samples",
        SCOPES_PER_TRIAL / 2,
        profile.spans.len()
    );

    // The Profile holds its own cloned copy of every span's metadata, so
    // it should be fully populated regardless of cache state.
    for id in &scope_ids {
        let _ = profile.spans.get(id); // may or may not be present (depends on whether the span had a direct sample), the important
        // assertion is below: the cache itself is empty.
        let _ = ctx.metadata(*id); // pre-clear-check is below
    }

    // Cache eviction: after snapshot, the LocalSpanContext's metadata
    // should be empty (no live samples held an active reference to any of
    // the scopes we entered — they're all dropped). The `on_snapshot` hook
    // wired into the aggregator should have cleared it.
    let mut still_cached = 0usize;
    for id in &scope_ids {
        if ctx.metadata(*id).is_some() {
            still_cached += 1;
        }
    }
    assert_eq!(
        still_cached, 0,
        "expected metadata cache to be empty after snapshot, but \
         {still_cached} of {} entries are still cached",
        scope_ids.len()
    );

    // Second trial: repeat. If on_snapshot is missing the eviction would
    // accumulate; we want to confirm it stays bounded across multiple
    // snapshot cycles.
    let _ = run_workload(SCOPES_PER_TRIAL);
    let _ = culpert::snapshot();

    let mut total_cached_after_second_trial = 0usize;
    for id in &scope_ids {
        if ctx.metadata(*id).is_some() {
            total_cached_after_second_trial += 1;
        }
    }
    assert_eq!(
        total_cached_after_second_trial, 0,
        "cache regrew or wasn't evicted between snapshots: {} entries \
         from the first trial still cached after a second snapshot",
        total_cached_after_second_trial
    );
}

fn run_workload(scopes: usize) -> Vec<culpert::SpanId> {
    let ctx = LocalSpanContext::new();
    let mut ids = Vec::with_capacity(scopes);
    for _ in 0..scopes {
        let _g = culpert::scope::enter("workload_scope");
        // Some allocation work so the scope produces samples.
        let v: Vec<u8> = Vec::with_capacity(8 * 1024);
        std::hint::black_box(&v);
        if let Some(id) = ctx.current_span() {
            ids.push(id);
        }
    }
    ids
}
