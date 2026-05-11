//! Like `dump_pprof`, but uses `culpert::LocalSpanContext` and
//! `#[culpert::span_fn]` — no foundations, no tracing crate, no external
//! trace sampling. Demonstrates that the sampling-independent
//! attribution path produces a usable profile.

use culpert::{Config, LocalSpanContext, TrackingAllocator};
use std::alloc::System;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

#[culpert::span_fn("decode_input")]
fn decode_input() -> Vec<Vec<u8>> {
    (0..30).map(|_| Vec::<u8>::with_capacity(20_000)).collect()
}

#[culpert::span_fn("render_template")]
fn render_template() -> String {
    let mut s = String::new();
    for _ in 0..50 {
        s.push_str(&format!("rendered output {}", "x".repeat(2_000)));
    }
    s
}

#[culpert::span_fn("encode_response")]
fn encode_response() -> u64 {
    let mut total = 0u64;
    for _ in 0..40 {
        let v: Vec<u32> = (0..10_000).collect();
        total = total.wrapping_add(v.iter().map(|&x| x as u64).sum::<u64>());
    }
    total
}

#[culpert::span_fn("handle_request")]
fn handle_request() {
    let _ = std::hint::black_box(decode_input());
    let _ = std::hint::black_box(render_template());
    let _ = std::hint::black_box(encode_response());
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/culpert-local.pb.gz".to_string());

    culpert::install(
        LocalSpanContext::new(),
        Config {
            rate_bytes: 4 * 1024,
            stack_depth: 32,
            buffer_capacity: 1 << 16,
        },
    );

    handle_request();

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
    eprintln!("  ./target/debug/culpert report {path}");
    eprintln!("  pprof -tags {path}");
}
