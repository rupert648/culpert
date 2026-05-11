//! End-to-end test of the sampling-independent attribution path:
//!
//! - `TrackingAllocator` set as the global allocator
//! - `LocalSpanContext` installed (no foundations / tracing involved)
//! - functions annotated with `#[culpert::span_fn(...)]`
//! - snapshot + assertions on attribution AND hierarchy
//!
//! Demonstrates that allocation attribution works without any external
//! tracer, and without any external trace-sampling rate gating.

use culpert::{Config, LocalSpanContext, Profile, SpanId, TrackingAllocator};
use std::alloc::System;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

#[culpert::span_fn("parse_input")]
fn parse_input() -> Vec<String> {
    (0..100).map(|i| format!("field_{i}")).collect()
}

#[culpert::span_fn("validate_input")]
fn validate_input(parsed: Vec<String>) -> Vec<String> {
    parsed.iter().map(|s| s.to_uppercase()).collect()
}

#[culpert::span_fn("build_response")]
fn build_response(validated: Vec<String>) -> String {
    let mut buf = String::with_capacity(1024);
    for v in validated {
        buf.push_str(&v);
        buf.push('\n');
    }
    buf
}

#[culpert::span_fn("handle_request")]
fn handle_request() -> String {
    let parsed = parse_input();
    let validated = validate_input(parsed);
    build_response(validated)
}

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
fn end_to_end_attribution_through_local_scope() {
    culpert::install(
        LocalSpanContext::new(),
        Config {
            rate_bytes: 4 * 1024,
            stack_depth: 16,
            buffer_capacity: 65_536,
        },
    );

    // Drive the request handler a bunch of times.
    for _ in 0..50 {
        let _ = std::hint::black_box(handle_request());
    }

    let p = culpert::snapshot();

    eprintln!(
        "spans observed: {:?}",
        p.spans.values().map(|m| &m.name).collect::<Vec<_>>()
    );

    // All four spans should have non-trivial attribution.
    let handle_bytes = bytes_for_named(&p, "handle_request");
    let parse_bytes = bytes_for_named(&p, "parse_input");
    let validate_bytes = bytes_for_named(&p, "validate_input");
    let build_bytes = bytes_for_named(&p, "build_response");

    eprintln!(
        "handle_request={handle_bytes} parse_input={parse_bytes} \
         validate_input={validate_bytes} build_response={build_bytes}"
    );

    assert!(
        parse_bytes > 0,
        "parse_input attribution should be non-zero: {parse_bytes}"
    );
    assert!(
        validate_bytes > 0,
        "validate_input attribution should be non-zero: {validate_bytes}"
    );
    assert!(
        build_bytes > 0,
        "build_response attribution should be non-zero: {build_bytes}"
    );

    // ----- hierarchy --------------------------------------------------
    // Find a parse_input span and its handle_request parent; verify the
    // parent linkage culpert::scope built.
    let parse_meta = p
        .spans
        .iter()
        .find(|(_, m)| m.name == "parse_input")
        .map(|(_, m)| m.clone())
        .expect("parse_input span should be in profile");

    let parent_id = parse_meta.parent.expect("parse_input should have a parent");
    let parent_meta = p
        .spans
        .get(&parent_id)
        .expect("parse_input's parent should resolve");

    assert_eq!(
        parent_meta.name, "handle_request",
        "parse_input's parent should be handle_request, got {:?}",
        parent_meta.name
    );

    // And handle_request itself should be a root (no parent).
    let handle_root_meta = p
        .spans
        .values()
        .find(|m| m.name == "handle_request")
        .expect("handle_request span should be in profile");
    assert_eq!(
        handle_root_meta.parent, None,
        "handle_request is the outermost scope; should have no parent (got {:?})",
        handle_root_meta.parent
    );
}
