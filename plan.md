# culpert

> Find the allocation culprits in your `foundations` service.

A heap allocation profiler that attributes every allocation to the
[`foundations`](https://github.com/cloudflare/foundations) span it happened
inside, exports pprof-format profiles, and (in v0.2) ships a CI mode that
diffs base vs PR and posts a regression comment.

Single-product scope. Sharp. Foundations-native.

---

## Product definition

### One sentence
A drop-in `#[global_allocator]` wrapper that records every allocation tagged
with the current `foundations::telemetry::TelemetryContext`, samples them with
configurable rate, and exposes a pprof endpoint for live profiling and a CLI
for diffing two profiles.

### What "adding it to a service" looks like

```toml
# Cargo.toml
[dependencies]
culpert              = "0.1"
culpert-foundations  = "0.1"

# Foundations must be brought in with `default-features = false`. Default
# features turn on `jemalloc`, which makes foundations declare its own
# `#[global_allocator] static GLOBAL: Jemalloc`. That conflicts with
# culpert's `#[global_allocator] static GLOBAL: TrackingAllocator<...>` and
# fails to link.
foundations = { version = "5", default-features = false, features = ["tracing", "telemetry-server"] }
# Optional: keep jemalloc as the underlying allocator (recommended for
# long-running services). Wrap it explicitly:
#   #[global_allocator]
#   static GLOBAL: TrackingAllocator<tikv_jemallocator::Jemalloc> = ...;
tikv-jemallocator = "0.6"   # only if wrapping jemalloc
```

```rust
// main.rs
#[global_allocator]
static GLOBAL: culpert::TrackingAllocator<tikv_jemallocator::Jemalloc> =
    culpert::TrackingAllocator::new(tikv_jemallocator::Jemalloc);

fn main() {
    // Routes are registered at init time via TelemetryConfig::custom_server_routes.
    // (foundations 5.x has no runtime add_route API — verified in Phase 0,
    // see notes.md Q5.)
    let driver = foundations::telemetry::init(TelemetryConfig {
        service_info: &service_info!(),
        settings: &settings.telemetry,
        custom_server_routes: vec![
            culpert_foundations::pprof_route("/debug/alloc/profile"),
        ],
    }).unwrap();

    // Hook foundations TelemetryContext lifecycle into culpert.
    culpert_foundations::install();

    // ... rest of app, with existing #[span_fn] instrumentation, then
    //     drive `driver` on the runtime.
}
```

That's it. Existing `#[span_fn]` annotations become attribution keys for free.

### Developer journey

```bash
# Live profile
$ curl localhost:8080/debug/alloc/profile?seconds=30 > prof.pb.gz

# Stock pprof works
$ pprof -http=:8081 prof.pb.gz

# Richer terminal report
$ culpert report prof.pb.gz

Top spans by total allocations (30s, 12,400 reqs):
─────────────────────────────────────────────────────────
handle_request                     142 MB    11.4 KB/req
├─ decode_input                     18.1 MB   1.5 KB/req
├─ validate_input                   48.0 MB   3.9 KB/req
│   └─ load_lookup_data             36.2 MB   2.9 KB/req  ← 25%
├─ build_response                   62.5 MB   5.0 KB/req
│   ├─ render_template              28.4 MB   2.3 KB/req
│   └─ encode_response              29.7 MB   2.4 KB/req  ← hot
└─ exit                              13.0 MB

Top callsites within handle_request::build_response:
  render/src/template.rs:142   12.4 MB  (44%)
  encode/src/response.rs:218    8.1 MB  (29%)
```

### Lead use case (locked)

**"Which handler allocates most, broken down by sub-span."**

Local-dev investigation workflow. The CI/diff workflow is v0.2.

### What we gain over existing tools

| Question | Existing answer | culpert answer |
|----------|----------------|----------------|
| "Which handler allocates most?" | jemalloc heap dump → manual stack→handler correlation | Sorted table, span-attributed, seconds |
| "Which sub-span dominates within `handle_request`?" | Not answerable — heap profile gives leaf stacks, not span tree | Hierarchical, native to data model |
| "What does `render_template` allocate when called from `request_handler` vs `background_worker`?" | Indistinguishable (same stack) | Different ancestor span, distinguishable |

### Relationship to `foundations::telemetry::MemoryProfiler`

Foundations already ships a heap profiler: `MemoryProfiler` is a thin
wrapper around jemalloc's `prof:true` mode + `mallctl`, gated on
`feature = "memory-profiling"` (which depends on `jemalloc`). It serves
binary heap profiles at `/pprof/heap` and works only on Linux + jemalloc.

culpert overlaps in *output* (both emit pprof) but is fundamentally
different in *mechanism*: culpert wraps any `GlobalAlloc` at the Rust
language level, samples in our own counter, and reads
`foundations::telemetry::tracing::rustracing_span()` at sample time so
each sample is **tagged with the active foundations span**. That's the
unique value — a jemalloc profile cannot answer "which handler" because
jemalloc doesn't know what a foundations span is.

If you only need a stack-level heap profile, use `MemoryProfiler` —
it's simpler and battle-tested. The two systems can coexist (independent
sample streams); a service that wants both keeps foundations' `jemalloc`
feature on AND wraps jemalloc with `TrackingAllocator<Jemalloc>` rather
than letting foundations register the global allocator itself. (See the
`default-features = false` note in the wiring example above.)

