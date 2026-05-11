# culpert

> Find the allocation culprits in your `foundations` service.

A heap allocation profiler that attributes every sampled allocation to the
[`foundations`](https://github.com/cloudflare/foundations) span it happened
inside, exports pprof-format profiles, and ships a CLI for human-readable
reports.

**Status:** pre-release. v0.1 in flight; not yet on crates.io. See
[`plan.md`](plan.md) for the design, [`notes.md`](notes.md) for the
Phase 0 research verdicts, [`CHANGELOG.md`](CHANGELOG.md) for what's
landed so far, and [`ROADMAP.md`](ROADMAP.md) for what's planned.

## Quickstart

```toml
# Cargo.toml
[dependencies]
culpert              = "0.1"
culpert-foundations  = "0.1"
foundations = { version = "5", default-features = false, features = ["tracing", "telemetry-server"] }
```

The `default-features = false` is **required**: foundations defaults turn on
its `jemalloc` feature, which makes foundations declare its own
`#[global_allocator]`. That conflicts with culpert's `TrackingAllocator` and
fails to link.

```rust
// main.rs
use culpert::TrackingAllocator;
use std::alloc::System;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

#[tokio::main]
async fn main() {
    let driver = foundations::telemetry::init(TelemetryConfig {
        service_info: &service_info!(),
        settings: &settings.telemetry,
        custom_server_routes: vec![
            culpert_foundations::pprof_route("/debug/alloc/profile"),
        ],
    }).unwrap();

    culpert_foundations::install();

    // ... your existing app, with #[span_fn] instrumentation as usual.
}
```

That's it. Existing `#[span_fn]` annotations become attribution keys for free.

## What you get

```sh
$ curl -o /tmp/prof.pb.gz http://localhost:8081/debug/alloc/profile
$ culpert report /tmp/prof.pb.gz

Top spans by allocation (143179 samples, sample rate 4.00 KB/alloc):
  raw_bytes        = sum of Layout::size() over sampled allocations.
  estimated_bytes  = bias-corrected: each sample of size < rate counts for `rate`.

  span                 samples       raw_bytes    raw %       est_bytes    est %
  ----------------  ----------  --------------  -------  --------------  -------
  vec                     3828         1.30 GB   97.29%         1.30 GB   70.48%
  (no span)              60308        29.77 MB    2.18%       246.45 MB   13.07%
  json                   46115         4.34 MB    0.32%       180.92 MB    9.59%
  parse_payload          15300       533.77 KB    0.04%        59.77 MB    3.17%
  validate_payload       15251       470.40 KB    0.03%        59.57 MB    3.16%
  strings                 1318         1.54 MB    0.11%         5.87 MB    0.31%
  nested                   908       230.55 KB    0.02%         3.55 MB    0.19%
  build_response           151       151.00 KB    0.01%       604.00 KB    0.03%
```

```sh
$ culpert report /tmp/prof.pb.gz --span json --top 4

Top callsites within span "json" (sample rate 4.00 KB/alloc):
  callsite                                                  samples    raw_bytes    est_bytes
  -------------------------------------------------------  --------  -----------  -----------
  alloc::vec::Vec::from_iter::SpecFromIter                   23000      2.81 MB    89.84 MB
  alloc::fmt::format::{closure}                              23000    314.45 KB    89.84 MB
```

`--no-span` flips the filter to drill into samples taken outside any
foundations span (tokio runtime, axum/hyper internals, foundations' own
trace reporter):

```sh
$ culpert report /tmp/prof.pb.gz --no-span --top 4

Top callsites in unattributed samples — outside any foundations span:
  callsite                                                  samples    raw_bytes    est_bytes
  -------------------------------------------------------  --------  -----------  -----------
  cf_rustracing_jaeger::Tag as Clone>::clone                  8720    90.48 KB    34.06 MB
  alloc::vec::Vec::append_elements                            6678   306.56 KB    26.09 MB
  alloc::boxed::Box::new_uninit                               6551   921.23 KB    25.59 MB
  bytes::bytes_mut::BytesMut::reserve_inner                   2307    14.42 MB    16.00 MB
```

The output is also a stock pprof file, so anything pprof can do works:

```sh
pprof -tags        /tmp/prof.pb.gz   # totals grouped by span_name + span_id
pprof -tagfocus="span_name:json" -text /tmp/prof.pb.gz
pprof -http=:8090  /tmp/prof.pb.gz   # interactive flame graph + source view
```

## Comparison

| | culpert | foundations `MemoryProfiler` | jemalloc heap prof | dhat | bytehound | heaptrack |
|---|---|---|---|---|---|---|
| Span attribution | ✓ | — | — | — | — | — |
| Allocator | any `GlobalAlloc` | jemalloc only | jemalloc only | dhat-rs alloc | linker-injected | LD_PRELOAD |
| Platform | any | Linux only | Linux only | any | Linux | Linux |
| Output | pprof | pprof | pprof | dhat-format | bytehound-format | heaptrack-format |
| Sampling | yes (~512 KiB) | yes (jemalloc) | yes | full-fidelity | full-fidelity | full-fidelity |
| Overhead (typical service) | ~0% off, ~24% on | low | low | extreme | high | high |
| Pre-existing instrumentation needed | `#[span_fn]` | none | none | none | none | none |

The comparison column that matters: **only culpert produces a profile that
answers "which handler allocates most?" without manual stack→handler
correlation**. That's the entire reason for it. If you don't need per-span
attribution, foundations' `MemoryProfiler` is simpler and battle-tested;
culpert and `MemoryProfiler` can also coexist (independent sample streams)
if you want both.

## Honest scope limits

What culpert currently does **not** do:

- **Low-overhead full-fidelity profiling.** Sampled is the only mode.
  Workloads that allocate heavily and do nothing else see significant
  overhead because each sampled allocation triggers `backtrace::trace`.
  Realistic services with CPU work between allocations see ~+24 % at
  the default 1-in-512 KiB rate. See
  [`plan.md` § Phase 6](plan.md#phase-6--overhead-benchmarks--polish-3-days--shipped);
  frame-pointer-based stack capture is queued for v0.2.x to bring this
  closer to ~+5 %.
- **Async `#[culpert::span_fn]`.** v0.2 ships sync support; the macro
  emits a compile error on `async fn` with a pointer at the alternative
  (use the foundations or tracing adapters for async paths). A
  `ScopedFuture` wrapper with explicit parent-capture-on-construction
  semantics is queued for v0.2.x.
- **Foundations attribution at very low trace sampling rates.** The
  `culpert-foundations` adapter gates on `span_is_sampled()`. With
  foundations' default 100 % sampling this never matters; with low-rate
  sampling, allocs in unsampled traces land in the `(no span)` bucket.
  Workarounds: bump foundations sampling, or use
  `#[culpert::span_fn]` for the spans you care about (it doesn't gate
  on any external tracer's sampling).
- **CPU profiling, lock contention, fragmentation analysis.** Wrong
  product; use [`pprof-rs`](https://crates.io/crates/pprof) /
  [`samply`](https://github.com/mstange/samply) for CPU, jemalloc's
  stats for fragmentation.
- **Auto-resolve transitive parent names in the CLI tree.** When a
  parent span has no direct samples of its own (e.g. a `handle_request`
  body that just orchestrates sub-spans), its sub-spans currently appear
  twice — once nested under the sampled parent instances, once at root.
  The aggregator already walks parent chains transitively for
  `Profile.spans`; the encoder needs to emit a `span_parent_name`
  label so the CLI can resolve cleanly. Polish work queued for v0.2.x.

## Overhead

Measured with Criterion on Apple M-series, release builds. Three modes:
*baseline* (System global allocator), *tracking_off* (TrackingAllocator,
no profiler installed), *tracking_on* (TrackingAllocator + installed
profiler at default 1-in-512 KiB).

| Workload                     | baseline | tracking_off | tracking_on | off Δ | on Δ |
|------------------------------|----------|--------------|-------------|-------|------|
| 200 × (alloc + ~1 µs CPU)    | 18.3 µs  | 16.1 µs      | 22.6 µs     | ~0 %  | +24 % |
| 200 × 64 B allocs (small)    | 2.55 µs  | 2.89 µs      | 3.79 µs     | +13 % | +49 % |
| 50 × 1 MiB allocs            | 26.2 µs  | 26.4 µs      | 549 µs      | +0.7 %| +1995 % |

The first row is the realistic case (allocation interleaved with real
work); the others are pure-alloc microbenches that emphasise the per-alloc
overhead. `tracking_off` adds ~10–20 ns per alloc which disappears under
any meaningful CPU work between allocations. `tracking_on` is dominated by
`backtrace::trace` per sampled alloc — frame-pointer-based capture is on
the v0.2 list.

## Examples

Four standalone examples in [`examples/`](examples) showing each
integration path:

| Example | What it shows | Key wiring |
|---------|---------------|------------|
| [`mock-axum`](examples/mock-axum) | foundations + axum service with `/debug/alloc/profile` HTTP endpoint, plus a load script | `foundations::telemetry::init` + `culpert_foundations::pprof_route` + `#[foundations::span_fn]` |
| [`foundations`](examples/foundations) | minimal foundations wiring without an HTTP server — just init, work, snapshot, write | `culpert_foundations::install` + `#[foundations::span_fn]` |
| [`tracing`](examples/tracing) | `tracing` crate integration via `tracing_subscriber::Layer` | `culpert_tracing::layer()` composed into a Registry + `#[tracing::instrument]` |
| [`macros`](examples/macros) | sampling-independent attribution with no external tracer | `culpert::LocalSpanContext` + `#[culpert::span_fn]` |

Each prints a profile to `/tmp/example-*.pb.gz`. View with
`cargo run -p culpert-cli --bin culpert -- report <path>` or `pprof -tags <path>`.

### Shutdown

Call `culpert::shutdown()` before letting `main` return. It flips an
internal "we're tearing down" flag so the observer ignores any
allocations triggered by TLS destructors during process exit. Without
it, certain combinations (notably the `tracing` crate's `Registry` on
macOS, whose `thread_local`-based per-thread state allocates during
teardown) can deadlock against a recursive `std::sync::Mutex` acquisition.

`culpert::install` registers a `libc::atexit` handler that sets the
same flag automatically — that catches typical Unix and Windows
teardown paths, but **macOS `dyld4`'s TLS finalizers run before
`atexit`**, so an explicit `culpert::shutdown()` is the safe way to
make exit deterministic.

## Architecture

A core crate (`culpert`) does the work in three pieces:

1. **`TrackingAllocator<A>`** — a `GlobalAlloc` wrapper that observes every
   alloc. Samples on a per-thread byte countdown.
2. **`SpanContext` trait** — pluggable source of "what's the active span on
   this thread, right now?". culpert is span-source agnostic.
3. **`Profile` snapshot** — drains every thread's per-thread sample buffer,
   buckets by `(span, callsite)`, resolves symbols, returns a `Profile`.
   `culpert::pprof::encode_gzipped` writes it as a pprof protobuf.

Three production `SpanContext` adapters ship alongside:

- **`culpert-foundations`** reads `foundations::telemetry::tracing::rustracing_span()`
  and uses cf-rustracing's `span_id` (a stable `u64` per span) as
  culpert's identity. Also provides `pprof_route()` — a
  `TelemetryServerRoute` you register at init time.
- **`culpert-tracing`** ships a `tracing_subscriber::Layer` that
  captures span metadata at creation and resolves
  `tracing::Span::current().id()` on the hot path. Compose into your
  existing subscriber stack.
- **`culpert::LocalSpanContext` + `#[culpert::span_fn]`** — sampling-independent
  attribution via culpert's own thread-local scope stack. No external
  tracer, no trace-sampling gating. Sync functions only in v0.2; async
  via `ScopedFuture` is queued for a follow-up.

`culpert-cli` is the report binary. It decodes pprof, groups by
`span_name` label, walks call stacks (skipping capture-machinery frames so
the leaf is real user code), and prints the tables shown above.

`MockSpanContext` lets you drive the system in tests without any adapter.

See [`plan.md`](plan.md) for the locked architectural decisions and
[`ROADMAP.md`](ROADMAP.md) for what's queued next.

## License

Dual-licensed under MIT or Apache-2.0.
