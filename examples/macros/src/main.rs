//! Smallest viable culpert + `#[culpert::span_fn]` example.
//!
//! No foundations, no `tracing` crate, no external tracer. Attribution
//! comes from culpert's own thread-local scope stack, which means it
//! works regardless of any trace-sampling rate.
//!
//! Also doubles as a frame-pointer-vs-backtrace demo. Set
//! `CULPERT_FP=1` to opt into `StackCaptureStrategy::FramePointer`;
//! leave it unset (or `=0`) to use the default `Backtrace` strategy.
//! The example times the work loop so you can compare the two side by
//! side:
//!
//! ```sh
//! cargo run --release -p example-macros            # backtrace
//! CULPERT_FP=1 cargo run --release -p example-macros  # frame pointer
//! ```
//!
//! Then inspect the profile:
//!
//! ```sh
//! cargo run -p culpert-cli --bin culpert -- report /tmp/example-macros.pb.gz
//! ```

use culpert::{Config, LocalSpanContext, StackCaptureStrategy, TrackingAllocator};
use std::alloc::System;
use std::time::Instant;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

// Bigger iteration count so the BT-vs-FP timing difference is visible
// over the noise of release builds. With 1-in-4 KiB sampling, the heavy
// route below samples on roughly every iteration.
const ITERS: usize = 2000;

fn main() {
    let use_fp = std::env::var("CULPERT_FP").is_ok_and(|v| v != "0");
    let strategy = if use_fp {
        StackCaptureStrategy::FramePointer
    } else {
        StackCaptureStrategy::Backtrace
    };
    eprintln!("stack capture strategy: {strategy:?} (toggle via CULPERT_FP=1)");

    culpert::install(
        LocalSpanContext::new(),
        Config {
            rate_bytes: 4 * 1024,
            stack_depth: 32,
            buffer_capacity: 1 << 16,
            stack_capture_strategy: strategy,
        },
    );

    let start = Instant::now();
    for _ in 0..ITERS {
        handle_request();
    }
    let elapsed = start.elapsed();
    let per_iter = elapsed / ITERS as u32;
    eprintln!(
        "ran {} iterations of handle_request in {:.2?} ({:.2?}/iter)",
        ITERS, elapsed, per_iter,
    );

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
