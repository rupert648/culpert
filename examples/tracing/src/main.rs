//! Smallest viable culpert + culpert-tracing example.
//!
//! Demonstrates the canonical wiring for the `tracing` crate ecosystem:
//! compose `culpert_tracing::layer()` into a `tracing_subscriber`
//! Registry → `culpert_tracing::install` → work inside
//! `#[tracing::instrument]`-annotated functions → snapshot → write a
//! pprof file.
//!
//! Run it:
//!
//! ```sh
//! cargo run -p example-tracing
//! # writes /tmp/example-tracing.pb.gz
//! cargo run -p culpert-cli --bin culpert -- report /tmp/example-tracing.pb.gz
//! ```

use culpert::{Config, TrackingAllocator};
use std::alloc::System;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

fn main() {
    // Compose culpert-tracing's layer into a Registry. Add your own
    // fmt / OTLP / etc. layers alongside it; the Registry has to be
    // present for LookupSpan-based parent resolution.
    tracing_subscriber::registry()
        .with(culpert_tracing::layer())
        .init();

    culpert_tracing::install_with_config(Config {
        rate_bytes: 4 * 1024,
        stack_depth: 32,
        buffer_capacity: 1 << 16,
        ..Default::default()
    });

    for _ in 0..20 {
        handle_request();
    }

    let profile = culpert::snapshot();
    let bytes = culpert::pprof::encode_gzipped(&profile).expect("gzip pprof");
    let path = "/tmp/example-tracing.pb.gz";
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

    // On macOS, dyld4's TLS destructors run before atexit handlers, so
    // culpert's automatic atexit-based shutdown gate doesn't fire in
    // time and we'd deadlock against tracing-subscriber's internal
    // thread_local management. Calling `culpert::shutdown` here makes
    // it deterministic across platforms.
    culpert::shutdown();
}

#[tracing::instrument]
fn handle_request() {
    let _ = parse_input();
    let _ = build_response();
}

#[tracing::instrument]
fn parse_input() -> Vec<String> {
    (0..50).map(|i| format!("field_{i}")).collect()
}

#[tracing::instrument]
fn build_response() -> String {
    let mut buf = String::with_capacity(1024);
    for i in 0..200 {
        buf.push_str(&format!("line {i:04}\n"));
    }
    buf
}
