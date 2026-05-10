//! Q2: Stable span identity (and Q4: lazy span metadata).
//!
//! Q2: "Does `TelemetryContext` expose a stable span ID? If not, do we mint
//!      our own and track it via the lifecycle hooks?"
//! Q4: "Once we have a span ID at alloc time, can we resolve name/attributes
//!      lazily at export time without holding refs?"
//!
//! Approach: enter a span, pull the underlying `Arc<RwLock<Span>>` via the
//! public `tracing::rustracing_span()` API, snapshot what we need, then
//! drop the span and prove the snapshot survives.

use foundations::reexports_for_macros::cf_rustracing::span::InspectableSpan;
use foundations::telemetry::TelemetryContext;
use foundations::telemetry::tracing::{self, rustracing_span};
use std::sync::Arc;

pub fn run() {
    println!("Q2: stable span identity (covers Q4 lazy metadata)");
    println!("--------------------------------------------------");

    // Use the test telemetry context: under the noop harness,
    // rustracing_span() returns Inactive spans (empty name, no span_id, fresh
    // Arc each call). The test context exercises the real Tracked path.
    let test_ctx = TelemetryContext::test();
    let _ctx_scope = test_ctx.scope();

    // No active span — outside any tracing::span scope.
    let outside = rustracing_span();
    println!(
        "outside scope: rustracing_span() = {}",
        if outside.is_none() { "None" } else { "Some(_)" }
    );

    // We snapshot inside the scope, then drop the scope.
    let snapshot = {
        let _root = tracing::span("root_q2");
        let s = rustracing_span().expect("expected current span inside scope");
        let ptr = Arc::as_ptr(&s) as usize;
        let g = s.read();
        let name = g.operation_name().to_owned();
        let id = g.context().map(|c| c.state().span_id());
        drop(g);
        println!("inside scope:  ptr=0x{ptr:x}, name={name:?}, span_id={id:?}");

        // Hierarchy: enter a child and confirm it has a distinct identity.
        {
            let _child = tracing::span("child_q2");
            let s2 = rustracing_span().unwrap();
            let ptr2 = Arc::as_ptr(&s2) as usize;
            let name2 = s2.read().operation_name().to_owned();
            println!("child scope:   ptr=0x{ptr2:x}, name={name2:?}");
            assert_ne!(ptr, ptr2, "child should have a distinct Arc identity");
        }

        // After the child drops, the current span should be back to the root.
        let s3 = rustracing_span().unwrap();
        let ptr3 = Arc::as_ptr(&s3) as usize;
        let name3 = s3.read().operation_name().to_owned();
        let id3 = s3.read().context().map(|c| c.state().span_id());
        println!("post-child:    ptr=0x{ptr3:x}, name={name3:?}, span_id={id3:?}");
        // Two ways to test "back to root":
        //   - by Arc identity (only stable for Tracked/Untracked variants)
        //   - by name + span_id (stable across all variants)
        assert_eq!(name3, name, "post-child name should match root by name");

        (name, id, ptr)
    };
    // Span is dropped here; snapshot lives on.

    println!(
        "after drop:    snapshot.name={:?}, snapshot.span_id={:?}, snapshot.ptr=0x{:x}",
        snapshot.0, snapshot.1, snapshot.2
    );

    println!();
    println!("VERDICT (Q2): stable identity is reachable per-sample via rustracing_span().");
    println!("              Either Arc::as_ptr (cheap, lifetime-of-Arc stable) or the");
    println!("              cf-rustracing span_id (when sampled) works as the key.");
    println!("              Inactive spans (NullSampler / unsampled) get a fresh Arc each");
    println!("              call — for those, mint our own monotonic id at first sight.");
    println!("VERDICT (Q4): yes — clone the operation_name into our own owned String at");
    println!("              first sight and stash it in a cache. No Arc retention needed.");
}
