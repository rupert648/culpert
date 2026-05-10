//! Run a tiny synthetic workload, take a snapshot, write a gzipped pprof
//! profile to disk. Sanity-check the encoder against the stock pprof tool:
//!
//!   cargo run -p culpert --example dump_pprof -- /tmp/culpert.pb.gz
//!   pprof -text /tmp/culpert.pb.gz
//!   pprof -http=:8081 /tmp/culpert.pb.gz   # interactive
//!
//! Useful as a manual Phase 2 verification step before we have the
//! mock-axum example wired up in Phase 4.

use culpert::{Config, MockSpanContext, SpanId, TrackingAllocator};
use std::alloc::System;
use std::num::NonZeroU64;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

fn span(n: u64) -> SpanId {
    NonZeroU64::new(n).unwrap()
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/culpert.pb.gz".to_string());

    let mock = MockSpanContext::new();
    culpert::install(
        mock.clone(),
        Config {
            rate_bytes: 4 * 1024,
            stack_depth: 32,
            buffer_capacity: 65_536,
        },
    );

    // A handler-and-sub-spans pattern matching plan.md's hierarchy example.
    let handle_request = span(1);
    let decode_input = span(2);
    let render_template = span(3);
    let encode_response = span(4);

    let _g = mock.enter(handle_request, "handle_request", None);

    {
        let _g = mock.enter(decode_input, "decode_input", Some(handle_request));
        for _ in 0..30 {
            let v = Vec::<u8>::with_capacity(20_000);
            std::hint::black_box(&v);
        }
    }

    {
        let _g = mock.enter(render_template, "render_template", Some(handle_request));
        for _ in 0..50 {
            let s = format!("rendered output {}", "x".repeat(2_000));
            std::hint::black_box(&s);
        }
    }

    {
        let _g = mock.enter(encode_response, "encode_response", Some(handle_request));
        for _ in 0..40 {
            let v: Vec<u32> = (0..10_000).collect();
            std::hint::black_box(&v);
        }
    }

    drop(_g);

    let profile = culpert::snapshot();
    let bytes = culpert::pprof::encode_gzipped(&profile).expect("gzip pprof");

    std::fs::write(&path, &bytes).expect("write profile file");

    eprintln!(
        "wrote {} bytes to {} ({} entries, {} spans, {} dropped)",
        bytes.len(),
        path,
        profile.entries.len(),
        profile.spans.len(),
        profile.dropped_samples
    );
    eprintln!();
    eprintln!("verify with:");
    eprintln!("  pprof -text {path}");
    eprintln!("  pprof -tags {path}     # see span_id / span_name labels");
    eprintln!("  pprof -http=:8081 {path}");
}
