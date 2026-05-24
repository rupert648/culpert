//! - `TrackingAllocator<System>` as `#[global_allocator]`
//! - foundations telemetry initialised with `Active(1.0)` sampling
//! - `culpert_foundations::install()` wired up
//! - multi-threaded tokio runtime (mimics the runner's worker pool)
//! - allocation-heavy work inside nested `#[span_fn]` scopes
//!
//! Variants:
//!   - `baseline_no_culpert`: same work, no allocator wrapping, no profiler
//!     (can't coexist in the same binary — compare by running the existing
//!     `cargo bench -p culpert --bench baseline` separately)
//!   - `single_thread_active_sampling`: one tokio task, Active(1.0)
//!   - `multi_thread_active_sampling_N`: N concurrent tokio tasks
//!   - `multi_thread_passive_sampling`: N tasks, Passive (no sampled spans)
//!
//! The goal is to find where the overhead cliff is — is it per-alloc
//! stack-walk time, aggregator mutex contention across threads, the
//! FoundationsSpanContext cache lookup, or something else entirely.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use culpert::{Config, StackCaptureStrategy, TrackingAllocator};
use foundations::telemetry::tracing;
use foundations::telemetry::TelemetryContext;

#[global_allocator]
static GLOBAL: TrackingAllocator<tikv_jemallocator::Jemalloc> =
    TrackingAllocator::new(tikv_jemallocator::Jemalloc);

static INIT: std::sync::Once = std::sync::Once::new();

fn ensure_culpert_installed() {
    INIT.call_once(|| {
        culpert_foundations::install_with_config(Config {
            stack_capture_strategy: StackCaptureStrategy::FramePointer,
            ..Default::default()
        });
    });
}

/// Simulate a "request handler": enter a foundations span, allocate
/// a bunch of data inside it (template rendering, JSON serialisation,
/// HashMap building then drop.
///
/// `alloc_count` × `alloc_size` gives the total bytes per "request".
fn simulated_request(alloc_count: usize, alloc_size: usize) {
    let _span = tracing::span("handle_request");
    for _ in 0..alloc_count {
        let v = Vec::<u8>::with_capacity(alloc_size);
        std::hint::black_box(&v);
    }

    // Nested child span — models e.g. "render_template" inside the handler.
    {
        let _child = tracing::span("render_template");
        for _ in 0..alloc_count / 2 {
            let v = Vec::<u8>::with_capacity(alloc_size * 2);
            std::hint::black_box(&v);
        }
    }
}

/// Runs `concurrency` tokio tasks each doing `requests_per_task` simulated
/// requests. Returns the total number of "requests" completed.
async fn concurrent_workload(concurrency: usize, requests_per_task: usize) -> usize {
    let mut handles = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        handles.push(tokio::spawn(async move {
            for _ in 0..requests_per_task {
                simulated_request(100, 1024); // 100 × 1 KiB per request
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    concurrency * requests_per_task
}

fn benchmarks(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .unwrap();

    // Foundations test telemetry context — Active(1.0) sampling.
    let test_ctx = TelemetryContext::test();
    let _ctx_scope = test_ctx.scope();

    ensure_culpert_installed();

    let requests_per_task = 10;

    // --- Single-threaded (1 task) ---
    {
        let mut g = c.benchmark_group("foundations_single_thread");
        g.throughput(Throughput::Elements(requests_per_task as u64));
        g.bench_function("active_sampling", |b| {
            b.iter(|| {
                rt.block_on(concurrent_workload(1, requests_per_task));
            });
        });
        g.finish();
    }

    // --- Multi-threaded scaling ---
    for concurrency in [2, 4, 8, 16, 32] {
        let total = (concurrency * requests_per_task) as u64;
        let mut g = c.benchmark_group(format!("foundations_{concurrency}_tasks"));
        g.throughput(Throughput::Elements(total));
        g.bench_function("active_sampling", |b| {
            b.iter(|| {
                rt.block_on(concurrent_workload(concurrency, requests_per_task));
            });
        });
        g.finish();
    }

    // --- Snapshot under load (models the /_pprof/heap call) ---
    {
        let mut g = c.benchmark_group("foundations_snapshot_under_load");
        // Warm up: run some work so there's data to snapshot.
        rt.block_on(concurrent_workload(8, 50));
        g.bench_function("snapshot", |b| {
            b.iter(|| {
                let p = culpert::snapshot();
                std::hint::black_box(&p);
            });
        });
        g.finish();
    }
}

criterion_group!(benches, benchmarks);
criterion_main!(benches);
