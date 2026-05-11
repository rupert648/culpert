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

#### 3. `culpert-tracing` adapter

A `culpert-tracing` crate mapping the [`tracing`](https://crates.io/crates/tracing)
crate's `Span` IDs into culpert's `SpanContext`. Opens culpert to ~the
entire Rust async ecosystem outside Cloudflare. The architecture was
deliberately set up for this in v0.1 — the core is span-source-agnostic
and the foundations adapter is the working blueprint. Mostly an
adapter implementation.

**Where it lives today:** `plan.md` § "Out of scope".

---

### Tier 2 — performance & correctness

#### 4. Frame-pointer-based stack capture

Replace `backtrace::trace` (the dominant cost of profiling-on overhead
per [`plan.md`](plan.md) § Phase 6) with a tiny `mov`+`cmp` loop walking
frame pointers on x86_64 / aarch64. Plan-noted target: realistic-workload
overhead drops from +24 % to roughly +5 %. Gated on
`RUSTFLAGS="-C force-frame-pointers=yes"` (already standard for production
Rust binaries).

**Where it lives today:** noted as a v0.2 mitigation in `plan.md`
§ Phase 6, alluded to in `README.md` overhead section.

#### 5. Geometric sampling

Replace the current counter-mod sampler with `next_sample_interval ~
Geometric(1/rate)` so each *byte* has independent 1/rate probability of
being sampled. Then `bytes_total` IS the unbiased estimate by
construction — the CLI's `estimated_bytes` correction goes away,
`raw_bytes` becomes the only column users need. Cleaner stats and
smaller mental model. Modest implementation cost (one `rand` call per
sample on the slow path).

**Where it lives today:** the bias-correction logic in
`culpert-cli/src/main.rs` notes the trade-off in its doc-comment;
`README.md` Phase 6 section mentions the bias indirectly.

#### 6. Sampling-independent attribution

Today `FoundationsSpanContext::current_span` gates on
`span_is_sampled() == true`. With foundations tracing at 1 % sampling,
99 % of allocations land in `(no span)`. Two paths:

- **Mint our own SpanIds at scope-enter** regardless of foundations'
  trace sampling. Requires intercepting scope creation, which means
  shipping `#[culpert::span_fn]` (a sibling of foundations' `span_fn`)
  for spans users care about.
- **Use the Arc pointer with a "did we just see this" disambiguator**
  to distinguish legit reuse from fresh span. Fragile; haven't fully
  designed.

The macro path is cleaner. Trade: opt-in instrumentation change for
users on low trace-sampling rates.

**Where it lives today:** `notes.md` Q2 verdict, `CHANGELOG.md` "Known
limits" #2.

---

### Tier 3 — polish

| | What | Status today |
|---|------|--------------|
| 7 | `FoundationsSpanContext` metadata cache eviction (currently grows monotonically over service lifetime — every new request adds an entry; long-running services need bounding) | `clear_metadata()` method exists; doc-comment notes the issue. No automatic eviction. |
| 8 | Strip capture-machinery frames in the **encoder**, not just the CLI display logic, so stock `pprof -text` shows real user code at the leaf instead of `backtrace::trace` | not started |
| 9 | Customisable pprof label keys (currently hardcoded `"span_id"` / `"span_name"`) | not started |
| 10 | Load `culpert::Config` from foundations' `TelemetrySettings` so it's one config tree, not two | not started |
| 11 | `pprof_route` allocates a `BoxFuture` per request — cheap on the telemetry server but worth pooling for very high request rates | not started |

---

## Suggested ordering for v0.2

1. ~~**Span hierarchy** (Tier 1 #2)~~ — **shipped.**
2. ~~**`culpert diff`** (Tier 1 #1)~~ — **shipped (flat).** Hierarchical
   diff stays a polish item.
3. **Frame-pointer capture** (Tier 2 #4) — next. Removes the loudest
   production complaint; visible in the README's overhead numbers.
4. **`tracing` adapter** (Tier 1 #3) — broadens beyond foundations.
5. Then opportunistically: geometric sampling, metadata eviction,
   hierarchical diff polish, JSON diff output.

Hierarchy + diff together is what makes v0.2 a real second release —
that core is now in. Tier 2 / 3 items are improvements rather than
new capabilities.

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
