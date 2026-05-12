//! End-to-end test of the core: drives the full pipeline as a real user
//! would.
//!
//! - install `TrackingAllocator` as the global allocator
//! - install a `MockSpanContext`
//! - allocate inside several spans, on multiple threads (some short-lived)
//! - snapshot and assert attribution + metadata + drain semantics
//!
//! Single `#[test]` function because `culpert::install` is process-global
//! and panics on a second call.

use culpert::{Config, MockSpanContext, Profile, SpanId, TrackingAllocator};
use std::alloc::System;
use std::num::NonZeroU64;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

fn span(n: u64) -> SpanId {
    NonZeroU64::new(n).unwrap()
}

fn bytes_for(profile: &Profile, span_id: Option<SpanId>) -> u64 {
    profile
        .entries
        .iter()
        .filter(|e| e.span == span_id)
        .map(|e| e.bytes_total)
        .sum()
}

fn samples_for(profile: &Profile, span_id: Option<SpanId>) -> u64 {
    profile
        .entries
        .iter()
        .filter(|e| e.span == span_id)
        .map(|e| e.samples)
        .sum()
}

#[test]
fn end_to_end_attribution() {
    let mock = MockSpanContext::new();

    // Tight sample rate for a tight test. 4 KiB sampling = ~125 samples per
    // 500KB of allocation. Buffer is large so we don't drop on the cargo-test
    // machinery's chatter.
    let config = Config {
        rate_bytes: 4 * 1024,
        stack_depth: 16,
        buffer_capacity: 65_536,
        ..Default::default()
    };
    culpert::install(mock.clone(), config);

    // ----- Phase A: single-thread, two spans + outside-span -------------

    let s1 = span(1);
    {
        let _g = mock.enter(s1, "span_one", None);
        for _ in 0..50 {
            let v = Vec::<u8>::with_capacity(10_000);
            std::hint::black_box(&v);
        }
    }

    let s2 = span(2);
    {
        let _g = mock.enter(s2, "span_two", None);
        for _ in 0..50 {
            let v = Vec::<u8>::with_capacity(10_000);
            std::hint::black_box(&v);
        }
    }

    for _ in 0..30 {
        let v = Vec::<u8>::with_capacity(10_000);
        std::hint::black_box(&v);
    }

    let p1 = culpert::snapshot();
    assert!(!p1.entries.is_empty(), "expected non-empty profile");

    let bytes_s1 = bytes_for(&p1, Some(s1));
    let bytes_s2 = bytes_for(&p1, Some(s2));
    let bytes_none = bytes_for(&p1, None);

    eprintln!(
        "p1: s1={bytes_s1}B s2={bytes_s2}B none={bytes_none}B dropped={}",
        p1.dropped_samples
    );

    // Each span allocated ~500 KB. With 4 KiB sampling we expect a healthy
    // count; assert generously to avoid flake.
    assert!(bytes_s1 > 100_000, "span 1 attribution too low: {bytes_s1}");
    assert!(bytes_s2 > 100_000, "span 2 attribution too low: {bytes_s2}");

    // Spans 1 and 2 allocated identically; their sample counts should be
    // within an order of magnitude.
    let s1_count = samples_for(&p1, Some(s1));
    let s2_count = samples_for(&p1, Some(s2));
    let ratio = (s1_count.max(s2_count)) as f64 / (s1_count.min(s2_count).max(1)) as f64;
    assert!(
        ratio < 4.0,
        "s1/s2 sample counts differ by >4x: {s1_count} vs {s2_count}"
    );

    // Metadata resolved.
    let m1 = p1.spans.get(&s1).expect("span 1 metadata missing");
    assert_eq!(m1.name, "span_one");
    let m2 = p1.spans.get(&s2).expect("span 2 metadata missing");
    assert_eq!(m2.name, "span_two");

    // Outside-span samples exist (our 30 + test machinery).
    assert!(
        bytes_none > 0,
        "expected at least some samples outside any span"
    );

    // ----- Phase B: multi-thread, four worker spans ---------------------

    let mock2 = mock.clone();
    let thread_handles: Vec<_> = (10..14u64)
        .map(|i| {
            let mock_t = mock2.clone();
            let id = span(i);
            std::thread::spawn(move || {
                let _g = mock_t.enter(id, &format!("thread_span_{i}"), None);
                for _ in 0..50 {
                    let v = Vec::<u8>::with_capacity(10_000);
                    std::hint::black_box(&v);
                }
                id
            })
        })
        .collect();

    let thread_spans: Vec<SpanId> = thread_handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();

    let p2 = culpert::snapshot();

    // Each thread span should have caught samples.
    for id in &thread_spans {
        let b = bytes_for(&p2, Some(*id));
        eprintln!("p2: thread span {id} bytes={b}");
        assert!(b > 50_000, "thread span {id:?} attribution too low: {b}");

        // And metadata was registered.
        let m = p2.spans.get(id).expect("thread span metadata missing");
        assert!(m.name.starts_with("thread_span_"));
    }

    // Snapshot 1 drained spans 1/2: snapshot 2 should not see them again.
    assert_eq!(
        bytes_for(&p2, Some(s1)),
        0,
        "span 1 should be drained by p1"
    );
    assert_eq!(
        bytes_for(&p2, Some(s2)),
        0,
        "span 2 should be drained by p1"
    );

    // ----- Phase C: snapshot drains everything --------------------------

    let p3 = culpert::snapshot();
    for id in &thread_spans {
        assert_eq!(
            bytes_for(&p3, Some(*id)),
            0,
            "thread span {id:?} should be drained by p2"
        );
    }

    // ----- Phase D: span hierarchy --------------------------------------

    let outer = span(100);
    let inner = span(101);
    {
        let _g_outer = mock.enter(outer, "outer", None);
        {
            let _g_inner = mock.enter(inner, "inner", Some(outer));
            for _ in 0..50 {
                let v = Vec::<u8>::with_capacity(10_000);
                std::hint::black_box(&v);
            }
        }
        // Allocations after _g_inner drops belong to outer again.
        for _ in 0..50 {
            let v = Vec::<u8>::with_capacity(10_000);
            std::hint::black_box(&v);
        }
    }

    let p4 = culpert::snapshot();
    let bytes_outer = bytes_for(&p4, Some(outer));
    let bytes_inner = bytes_for(&p4, Some(inner));
    eprintln!("p4: outer={bytes_outer}B inner={bytes_inner}B");
    assert!(
        bytes_outer > 100_000,
        "outer span attribution too low: {bytes_outer}"
    );
    assert!(
        bytes_inner > 100_000,
        "inner span attribution too low: {bytes_inner}"
    );

    let m_outer = p4.spans.get(&outer).expect("outer metadata");
    let m_inner = p4.spans.get(&inner).expect("inner metadata");
    assert_eq!(m_outer.name, "outer");
    assert_eq!(m_inner.name, "inner");
    assert_eq!(m_outer.parent, None);
    assert_eq!(m_inner.parent, Some(outer));
}
