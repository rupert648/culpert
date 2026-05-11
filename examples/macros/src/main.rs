//! Smallest viable culpert + `#[culpert::span_fn]` example.
//!
//! No foundations, no `tracing` crate, no external tracer. Attribution
//! comes from culpert's own thread-local scope stack, which means it
//! works regardless of any trace-sampling rate.
//!
//! Run it:
//!
//! ```sh
//! cargo run -p example-macros
//! # writes /tmp/example-macros.pb.gz
//! cargo run -p culpert-cli --bin culpert -- report /tmp/example-macros.pb.gz
//! ```

use culpert::{Config, LocalSpanContext, TrackingAllocator};
use std::alloc::System;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

fn main() {
    culpert::install(
        LocalSpanContext::new(),
        Config {
            rate_bytes: 4 * 1024,
            stack_depth: 32,
            buffer_capacity: 1 << 16,
        },
    );

    for _ in 0..20 {
        handle_request();
    }

    let profile = culpert::snapshot();
    let bytes = culpert::pprof::encode_gzipped(&profile).expect("gzip pprof");
    let path = "/tmp/example-macros.pb.gz";
    std::fs::write(path, &bytes).expect("write profile");

    eprintln!(
        "wrote {} bytes to {} ({} entries, {} spans)",
        bytes.len(),
        path,
        profile.entries.len(),
        profile.spans.len(),
    );
    eprintln!();
    eprintln!("view with:");
    eprintln!("  cargo run -p culpert-cli --bin culpert -- report {path}");
    eprintln!("  pprof -tags {path}");

    // See examples/tracing for the rationale — call shutdown to avoid
    // a deadlock window in process teardown on macOS.
    culpert::shutdown();
}

#[culpert::span_fn("handle_request")]
fn handle_request() {
    let _ = parse_input();
    let _ = build_response();
}

#[culpert::span_fn("parse_input")]
fn parse_input() -> Vec<String> {
    (0..50).map(|i| format!("field_{i}")).collect()
}

#[culpert::span_fn("build_response")]
fn build_response() -> String {
    let mut buf = String::with_capacity(1024);
    for i in 0..200 {
        buf.push_str(&format!("line {i:04}\n"));
    }
    buf
}
