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
use foundations::telemetry::tracing;
use foundations::telemetry::TelemetryContext;
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
        ..Default::default()
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
    let ratio = (root_bytes.max(other_bytes)) as f64 / (root_bytes.min(other_bytes)).max(1) as f64;
    assert!(
        ratio < 4.0,
        "root vs other differ by >4x: root={root_bytes} other={other_bytes}"
    );

    // ----- hierarchy --------------------------------------------------
    // Enter a parent span and a child span inside it; verify the adapter
    // extracts the parent SpanId from cf-rustracing's ChildOf reference.
    //
    // The parent block needs at least one allocation of its own so that
    // its metadata gets cached in the FoundationsSpanContext (the cache
    // is populated on first-sight via `current_span_inner`, which only
    // fires when an alloc inside the parent's scope is sampled). Under
    // v0.2 geometric sampling a single tiny allocation is only sampled
    // with low probability, so we do enough work to make the cache hit
    // statistically certain.
    {
        let _parent = tracing::span("parent_via_adapter");
        for _ in 0..30 {
            let v = Vec::<u8>::with_capacity(10_000);
            std::hint::black_box(&v);
        }
        {
            let _child = tracing::span("child_via_adapter");
            for _ in 0..30 {
                let v = Vec::<u8>::with_capacity(10_000);
                std::hint::black_box(&v);
            }
        }
    }

    let p2 = culpert::snapshot();

    // Find the two new spans by name.
    let parent_id = p2
        .spans
        .iter()
        .find(|(_, m)| m.name == "parent_via_adapter")
        .map(|(id, _)| *id);
    let (child_id, child_meta) = p2
        .spans
        .iter()
        .find(|(_, m)| m.name == "child_via_adapter")
        .map(|(id, m)| (*id, m.clone()))
        .expect("child_via_adapter span should be in profile");

    // The child's metadata.parent should point at the parent span's id.
    assert_eq!(
        child_meta.parent, parent_id,
        "child span's parent should be the parent span (parent_id={parent_id:?}, child.parent={:?})",
        child_meta.parent
    );
    eprintln!(
        "hierarchy: parent={:?} child={:?} child.parent={:?}",
        parent_id, child_id, child_meta.parent
    );
}
