//! End-to-end test of the foundations adapter:
//!
//! - `TrackingAllocator` set as the global allocator
//! - foundations test telemetry context entered
//! - culpert installed via `culpert_foundations::install`
//! - allocations performed inside several `tracing::span(...)` scopes
//! - snapshot + assertions on span attribution
//!
//! Single `#[test]` function because `culpert::install` is process-global.

use culpert::{Config, Profile, SpanId, TrackingAllocator};
use foundations::telemetry::TelemetryContext;
use foundations::telemetry::tracing;
use std::alloc::System;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

fn bytes_for_named(profile: &Profile, name: &str) -> u64 {
    let id_for_name: Option<SpanId> = profile
        .spans
        .iter()
        .find(|(_, m)| m.name == name)
        .map(|(id, _)| *id);
    match id_for_name {
        None => 0,
        Some(id) => profile
            .entries
            .iter()
            .filter(|e| e.span == Some(id))
            .map(|e| e.bytes_total)
            .sum(),
    }
}

#[test]
fn end_to_end_attribution_through_foundations() {
    // Enter a test telemetry context for the entire test. Without it, the
    // noop tracing harness yields Inactive spans and the adapter (correctly)
    // reports None for them.
    let test_ctx = TelemetryContext::test();
    let _ctx_scope = test_ctx.scope();

    culpert_foundations::install_with_config(Config {
        rate_bytes: 4 * 1024,
        stack_depth: 16,
        buffer_capacity: 65_536,
    });

    // Allocate inside a foundations span.
    {
        let _root = tracing::span("root_via_adapter");
        for _ in 0..50 {
            let v = Vec::<u8>::with_capacity(10_000);
            std::hint::black_box(&v);
        }
    }

    // And a sibling span.
    {
        let _other = tracing::span("other_via_adapter");
        for _ in 0..50 {
            let v = Vec::<u8>::with_capacity(10_000);
            std::hint::black_box(&v);
        }
    }

    let profile = culpert::snapshot();

    eprintln!(
        "spans seen: {:?}",
        profile.spans.values().map(|m| &m.name).collect::<Vec<_>>()
    );
    eprintln!("entries: {}", profile.entries.len());

    let root_bytes = bytes_for_named(&profile, "root_via_adapter");
    let other_bytes = bytes_for_named(&profile, "other_via_adapter");

    // Each allocated ~500 KB (50 × 10 KB). At 4 KiB sampling we should
    // catch a clearly non-zero amount.
    assert!(
        root_bytes > 100_000,
        "root_via_adapter attribution too low: {root_bytes}"
    );
    assert!(
        other_bytes > 100_000,
        "other_via_adapter attribution too low: {other_bytes}"
    );

    // Sample counts should be similar order of magnitude.
    let ratio =
        (root_bytes.max(other_bytes)) as f64 / (root_bytes.min(other_bytes)).max(1) as f64;
    assert!(
        ratio < 4.0,
        "root vs other differ by >4x: root={root_bytes} other={other_bytes}"
    );
}
