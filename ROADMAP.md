# Roadmap

Forward-looking plan for culpert beyond v0.1. For where v0.1 stands, see
[`CHANGELOG.md`](CHANGELOG.md). For the locked architectural decisions
and v0.1 design rationale, see [`plan.md`](plan.md). For Phase 0
research, see [`notes.md`](notes.md).

## Status

v0.1 is feature-complete per `plan.md` Phases 0–7 but not yet published
to crates.io. v0.2 is the next planning horizon.

---

## v0.2 — make it the regression-prevention tool

Three tiers, in order of likely impact on adoption.

### Tier 1 — headline features

#### 1. `culpert diff` CLI — **shipped (flat)**

Compare two pprof profiles by `span_name` and emit a regression /
improvement report. Two output formats: text (terminal) and markdown
(designed for PR comments; pipe into `$GITHUB_STEP_SUMMARY`).
Configurable absolute (`--threshold-bytes`) and relative
(`--threshold-pct`) thresholds — both gates must pass for a row to
surface. NEW and GONE spans are called out explicitly.

What's deferred to a v0.2.x polish round:
- **Hierarchical diff** — regressions nested under their parent span,
  using the same tree builder as `culpert report`. Flat is enough for
  the headline value (PR comments).
- **JSON output.** Useful for downstream machine consumption; tabled
  until someone asks.
- **Statistical confidence bands.** Single-sample variance at 1-in-N
  is ~±sqrt(B/N)·N per span. A future version could compute the gate
  threshold from sampling theory instead of arbitrary defaults.
- **`--format=pprof`** to write a delta-pprof — possible, niche.

#### 2. Span hierarchy / proper parent tracking — **shipped (Path B)**

v0.1 set `parent: None` on every foundations-sourced span because
cf-rustracing's parent references appeared private. They weren't —
`cf_rustracing::span::InspectableSpan::references()` is public and
returns `&[SpanReference<T>]`, each of which has `is_child_of()` and
`span()` exposing the parent's `SpanContextState`. Path B was the right
call: no user instrumentation change, no opt-in macro.

What landed:

- `FoundationsSpanContext::current_span` now extracts the first
  `ChildOf` reference's `span_id` and stores it as the parent in the
  cached `SpanMetadata`.
- The pprof encoder emits a `span_parent_id` numeric label on every
  sample whose metadata has a parent.
- `culpert report` defaults to a tree view (box-drawing `├─` / `└─` /
  `│`), built by walking the `span_parent_id` labels. `--flat` falls
  back to the previous sorted-by-bytes table.
- Foundations integration test now asserts parent extraction
  end-to-end.

#### 3. `culpert-tracing` adapter — **shipped**

A new `culpert-tracing` crate mirroring the foundations adapter:
`culpert_tracing::layer()` returns a `tracing_subscriber::Layer` that
captures span name + parent at creation time, and
`culpert_tracing::install()` registers a `TracingSpanContext` that
resolves `tracing::Span::current().id()` plus the layer-captured
metadata. Compose the layer into the subscriber stack alongside
whatever other layers the user already has.

The architecture was deliberately set up for this in v0.1 — the core
is span-source-agnostic and the foundations adapter was the working
blueprint.

---

### Tier 2 — performance & correctness

#### 4. Frame-pointer-based stack capture — **shipped**

New `Config::stack_capture_strategy` field selects between
`StackCaptureStrategy::Backtrace` (the default — `backtrace::trace`)
and `StackCaptureStrategy::FramePointer` (a tiny `mov`+`cmp` loop over
the frame-pointer chain on x86_64 / aarch64; other targets fall back
to `Backtrace`).

What landed:

- `culpert::stack_capture::capture(strategy, depth) -> SmallVec<[usize; 32]>`
  with the FP walk dispatched via `std::cfg_select!` on `target_arch`.
- Inline asm reads `rbp` / `x29` with `nomem, nostack, preserves_flags`.
- Bounds check `[sp, sp + 16 MiB)` plus strict-monotonic-increase
  termination keeps the walk safe even when frame pointers aren't
  guaranteed; worst case it returns early.
- New `tracking_on_fp` Criterion bench mirrors `tracking_on` for direct
  comparison.

Measured on Apple M-series: 91× faster on the dense-sampling microbench
(584 µs → 6.4 µs); within measurement noise on realistic alloc + CPU
workloads where samples are rare. Linux x86_64 without compiled-in
frame pointers (`RUSTFLAGS="-C force-frame-pointers=yes"`) falls back to
DWARF-based backtrace; FP is expected to be a major win there but
isn't measured in this repo.

Default remains `Backtrace` for compatibility. Opt in:

```rust
culpert::install(ctx, Config {
    stack_capture_strategy: StackCaptureStrategy::FramePointer,
    ..Default::default()
});
```

#### 5. Geometric sampling — **shipped**

