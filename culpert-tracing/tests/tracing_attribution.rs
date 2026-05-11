//! End-to-end test of the tracing-crate adapter:
//!
//! - `TrackingAllocator` set as the global allocator
//! - `tracing_subscriber::registry()` composed with `culpert_tracing::layer()`
//! - culpert installed via `culpert_tracing::install`
//! - allocations performed inside `tracing::info_span!` scopes
//! - snapshot + assertions on span attribution AND parent extraction
//!
//! Single `#[test]` function because both `culpert::install` and
//! `tracing::subscriber::set_global_default` are process-global one-shots.

use culpert::{Config, Profile, SpanId, TrackingAllocator};
use std::alloc::System;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

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
fn end_to_end_attribution_through_tracing() {
    // Set up the subscriber stack with culpert-tracing's layer in it.
    // Registry must be present for LookupSpan to work; our layer needs it
    // for parent resolution.
    tracing_subscriber::registry()
        .with(culpert_tracing::layer())
        .init();

    culpert_tracing::install_with_config(Config {
        rate_bytes: 4 * 1024,
        stack_depth: 16,
        buffer_capacity: 65_536,
    });

    // Two sibling spans.
    {
        let span = tracing::info_span!("root_via_tracing");
        let _entered = span.enter();
        for _ in 0..50 {
            let v = Vec::<u8>::with_capacity(10_000);
            std::hint::black_box(&v);
        }
    }
    {
        let span = tracing::info_span!("other_via_tracing");
        let _entered = span.enter();
        for _ in 0..50 {
            let v = Vec::<u8>::with_capacity(10_000);
            std::hint::black_box(&v);
        }
    }

    let p1 = culpert::snapshot();
    eprintln!(
        "spans seen: {:?}",
        p1.spans.values().map(|m| &m.name).collect::<Vec<_>>()
    );

    let root_bytes = bytes_for_named(&p1, "root_via_tracing");
    let other_bytes = bytes_for_named(&p1, "other_via_tracing");

    assert!(
        root_bytes > 100_000,
        "root_via_tracing attribution too low: {root_bytes}"
    );
    assert!(
        other_bytes > 100_000,
        "other_via_tracing attribution too low: {other_bytes}"
    );

    // ----- hierarchy --------------------------------------------------
    // A parent span containing a child span; verify the layer recorded the
    // parent relationship.
    {
        let parent = tracing::info_span!("parent_via_tracing");
        let _enter_parent = parent.enter();
        {
            let child = tracing::info_span!("child_via_tracing");
            let _enter_child = child.enter();
            for _ in 0..30 {
                let v = Vec::<u8>::with_capacity(10_000);
                std::hint::black_box(&v);
            }
        }
    }

    let p2 = culpert::snapshot();

    let parent_id = p2
        .spans
        .iter()
        .find(|(_, m)| m.name == "parent_via_tracing")
        .map(|(id, _)| *id);
    let (child_id, child_meta) = p2
        .spans
        .iter()
        .find(|(_, m)| m.name == "child_via_tracing")
        .map(|(id, m)| (*id, m.clone()))
        .expect("child_via_tracing span should be in profile");

    assert_eq!(
        child_meta.parent, parent_id,
        "child's parent should be the parent span \
         (parent_id={parent_id:?}, child.parent={:?})",
        child_meta.parent
    );
    eprintln!(
        "hierarchy: parent={:?} child={:?} child.parent={:?}",
        parent_id, child_id, child_meta.parent
    );
}