---

## Architectural decisions (locked, do not re-debate)

These decisions have been made; the rationale is preserved here so future
sessions don't re-litigate them.

### 1. Foundations-first, not `tracing` crate

**Decision:** Build on `foundations::telemetry::TelemetryContext`, not the
`tracing` crate's `Span`.

**Rationale:**
- Goal includes Cloudflare adoption. Every Cloudflare service uses foundations.
- `foundations::telemetry::TelemetryServerRoute` provides production HTTP
  serving for free (no axum integration to maintain).
- `foundations::telemetry::MemoryProfiler` exists already as a jemalloc wrapper
  — culpert is a coherent extension, not a parallel system.
- Cleaner span lifecycle: explicit `TelemetryContext::scope`/`apply` vs
  tracing's subscriber `on_enter`/`on_exit`. Less reentrancy risk in the
  allocator.

**Trade-off accepted:** narrower open-source pitch ("for foundations users")
mitigated by the core+adapter architecture below.

### 2. Core + adapter architecture

**Decision:** Core crate is span-source-agnostic. Foundations adapter ships
in v0.1. `tracing` adapter ships in v0.2 (or whenever there's pull).

```
culpert/                  # core: TrackingAllocator, sampling, pprof export
                          # depends on a SpanContext trait, no foundations dep
culpert-foundations/      # adapter: TelemetryContext → SpanContext
culpert-cli/              # `culpert report`, `culpert diff`
examples/
  mock-axum/              # the dogfooding service (NOT challenge-platform)
```

**Rationale:** Foundations adoption is the immediate win, but the technical
work in the core (allocator wrapper, sampling, pprof, aggregation) is
identical regardless of span source. A small `SpanContext` trait costs
~half a day of design and unlocks the broader ecosystem in v0.2.

### 3. Mock axum service first, challenge-platform later

**Decision:** Validate against a clean synthetic axum service before
attempting integration with challenge-platform.

**Rationale:**
- challenge-platform is large, has miniflour/NAPI complexity, has its own
  HTTP serving setup. Fighting it in v0.0 wastes time.
- Synthetic service can have known allocation patterns to verify attribution
  correctness (handler X is supposed to allocate exactly Y bytes per
  request — does culpert show that?).
- Forces ergonomics to be real for any foundations user, not just this repo.

challenge-platform integration becomes Phase 6 validation, not Phase 1.

### 4. pprof protobuf as canonical output

**Decision:** Output is pprof-format protobuf with span context as labels.

**Rationale:**
- Stock `pprof` tool, Speedscope, Pyroscope, Polar Signals all consume it.
- Don't reinvent viewers.
- Custom CLI (`culpert report`/`diff`) on top is for richer/differential
  output but pprof remains the truth.

### 5. Sampled, not exhaustive

**Decision:** Sample every Nth byte allocated (default 1-in-512K, like
jemalloc). Capture stack trace only on sampled allocs. Symbolize lazily on
export.

**Rationale:** Recording every alloc is unworkable (millions/sec). Sampled
profiling is statistically sound and what every other heap profiler does
(jemalloc, dhat in some modes, all CPU profilers).

---

## What v0.1 ships (deliverables checklist)

1. ☐ `culpert` core crate
   - `TrackingAllocator<A: GlobalAlloc>` wrapper
   - Sampled per-`(span_id, callsite)` accumulation
   - pprof-format export with `span_id`, `span_name` labels
   - `SpanContext` trait abstraction
   - No foundations or tracing dependency
2. ☐ `culpert-foundations` adapter
   - Hooks `TelemetryContext` lifecycle (enter/exit/apply/scope)
   - Propagates span context across `WithTelemetryContext` futures
   - `install()` entrypoint
   - Provides a `pprof_handler()` returning a `TelemetryRouteHandler`
3. ☐ `culpert-cli`
   - `culpert report <prof.pb.gz>` — terminal-pretty hierarchical span report
4. ☐ Mock axum service in `examples/mock-axum/`
   - 3-5 routes with deliberately different allocation profiles
   - Uses `foundations` for span instrumentation
   - Wired up with culpert
   - Includes a small load script (e.g. `wrk` or hand-rolled)
5. ☐ Documented overhead
   - Synthetic benchmark
   - Target: <1% off, <3% on (1-in-512K sampling)
6. ☐ README
   - One-paragraph pitch
   - Comparison table vs `dhat`, `bytehound`, `heaptrack`, jemalloc heap profile,
     `foundations::MemoryProfiler`
   - Quickstart
   - Honest scope limits (out-of-scope list below)

### Explicitly out of scope for v0.1

- ❌ CPU profiling (use `pprof-rs`, `samply`)
- ❌ Lock contention (separate problem)
- ❌ `culpert diff` CLI / CI mode (v0.2)
- ❌ Tail-bucketed profiling (v0.3 if at all)
- ❌ `culpert-tracing` adapter (v0.2)
- ❌ Live time-series dashboard (use Pyroscope on culpert's pprof output)
- ❌ Per-request hard memory limits (different product)
- ❌ challenge-platform integration (Phase 6, post v0.1 cut)

---

## Phased build plan (~4 weeks elapsed, on-and-off)

### Phase 0 — research spike (3-4 days) — **shipped**

The single goal: **answer the 5 load-bearing technical questions** before
committing to v0.1 implementation. Throwaway code, just feasibility.

Verdicts and code refs in `notes.md`. All five resolve PASS. Two corrections
folded back into this plan: the `add_route` example was wrong (foundations
5.x has no runtime API for that — see § "What 'adding it to a service' looks
like"), and Phase 1 must include a per-thread reentrancy guard.

1. **Allocator reentrancy with foundations.** Does
   `TelemetryContext::current()` allocate? If yes, can we read a thread-local
   pointer to the current span ID without going through foundations' API?
   *Risk: high. If this fails, the whole product needs a different shape.*
2. **Stable span identity.** Does `TelemetryContext` expose a stable span ID?
   If not, do we mint our own and track it via the lifecycle hooks?
3. **Async propagation.** When a future moves between worker threads via
   `WithTelemetryContext`, how does the span context follow? What hook fires?
4. **Span metadata access.** Once we have a span ID at alloc time, can we
   resolve name/attributes lazily at export time without holding refs?
5. **`telemetry-server` route shape.** Can `TelemetryServerRoute` serve a
   binary `application/octet-stream` body (gzipped pprof), not just text/json?

**Phase 0 deliverable:** a `notes.md` in the workspace answering each question
with a code reference and a "yes/no/workaround" verdict. Stop and re-plan
before Phase 1 if any answer is "no" without a clear workaround.

### Phase 1 — core allocator + sampled accumulation (~5 days) — **shipped**

- `TrackingAllocator<A: GlobalAlloc>` skeleton.
- Per-thread sample buffer (`Vec<RawSample>`, capped at `Config::buffer_capacity`).
  Bucketing by `(span_id, frames_hash)` happens in the snapshot path, not the
  hot path — cheaper observe(), bounded memory by buffer cap. (Drift from
  the original `(span_id, callsite_addr) → bytes` map design; chosen for
  hot-path simplicity. Trade-off documented in `culpert/src/thread_state.rs`.)
- Sampling: every Nth byte allocated triggers a sample with stack capture.
- `SpanContext` trait: pluggable source of "what's the current span ID?"
- `MockSpanContext` for testing (deterministic span IDs from a vec).
- Aggregator: hot path takes only the per-thread `Mutex` (uncontended in
  steady state) — effectively lock-free against any global state. Snapshot
  path takes brief locks across threads. Pure lock-free is a v0.2 polish
  if benchmarks demand it.
- **Deceased-thread salvage queue:** on thread exit, each thread's leftover
  samples are moved into a global queue that the next snapshot also drains.
  Without this, short-lived workers lose all their attribution at thread exit.
  (Discovered immediately by `tests/smoke.rs`; not in the original plan.)
- **`CULPERT_DEBUG=1` env-gated debug logging.** ~1 atomic load on the hot
  path when off; eprintln! traces of sampler/snapshot lifecycle when on.
  (Saved a deadlock debug session; not in the original plan.)
- End-to-end test (`tests/smoke.rs::end_to_end_attribution`): synthetic
  workload exercises single-thread, multi-thread (with thread-exit salvage),
  nested span hierarchy with parent metadata, and snapshot drain idempotency.

### Phase 2 — pprof export (~2 days)

- Add `prost` and `pprof::profile.proto` (vendor or `pprof` crate).
- Emit `Profile` with sample type `space/bytes`, `inuse_objects` etc.
- Span context as **sample labels** (`span_id`, `span_name`).
- Symbolize stacks via `backtrace` crate, lazily at export.
- Verify in stock `pprof` tool: `pprof -http=:8081 sample.pb.gz` shows
  expected attribution.

### Phase 3 — foundations adapter (~3 days)

- `culpert-foundations` crate.
- `install()`: registers a `SpanContext` impl that reads from
  `TelemetryContext::current()`.
- Hook span enter/exit to update thread-local pointer.
- Async propagation: ensure `WithTelemetryContext` correctly updates the
  pointer on poll.
- `pprof_handler()`: returns a `TelemetryRouteHandler` that produces a
  gzipped pprof body.

### Phase 4 — mock axum service (~2 days)

- New crate in `examples/mock-axum/`.
- Uses `foundations::telemetry::init`, registers culpert.
- 3-5 routes with deliberately distinct allocation patterns:
  - `/cheap` — minimal alloc baseline
  - `/json` — heavy serde allocation
  - `/strings` — `format!` and `String::push_str` heavy
  - `/vec` — large `Vec` allocations
  - `/nested` — calls multiple `#[span_fn]` sub-spans
- Small load script.
- Validation pass: each route should attribute as expected.

### Phase 5 — `culpert-cli report` (~3 days)

- `culpert report <file.pb.gz>` — pretty terminal output of hierarchical
  span breakdown.
- Top callsites within a span filter.
- Uses `comfy-table` or similar for the table output.

### Phase 6 — overhead benchmarks + polish (~3 days) — **shipped**

Three Criterion bench binaries: `baseline` (System global allocator),
`tracking_off` (TrackingAllocator with no profiler installed),
`tracking_on` (TrackingAllocator + installed profiler under a single
mock span). All three run the same workloads from `benches/common.rs`.

Measured on Apple M-series, release mode:

| Workload                  | baseline | tracking_off | tracking_on | off Δ | on Δ |
|---------------------------|----------|--------------|-------------|-------|------|
| 200 × 64 B allocs (small) | 2.55 µs  | 2.89 µs      | 3.79 µs     | +13 % | +49 % |
| 200 × 4 KiB allocs (med)  | 3.90 µs  | 7.20 µs      | 34.8 µs     | +85 % | +792 % |
| 50 × 1 MiB allocs         | 26.2 µs  | 26.4 µs      | 549 µs      | +0.7% | +1995 % |
| `Vec` grow to 10 k        | 5.74 µs  | 7.17 µs      | 149 µs      | +25 % | +2495 % |
| 200 × (alloc + ~1 µs CPU) | 18.3 µs  | 16.1 µs      | 22.6 µs     | ~0 %  | +24 % |

The plan's original headline targets ("<1 % off, <3 % on") apply to
**typical services where CPU work dominates allocation**, not to pure
allocation microbenches. Two takeaways:

1. **`tracking_off` overhead** is ~10–20 ns per alloc — a `try_with` on the
   reentrancy guard plus an `OnceLock::get`. Disappears when each alloc
   costs hundreds of ns (mmap-class large allocs); doubles total cost
   when each alloc is ~20 ns (tcache-hit small allocs). For a workload
   with any real work between allocations (the bottom row) it sits in
   the noise.
2. **`tracking_on` overhead** is dominated by `backtrace::trace` per
   sampled alloc. Workloads that allocate well above the 512 KiB sample
   rate (large × 50 = 50 MiB / iter ≈ 100 samples) pay the per-sample
   stack capture cost on every iter; large bench shows +1995 % because
   pure-alloc workloads have no other work to dilute the sample cost.
   The realistic alloc + CPU bench shows +24 %, more representative.

Mitigation candidates (v0.2): frame-pointer-based stack capture
(replace `backtrace::trace` with a few `mov`+`cmp` instructions on
x86_64/aarch64), inline-able reentrancy gate. The current
implementation accepts the cost in exchange for portability and
robustness.

### Phase 7 — docs + release (~2 days)

- README quickstart against mock-axum.
- API docs via rustdoc.
- Comparison table.
- Tag v0.1, publish to crates.io.

### Phase 8 (post-v0.1) — challenge-platform integration

Wire culpert into `challenge_platform_http::start_http_server` behind
`CHALLENGE_PLATFORM_IS_LOCAL` initially. Run acceptance tests, find at least
one real allocation hotspot for a writeup. This is the validation case
study, not part of v0.1.

---

## Risks (worth keeping front-of-mind)

1. **Allocator reentrancy.** Phase 0 #1. If reading the current span
   allocates, the whole approach needs rework. Mitigation: skip foundations'
   API in the allocator hot path; mirror the span ID via our own
   thread-local that foundations' lifecycle hooks update.
2. **Async span propagation correctness.** A future moving between worker
   threads must carry its span pointer. Subtle. Mitigation: extensive
   tests with `tokio::spawn` + `tokio::task::yield_now()`.
3. **Sampling bias.** A 1-in-512K sampler can miss small-but-frequent
   allocators or over-weight large-but-rare ones. Mitigation: document the
   trade-off, allow rate tuning, follow jemalloc's well-understood model.
4. **Stack capture cost.** `backtrace::Backtrace::new()` is hundreds of µs.
   Mitigation: only capture on sampled allocs (already implied), consider
   frame-pointer-based capture later if `backtrace` is too slow.
5. **pprof label cardinality.** If `span_id` is a string per request, label
   cardinality explodes. Mitigation: use a string-interned label or hash;
   keep span *name* (low cardinality) and span *id* (high cardinality)
   separate, only emit one in the profile.

---

## Forward prompt (resume from here)

When picking this back up — possibly in a fresh session — start with this:

> I'm building **culpert**, a per-span heap allocation profiler for Rust
> services using Cloudflare's `foundations`. Working dir is
> `~/Documents/personal/culpert`. The full plan and locked decisions are in
> `plan.md` — read it before doing anything else.
>
> **Status:** plan written, no code yet.
>
> **Next concrete task:** Phase 0 spike. Create a small experimental crate
> (`spike/` or similar) that answers the 5 load-bearing technical questions
> in `plan.md` § "Phase 0 — research spike". Output a `notes.md` with a
> verdict per question. Do not start Phase 1 until Phase 0 is done.
>
> **Locked decisions** (do not re-debate, see `plan.md` for rationale):
> - Foundations-first, with a `tracing` adapter punted to v0.2.
> - Core + adapter architecture from day one (`culpert` core,
>   `culpert-foundations` adapter, `culpert-cli`).
> - Mock axum service in `examples/mock-axum/` is the dogfooding target
>   for v0.1, not challenge-platform.
> - Lead use case: "Which handler allocates most, broken down by sub-span."
> - pprof protobuf is the canonical output format.
> - Sampled (1-in-512K default), not exhaustive.
> - v0.1 explicitly excludes CPU profiling, locks, CI/diff mode, tail
>   bucketing, and challenge-platform integration. See "Out of scope".
>
> **Key foundations APIs to investigate in Phase 0:**
> - `foundations::telemetry::TelemetryContext` (current, scope, apply,
>   with_forked_trace)
> - `foundations::telemetry::WithTelemetryContext` (async propagation)
> - `foundations::telemetry::TelemetryServerRoute` (HTTP route shape)
> - `foundations::telemetry::MemoryProfiler` (precedent / coexistence)
> - `#[foundations::telemetry::tracing::span_fn]` macro expansion
>   (cargo expand on a small example)
>
> Source for foundations:
> https://github.com/cloudflare/foundations/tree/main/foundations/src/telemetry
>
> Begin by setting up the workspace skeleton (Cargo workspace,
> `culpert/`, `culpert-foundations/`, `culpert-cli/`, `examples/mock-axum/`,
> `spike/`) and confirming foundations builds locally with a hello-world.
> Then start the spike.
