//! Smallest viable culpert + culpert-foundations example.
//!
//! Demonstrates the canonical wiring without spinning up an HTTP server:
//! `foundations::telemetry::init` → `culpert_foundations::install` →
//! work inside `#[span_fn]`-instrumented functions → snapshot → write
//! a pprof file. See `examples/mock-axum` for the full HTTP-serving
//! version with the `/debug/alloc/profile` route.
//!
//! Run it:
//!
//! ```sh
//! cargo run -p example-foundations
//! # writes /tmp/example-foundations.pb.gz
//! cargo run -p culpert-cli --bin culpert -- report /tmp/example-foundations.pb.gz
//! ```

use culpert::{Config, TrackingAllocator};
use foundations::service_info;
use foundations::telemetry::settings::TelemetrySettings;
use foundations::telemetry::tracing::span_fn;
use foundations::telemetry::{TelemetryConfig, init};
use std::alloc::System;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

#[tokio::main]
async fn main() {
    // Disable the telemetry server for this example — we just want
    // foundations' tracing harness up so the adapter sees sampled spans.
    let mut settings = TelemetrySettings::default();
    settings.server.enabled = false;

    let _driver = init(TelemetryConfig {
        service_info: &service_info!(),
        settings: &settings,
        custom_server_routes: vec![],
    })
    .expect("foundations init");

    culpert_foundations::install_with_config(Config {
        rate_bytes: 4 * 1024,
        stack_depth: 32,
        buffer_capacity: 1 << 16,
        ..Default::default()
    });

    // Drive the workload.
    for _ in 0..20 {
        handle_request();
    }

    let profile = culpert::snapshot();
    let bytes = culpert::pprof::encode_gzipped(&profile).expect("gzip pprof");
    let path = "/tmp/example-foundations.pb.gz";
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

    // Mark culpert as shutting down so the observer bails on any
    // allocations triggered by TLS destructors during process exit.
    // Avoids a deadlock that mostly affects macOS, where dyld4 runs
    // TLS finalizers before atexit handlers.
    culpert::shutdown();
}

#[span_fn("handle_request")]
fn handle_request() {
    let _ = parse_input();
    let _ = build_response();
}

#[span_fn("parse_input")]
fn parse_input() -> Vec<String> {
    (0..50).map(|i| format!("field_{i}")).collect()
}

#[span_fn("build_response")]
fn build_response() -> String {
    let mut buf = String::with_capacity(1024);
    for i in 0..200 {
        buf.push_str(&format!("line {i:04}\n"));
    }
    buf
}
