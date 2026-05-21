//! Async version of the `#[culpert::span_fn]` macro example.
//!
//! Demonstrates that `#[culpert::span_fn("name")]` works on `async fn`
//! just as it does on sync functions. Under the hood the macro wraps the
//! async body in a [`culpert::ScopedFuture`] that enters/exits the scope
//! around each `poll()`, so allocations are attributed to the correct
//! span even after the future migrates between tokio worker threads at
//! `.await` points.
//!
//! No foundations, no `tracing` crate, no external tracer. Attribution
//! comes from culpert's own thread-local scope stack.
//!
//! ```sh
//! cargo run --release -p example-macros-async
//! cargo run -p culpert-cli --bin culpert -- report /tmp/example-macros-async.pb.gz
//! ```

use culpert::{Config, LocalSpanContext, TrackingAllocator};
use std::alloc::System;
use std::time::Instant;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

const ITERS: usize = 200;

#[tokio::main]
async fn main() {
    culpert::install(
        LocalSpanContext::new(),
        Config {
            rate_bytes: 4 * 1024,
            stack_depth: 32,
            buffer_capacity: 1 << 16,
            ..Default::default()
        },
    );

    let start = Instant::now();

    // Spawn concurrent tasks — each one runs through the full
    // handle_request pipeline with async attribution.
    let mut handles = Vec::new();
    for _ in 0..ITERS {
        handles.push(tokio::spawn(handle_request()));
    }
    for h in handles {
        h.await.unwrap();
    }

    let elapsed = start.elapsed();
    let per_iter = elapsed / ITERS as u32;
    eprintln!(
        "ran {} async iterations of handle_request in {:.2?} ({:.2?}/iter)",
        ITERS, elapsed, per_iter,
    );

    let profile = culpert::snapshot();
    let bytes = culpert::pprof::encode_gzipped(&profile).expect("gzip pprof");
    let path = "/tmp/example-macros-async.pb.gz";
    std::fs::write(path, &bytes).expect("write profile");

    eprintln!(
        "wrote {} bytes to {} ({} entries, {} spans)",
        bytes.len(),
        path,
        profile.entries.len(),
        profile.spans.len(),
    );

    // Print a quick summary of where allocations landed.
    let mut by_span: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    for entry in &profile.entries {
        let name = entry
            .span
            .and_then(|id| profile.spans.get(&id))
            .map(|m| m.name.as_str())
            .unwrap_or("(no span)");
        *by_span.entry(name).or_default() += entry.bytes_total;
    }
    let mut sorted: Vec<_> = by_span.into_iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.1));
    eprintln!();
    eprintln!("allocation attribution:");
    for (name, bytes) in &sorted {
        eprintln!("  {name:30} {bytes:>12} bytes");
    }

    eprintln!();
    eprintln!("view with:");
    eprintln!("  cargo run -p culpert-cli --bin culpert -- report {path}");
    eprintln!("  pprof -tags {path}");

    culpert::shutdown();
}

/// Top-level request handler. Calls async sub-operations that yield
/// across `.await` points — allocations in each are attributed to
/// their respective spans, not to `handle_request` or `(no span)`.
#[culpert::span_fn("handle_request")]
async fn handle_request() {
    let _input = parse_input().await;
    let _response = build_response().await;
    let _fetched = fetch_data().await;
}

/// Simulates parsing work with allocations after an async yield.
#[culpert::span_fn("parse_input")]
async fn parse_input() -> Vec<String> {
    // Yield to the executor — may resume on a different worker thread.
    tokio::task::yield_now().await;
    (0..50).map(|i| format!("field_{i}")).collect()
}

/// Simulates building a response with allocations after a sleep.
#[culpert::span_fn("build_response")]
async fn build_response() -> String {
    tokio::time::sleep(std::time::Duration::from_micros(10)).await;
    let mut buf = String::with_capacity(1024);
    for i in 0..200 {
        buf.push_str(&format!("line {i:04}\n"));
    }
    buf
}

/// Simulates an async data fetch with multiple yield points.
#[culpert::span_fn("fetch_data")]
async fn fetch_data() -> Vec<u8> {
    // First yield.
    tokio::task::yield_now().await;
    let mut data = Vec::with_capacity(4096);

    // Second yield — the future may move to yet another thread.
    tokio::time::sleep(std::time::Duration::from_micros(10)).await;
    data.extend_from_slice(&[0u8; 2048]);

    data
}