Replaces the v0.1 deterministic counter-mod rearm with a fresh
`Geometric(1/rate_bytes)` draw on each sample (Go / jemalloc-style —
reset, no debt carry-over). The aggregator applies the Bernstein
correction `bytes / (1 − exp(−bytes/rate))` per sample when computing
`ProfileEntry::bytes_total`, so the value in the encoded pprof is
itself an unbiased estimator of total bytes allocated for that
`(span, callsite)` bucket.

What landed:

- New crate-private `culpert::rng::geometric_interval(rate_bytes)`
  backed by `fastrand` (zero transitive deps, thread-local seeded from
  OS entropy on first use → different sequence per run, per thread).
- Sampler rearms via the geometric draw; thread state's first-sample
  position also drawn from the same distribution (avoids biasing
  short-lived threads).
- Aggregator applies the Bernstein correction once when bucketing
  raw samples into `ProfileEntry`s.
- `culpert report` drops the three-column raw/samples/est layout in
  favour of two columns (`samples / bytes`); `culpert diff` compares
  `bytes_total` directly.
- New integration test (`culpert/tests/geometric_sampling.rs`) runs
  a known workload 30 times and asserts the mean estimate is within
  5 % of true total (theoretical Monte-Carlo error ~0.1 %).

Hot-path cost is unchanged (still one branch + one subtraction).
Slow path adds one `f64::ln` per fired sample, on the order of tens
of ns — negligible vs the stack-capture cost.

#### 6. Sampling-independent attribution — **shipped (sync)**

`culpert::scope::enter(name) -> Scope` is the runtime entry point;
`#[culpert::span_fn("name")]` (from the new `culpert-macros` proc-macro
crate, re-exported at `culpert::span_fn`) is the ergonomic wrapper.
Each `enter` mints a fresh culpert-owned `SpanId`, pushes it onto a
thread-local stack with parent linkage, and snapshots metadata.
`LocalSpanContext` reads from that stack — no dependency on any
external tracer's sampling rate.

The aggregator was upgraded to walk parent chains transitively when
building `Profile.spans`, so the tree report works end-to-end even
when a parent span had no direct samples of its own.

**Sync-only in v0.2.** The macro emits a compile error on `async fn`.
A `ScopedFuture` wrapper with careful parent-capture-on-construction
semantics is a v0.2.x follow-up (so the macro can support async
without subtle parent-resolution surprises).

---

### Tier 3 — polish

| | What | Status today |
|---|------|--------------|
| 7 | ~~`FoundationsSpanContext` metadata cache eviction~~ — **shipped** as `SpanContext::on_snapshot` hook; foundations / tracing / local-scope adapters all clear their caches automatically at end of snapshot. |
| 8 | Strip capture-machinery frames in the **encoder**, not just the CLI display logic, so stock `pprof -text` shows real user code at the leaf instead of `backtrace::trace` | not started |
| 9 | Customisable pprof label keys (currently hardcoded `"span_id"` / `"span_name"`) | not started |
| 10 | Load `culpert::Config` from foundations' `TelemetrySettings` so it's one config tree, not two | not started |
| 11 | `pprof_route` allocates a `BoxFuture` per request — cheap on the telemetry server but worth pooling for very high request rates | not started |

---

## Suggested ordering for v0.2

1. ~~**Span hierarchy** (Tier 1 #2)~~ — **shipped.**
2. ~~**`culpert diff`** (Tier 1 #1)~~ — **shipped (flat).** Hierarchical
   diff stays a polish item.
3. ~~**`tracing` adapter** (Tier 1 #3)~~ — **shipped.**
4. ~~**Sampling-independent attribution** (Tier 2 #6)~~ — **shipped (sync).**
   Async support via `ScopedFuture` is a v0.2.x follow-up.
5. ~~**Frame-pointer capture** (Tier 2 #4)~~ — **shipped.** Default is
   still `Backtrace`; opt in via `Config::stack_capture_strategy`.
6. ~~**Geometric sampling** (Tier 2 #5)~~ — **shipped.** Bernstein-
   corrected unbiased `bytes_total`; CLI dropped to two columns.
7. ~~**Metadata cache eviction**~~ — **shipped** as
   `SpanContext::on_snapshot`.
8. Then opportunistically: hierarchical diff polish, JSON diff output,
   async `#[culpert::span_fn]`, encoder-side machinery-frame stripping.

The v0.2 marquee is fully in: hierarchy, diff, broader-ecosystem reach
(`tracing`), sampling-independent attribution, FP-based capture, and
unbiased geometric sampling. Remaining work is polish.

---

## v0.3+ horizon

These are mentioned in [`plan.md`](plan.md) § "Out of scope" and
parked until there's a concrete need:

- **Tail-bucketed profiling** — break samples into latency buckets so
  you can ask "which spans dominate p99 allocations?". Significant
  complexity; unclear whether the demand is there.
- **Per-request hard memory limits** — different product, won't ship
  here.
- **Live time-series dashboard** — use Pyroscope, Grafana, or
  Polar Signals on culpert's pprof output instead.
- **CPU profiling, lock contention, fragmentation analysis** — wrong
  product family. `pprof-rs` / `samply` / jemalloc stats already exist.
