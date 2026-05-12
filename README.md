# culpert

> Per-span heap allocation profiler for Rust services.

A `#[global_allocator]` wrapper that attributes every sampled allocation to
the **span** it happened inside, exports pprof-format profiles so the
existing tool ecosystem (stock `pprof`, Speedscope, Pyroscope, Polar Signals)
keeps working, and ships a CLI with bias-corrected reports plus a `diff`
subcommand for CI/PR workflows.

Three integration paths — pick whichever matches your service:

| Your service uses… | Adapter | Cost |
|---|---|---|
| [`foundations`](https://github.com/cloudflare/foundations) | `culpert-foundations` | Zero instrumentation change. Existing `#[span_fn]` annotations become attribution keys. |
| The [`tracing`](https://crates.io/crates/tracing) crate | `culpert-tracing` | Compose a `tracing_subscriber::Layer` into your subscriber stack. Existing `#[tracing::instrument]` annotations become attribution keys. |
| Neither, or you want attribution independent of trace sampling | `culpert::scope` + `#[culpert::span_fn]` | One macro on the functions you want attributed. No external tracer. |

**Status:** pre-release. v0.1 (foundations integration + pprof + CLI) and
v0.2 in-flight work (hierarchy, `diff`, tracing adapter, sampling-independent
attribution) all on `main`; not yet on crates.io. See [`plan.md`](plan.md)
for the design, [`notes.md`](notes.md) for the Phase 0 research verdicts,
[`CHANGELOG.md`](CHANGELOG.md) for what's landed so far, and
[`ROADMAP.md`](ROADMAP.md) for what's planned.

## Quickstart — standalone (`#[culpert::span_fn]`)

The smallest viable setup. No external tracer:

```toml
[dependencies]
culpert = "0.1"
```

```rust
use culpert::{Config, LocalSpanContext, TrackingAllocator};
use std::alloc::System;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

fn main() {
    culpert::install(LocalSpanContext::new(), Config::default());

    handle_request();

    let profile = culpert::snapshot();
    let bytes = culpert::pprof::encode_gzipped(&profile).unwrap();
    std::fs::write("/tmp/prof.pb.gz", &bytes).unwrap();

    culpert::shutdown();
}

#[culpert::span_fn("handle_request")]
fn handle_request() {
    // Every sampled allocation in here, transitively, is attributed to
    // span_name = "handle_request".
}
```

See `examples/macros` for a runnable version.

## Quickstart — with `foundations`

```toml
[dependencies]
culpert              = "0.1"
culpert-foundations  = "0.1"
foundations = { version = "5", default-features = false, features = ["tracing", "telemetry-server"] }
```

The `default-features = false` is **required** — foundations' default
`jemalloc` feature declares its own `#[global_allocator]` which conflicts
with culpert's `TrackingAllocator` and fails to link.

```rust
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

    // ... your existing app, with #[span_fn] annotations as usual.
}
```

Existing `#[foundations::telemetry::tracing::span_fn]` annotations become
attribution keys for free. See `examples/foundations` (minimal) and
`examples/mock-axum` (full HTTP service with `pprof_route`-served profile).

## Quickstart — with the `tracing` crate

```toml
[dependencies]
culpert            = "0.1"
culpert-tracing    = "0.1"
tracing            = "0.1"
tracing-subscriber = "0.3"
```

```rust
use tracing_subscriber::prelude::*;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

fn main() {
    tracing_subscriber::registry()
        .with(culpert_tracing::layer())
        .with(/* your other layers — fmt, OTLP, etc. */)
        .init();

    culpert_tracing::install();

    // ... your existing app, with #[tracing::instrument] annotations as usual.
}
```

Existing `#[tracing::instrument]` annotations become attribution keys. See
`examples/tracing` for a runnable version.

## What you get

A profile (`*.pb.gz`) you can either feed to stock `pprof` or read with the
shipped CLI. Output below is from the `examples/mock-axum` service under
load; the same shape works for any of the three integration paths.

### Tree report — `culpert report <profile>`

The default: hierarchical breakdown, with each sub-span nested under its
parent. Built from `span_parent_id` labels emitted by whichever
`SpanContext` was installed.

```sh
$ culpert report /tmp/mock-axum.pb.gz

Hierarchical span report (143179 samples, sample rate 4.00 KB/alloc):
  Tree shows span_name groupings under their parents. Values are
  bias-corrected estimates. Use --flat for a simple sorted table.

vec                                            1.30 GB   70.48%  (self 1.30 GB)
(no span)                                    246.45 MB   13.07%  (self 246.45 MB)
json                                         180.92 MB    9.59%  (self 180.92 MB)
nested                                         3.55 MB    0.19%  (self 170.62 KB)
├─ parse_payload                              59.77 MB    3.17%  (self 59.77 MB)
├─ validate_payload                           59.57 MB    3.16%  (self 59.57 MB)
└─ build_response                            604.00 KB    0.03%  (self 604.00 KB)
strings                                        5.87 MB    0.31%  (self 5.87 MB)
```

`--flat` switches to the previous sorted-by-bytes table for users who
prefer it. `raw_bytes` is the sum of `Layout::size()` over sampled
allocations (what stock pprof shows). `est_bytes` is the bias-corrected
estimate — every sample below the sample rate counts as `rate_bytes`.

### Drill into one span — `--span <name>`

```sh
$ culpert report /tmp/mock-axum.pb.gz --span json --top 4

Top callsites within span "json" (sample rate 4.00 KB/alloc):
  callsite                                                  samples    raw_bytes    est_bytes
  -------------------------------------------------------  --------  -----------  -----------
  alloc::vec::Vec::from_iter::SpecFromIter                   23000      2.81 MB    89.84 MB
  alloc::fmt::format::{closure}                              23000    314.45 KB    89.84 MB
```

### Drill into the unattributed bucket — `--no-span`

Useful for "is this my code's fault, or the runtime's?". Shows the top
callsites of allocations that fired outside any span — i.e. tokio runtime
work, framework internals, foundations' or `tracing`'s own reporters, or
code paths you haven't yet annotated.

```sh
$ culpert report /tmp/mock-axum.pb.gz --no-span --top 4

Top callsites in unattributed samples — outside any span:
  callsite                                                  samples    raw_bytes    est_bytes
  -------------------------------------------------------  --------  -----------  -----------
  cf_rustracing_jaeger::Tag as Clone>::clone                  8720    90.48 KB    34.06 MB
  alloc::vec::Vec::append_elements                            6678   306.56 KB    26.09 MB
  alloc::boxed::Box::new_uninit                               6551   921.23 KB    25.59 MB
  bytes::bytes_mut::BytesMut::reserve_inner                   2307    14.42 MB    16.00 MB
```

### Compare two profiles — `culpert diff`

For CI workflows: diff a "before" and "after" profile by `span_name` with
both an absolute (`--threshold-bytes`) and a relative (`--threshold-pct`)
gate. `--format markdown` produces output you can pipe straight into
`$GITHUB_STEP_SUMMARY`:

```sh
$ culpert diff before.pb.gz after.pb.gz --format markdown

### culpert: allocation diff

- **before:** `before.pb.gz` — total 2.70 MB (estimated)
- **after:**  `after.pb.gz` — total 5.75 MB (estimated)
- **net Δ:** +3.05 MB (+113.21%)

#### Regressions

| Span | Before | After | Δ | Δ% |
|------|-------:|------:|--:|---:|
| `encode_response` | 1.53 MB | 4.58 MB | +3.05 MB | +200.00% |
```

### Stock `pprof` works too

The on-disk format is canonical pprof, so everything in the ecosystem reads
it:

```sh
pprof -tags        /tmp/mock-axum.pb.gz   # totals grouped by span_name + span_id
pprof -tagfocus="span_name:json" -text /tmp/mock-axum.pb.gz
pprof -http=:8090  /tmp/mock-axum.pb.gz   # interactive flame graph + source view
```

## Comparison

| | culpert | foundations `MemoryProfiler` | jemalloc heap prof | dhat | bytehound | heaptrack |
|---|---|---|---|---|---|---|
| Span attribution | ✓ | — | — | — | — | — |
| Allocator | any `GlobalAlloc` | jemalloc only | jemalloc only | dhat-rs alloc | linker-injected | LD_PRELOAD |
| Platform | any | Linux only | Linux only | any | Linux | Linux |
| Output | pprof | pprof | pprof | dhat-format | bytehound-format | heaptrack-format |
| Sampling | yes (~512 KiB) | yes (jemalloc) | yes | full-fidelity | full-fidelity | full-fidelity |
| Overhead (typical service) | ~0% off, ~0–5% on | low | low | extreme | high | high |
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
  Workloads that allocate heavily and do nothing else see meaningful
  per-alloc cost from the stack-capture path; realistic services
  with CPU work between allocations see negligible overhead at the
  default 1-in-512 KiB rate (samples are rare relative to surrounding
  work). Opt in to `StackCaptureStrategy::FramePointer` to drop the
  alloc-heavy overhead by ~91× — see the **Overhead** section.
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

Measured with Criterion on Apple M-series, release builds. Four
configurations: *baseline* (System global allocator), *tracking_off*
(TrackingAllocator, no profiler installed), *tracking_on (BT)*
(TrackingAllocator + installed profiler at default 1-in-512 KiB using
the default `Backtrace` strategy), and *tracking_on (FP)* using
`StackCaptureStrategy::FramePointer`.

| Workload                     | baseline | tracking_off | tracking_on (BT) | tracking_on (FP) |
|------------------------------|----------|--------------|------------------|------------------|
| 200 × (alloc + ~1 µs CPU)    | 19.2 µs  | 17.8 µs      | 18.8 µs          | 17.9 µs          |
| 200 × 64 B allocs (small)    | 3.44 µs  | 4.12 µs      | 3.76 µs          | 3.45 µs          |
| 50 × 1 MiB allocs            | 5.06 µs  | 4.30 µs      | 584 µs           | 6.41 µs          |

The first row is the realistic case (allocation interleaved with real
work); the others are pure-alloc microbenches that emphasise the per-alloc
overhead.

`tracking_off` adds tens of ns per alloc which disappears under any
meaningful CPU work between allocations.

`tracking_on (BT)` is dominated by `backtrace::trace` per sampled alloc.
On the realistic workload most iterations have no samples (only ~1 in
~10 iterations crosses 512 KiB), so the cost is in the noise. On the
`50 × 1 MiB` microbench every allocation samples, so each iteration pays
100 backtrace walks — at ~5 µs each on macOS's libunwind that's 500 µs
per iteration, which is what dominates.

`tracking_on (FP)` opts in to `StackCaptureStrategy::FramePointer` —
a tiny load-and-cmp loop over the frame-pointer chain — and brings the
dense-sampling case from 584 µs to 6.4 µs (~91× faster). On the
realistic workload the difference is within measurement noise because
samples are rare relative to the surrounding CPU work.

```rust
culpert::install(ctx, Config {
    stack_capture_strategy: StackCaptureStrategy::FramePointer,
    ..Default::default()
});
```

Frame-pointer capture is x86_64 / aarch64 only; other targets fall back
to `Backtrace` transparently. Requires
`RUSTFLAGS="-C force-frame-pointers=yes"` on Linux x86_64 release
builds; macOS aarch64 has frame pointers on by default. On Linux
x86_64 without compiled-in frame pointers, `backtrace::trace` uses
DWARF unwinding (slow); FP is expected to be a major win across all
workloads on that platform, though we haven't measured it here.

## Examples

Four standalone examples in [`examples/`](examples) showing each
integration path:

| Example | What it shows | Key wiring |
|---------|---------------|------------|
| [`mock-axum`](examples/mock-axum) | foundations + axum service with `/debug/alloc/profile` HTTP endpoint, plus a load script | `foundations::telemetry::init` + `culpert_foundations::pprof_route` + `#[foundations::span_fn]` |
| [`foundations`](examples/foundations) | minimal foundations wiring without an HTTP server — just init, work, snapshot, write | `culpert_foundations::install` + `#[foundations::span_fn]` |
| [`tracing`](examples/tracing) | `tracing` crate integration via `tracing_subscriber::Layer` | `culpert_tracing::layer()` composed into a Registry + `#[tracing::instrument]` |
| [`macros`](examples/macros) | sampling-independent attribution with no external tracer; also doubles as a BT-vs-FP timing demo (set `CULPERT_FP=1`) | `culpert::LocalSpanContext` + `#[culpert::span_fn]` |

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
