# Changelog

All notable changes to this project will be documented in this file. The
format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

_Nothing yet._

## [0.1.1] — 2026-05-15

### Added

- **`culpert-cli`**: optional `cloudflare-access` cargo feature (off
  by default) that adds `--cf-access-client-id` /
  `--cf-access-client-secret` flags to `upload` and `pull`, with
  `CF_ACCESS_CLIENT_ID` / `CF_ACCESS_CLIENT_SECRET` env-var
  fallbacks. Sets the `CF-Access-Client-Id` /
  `CF-Access-Client-Secret` headers when contacting a
  culpert-archive instance that sits behind Cloudflare Access.
  Builds without the feature don't carry the flags or the
  header-emitting code — backwards-compatible for non-Access
  deployments.

  Install with `cargo install --locked --features cloudflare-access
  culpert-cli` when needed.

- **`culpert-diff` composite action**: new `access-client-id` /
  `access-client-secret` inputs. When set, the action forwards
  them as `CF_ACCESS_CLIENT_ID` / `CF_ACCESS_CLIENT_SECRET`
  env vars to every `culpert-cli` invocation. The caller is
  responsible for installing the CLI with `--features
  cloudflare-access` if they're using Access (the action doesn't
  install the CLI itself).

Other crates (`culpert`, `culpert-macros`, `culpert-foundations`,
`culpert-tracing`) are unchanged at 0.1.1 — re-publish for the
version bump only.

## [0.1.0] — 2026-05-15

First publish to crates.io. Five crates land at the same version:
`culpert`, `culpert-macros`, `culpert-foundations`, `culpert-tracing`,
`culpert-cli`. The CLI is installable via `cargo install --locked
culpert-cli`; libraries via `cargo add culpert` (plus the matching
adapter if you use foundations or tracing).

This release rolls up everything described as "v0.1" and "v0.2" in
the dev history below. Headlines: per-span heap-allocation profiling
with `#[culpert::span_fn]` / foundations / tracing adapters,
geometric sampling with Bernstein-corrected unbiased totals,
frame-pointer stack capture, the `culpert` CLI with `report` / `diff`
/ `info` / `upload` / `pull` subcommands, and a [`culpert-archive`
companion worker](https://github.com/rupert648/culpert-archive)
plus a [composite GitHub
Action](.github/actions/culpert-diff/action.yml) for CI-PR-comment
diffs.

### Added in v0.2 (in-flight on `main`)

- **Span hierarchy.** `FoundationsSpanContext` now extracts parent SpanIds
  from cf-rustracing's `Span::references()`. The pprof encoder emits a
  `span_parent_id` numeric label on every sample whose `SpanMetadata` has
  a parent. `culpert report` defaults to a hierarchical tree view built
  from those labels (`├─` / `└─` / `│` box-drawing); `--flat` falls back
  to the previous sorted-by-bytes table.

- **`culpert diff`.** New subcommand: `culpert diff <before.pb.gz> <after.pb.gz>`
  compares two profiles by `span_name`, computes per-span byte deltas
  (using the bias-corrected estimate), and emits a regression / improvement
  report. Two output formats:
  - **text** (default) — terminal-friendly table.
  - **markdown** (`--format markdown`) — designed for PR comments; a
    GitHub Action can pipe it into `$GITHUB_STEP_SUMMARY`.

  Configurable thresholds:
  - `--threshold-bytes` (default 4 KiB) — minimum absolute change to surface.
  - `--threshold-pct` (default 5.0) — minimum relative change to surface.
  Both gates must pass for a row to appear; changes below either are
  hidden and counted in a summary footer. NEW / GONE spans (one-sided
  presence) are flagged explicitly.

  Errors out if `before` and `after` have different sample rates — they
  aren't directly comparable. Hierarchical diff (regressions nested under
  their parent span) is a polish item, deferred.

- **`culpert-tracing` adapter.** New crate mirroring `culpert-foundations`
  for the `tracing`-crate ecosystem. `culpert_tracing::layer()` returns a
  `tracing_subscriber::Layer` that captures span name + parent at
  creation time; `culpert_tracing::install()` registers a
  `TracingSpanContext` that resolves `tracing::Span::current()` IDs and
  reads the snapshotted metadata. Compose the layer with the rest of
  your subscriber stack (`tracing_subscriber::registry().with(...)`).

- **`culpert::scope` + `#[culpert::span_fn]`** — **sampling-independent
  attribution.** Culpert now ships its own thread-local scope stack and
  `LocalSpanContext` for attribution that doesn't depend on any external
  tracer or its sampling rate.
  - `culpert::scope::enter(name) -> Scope` is the runtime entry point.
  - `#[culpert::span_fn("name")]` (re-exported from a new
    `culpert-macros` proc-macro crate) wraps a sync function body with
    `let _g = culpert::scope::enter(name);`. RAII-popped on any exit
    path (`?`, early `return`, panic).
  - `LocalSpanContext` reads from the scope stack; install with
    `culpert::install(LocalSpanContext::new(), config)`.
  - Hierarchy is captured directly (parent = previous top-of-stack)
    so the tree view works end-to-end without external help.
  - Aggregator now walks parent chains transitively at snapshot time
    so the tree report includes parent spans that themselves had no
    direct samples (e.g. a `handle_request` whose body only orchestrates
    sub-spans).
  - **Sync-only in v0.2.** The macro emits a compile error on `async fn`
    with a pointer to the foundations / tracing adapters; a `ScopedFuture`
    wrapper for clean async parent semantics is a v0.2.x follow-up.

- **CLI improvements for CI integration.** Three new pieces aimed at
  letting `culpert diff` drive PR gates and at making profiles
  self-describing on the wire (so a future culpert-store can key
  artefacts by what's *in* the file rather than out-of-band metadata):

  - **`culpert diff` exit codes.** Exit `1` when any row classifies as
    a regression or NEW after the threshold gates (so CI can gate
    directly: `culpert diff before.pb.gz after.pb.gz || exit 1`).
    Exit `2` for I/O or usage errors. New `--no-fail` flag preserves
    the old "always exit 0" behaviour for steps that want to post the
    report and continue regardless.

  - **`culpert diff --format json`.** Structured output alongside the
    existing `text` / `markdown` formats. Schema is documented in the
    `render_diff_json` doc-comment (top-level keys: `schema_version`,
    `before`, `after`, `rate_bytes`, `thresholds`, `rows`, `summary`)
    and stamped with `schema_version: 1` so downstream consumers can
    pin against breaking changes. Includes any metadata embedded in
    the source profiles (see next bullet).

  - **Profile metadata embedding.** New `Config::metadata:
    HashMap<String, String>` field; whatever the user puts in there
    at install time is written into the pprof's `comment` field as
    `key=value` lines and surfaced by `culpert::pprof::metadata()`.
    Comment order is deterministic (sorted by key) so the on-disk
    bytes don't drift across runs with the same inputs.

  - **`culpert info <file>`** subcommand. Reads the embedded metadata
    + a short summary (sample count, total bytes, sample rate,
    unique-span count) without rendering the full report. Useful for
    CI logs ("uploaded profile for commit abc123") and quick file
    inspection.

  Round-trip + malformed-input tests added to `culpert/src/pprof.rs`.

- **Automatic metadata-cache eviction.** New `SpanContext::on_snapshot()`
  hook with a default no-op impl. The aggregator calls it after every
  `snapshot()`, once the `Profile` has its own cloned copy of every
  span's metadata. The three production adapters override it to drop
  their internal `HashMap<SpanId, SpanMetadata>` caches:
  - `culpert_foundations::FoundationsSpanContext::by_id`
  - `culpert_tracing::Shared::metadata`
  - `culpert::scope::METADATA` (used by `LocalSpanContext` and `#[culpert::span_fn]`)

  Pre-fix the caches grew monotonically over the process lifetime —
  one entry per cf-rustracing / tracing-subscriber / culpert-minted
  span_id, ~70–100 bytes each. A service taking 100 req/s with three
  spans per request leaked ~700 MB/day. New
  `culpert/tests/metadata_eviction.rs` integration test asserts the
  cache is empty after a snapshot across two consecutive trials.

  The hook is a default-empty trait method, so user-defined
  `SpanContext` impls compile unchanged and just don't get auto-eviction
  unless they opt in.

- **Geometric sampling + Bernstein-unbiased per-span totals.** Replaces
  the v0.1 deterministic counter-mod rearm (`+= rate_bytes`) with a
  fresh `Geometric(1/rate_bytes)` draw (Go / jemalloc-style — reset, no
  debt carry-over). The aggregator now applies the standard Bernstein
  correction `bytes / (1 − exp(−bytes/rate))` per sample when computing
  `ProfileEntry::bytes_total`, so the value in the encoded pprof is
  itself an unbiased estimator of total bytes allocated for that
  `(span, callsite)` bucket.

  Visible knock-on changes:
  - `culpert report` drops the three-column `raw_bytes / samples /
    est_bytes` layout in favour of two columns (`samples / bytes`).
    `bytes` is the unbiased estimate; no read-time correction needed.
  - `culpert diff` compares span totals directly without any
    correction logic of its own.
  - One `f64::ln` per fired sample on the slow path (~tens of ns —
    negligible vs stack capture cost). Hot-path cost unchanged.

  New `fastrand = "2.4.1"` workspace dep (zero transitive deps). New
  internal `culpert::rng::geometric_interval(rate_bytes)` helper. New
  integration test `culpert/tests/geometric_sampling.rs` runs a known
  workload across 30 trials and asserts the mean estimate is within
  5% of true total (Monte-Carlo bound is ~0.1%; the wide tolerance
  catches only systematic regressions).

- **Frame-pointer stack capture.** New `Config::stack_capture_strategy`
  field (`StackCaptureStrategy::{Backtrace, FramePointer}`); default
  stays `Backtrace` for compatibility. `FramePointer` swaps
  `backtrace::trace` for a tiny load-and-cmp loop over the frame-pointer
  chain — `mov` from `rbp` (x86_64) / `x29` (aarch64), bounds-check each
  frame against the current `rsp` / `sp` plus a 16 MiB heuristic, walk
  until either a null FP, a non-monotonic FP, or the configured stack
  depth is reached. Dropped the dominant sampling cost from `backtrace::trace`
  (~5 µs on macOS libunwind) to ~50 ns per walk on the same workload —
  see the **Overhead** section of the README for numbers (91× speedup on
  the dense-sampling microbench; in the noise on realistic alloc + CPU
  workloads where samples are rare). x86_64 + aarch64 only; other targets
  transparently fall back to `Backtrace`. Requires
  `RUSTFLAGS="-C force-frame-pointers=yes"` on Linux x86_64 release
  builds; macOS aarch64 has frame pointers on by default.

### Added in v0.1

#### `culpert` (core)

- `TrackingAllocator<A: GlobalAlloc>` — `#[global_allocator]`-installable wrapper
  around any allocator. Pure forwarder until `culpert::install` is called.
- `SpanContext` trait — pluggable source of "what's the active span on this
  thread"; production adapter is `culpert-foundations`, tests use
  `MockSpanContext`.
- Sampled per-thread observe path: byte-counter sampler at 1-in-512 KiB by
  default, configurable via `Config { rate_bytes, stack_depth, buffer_capacity }`.
- Per-thread reentrancy guard so allocations triggered by our own bookkeeping
  short-circuit cleanly. Same guard wraps the snapshot path.
- Per-thread sample buffer + global registry. Snapshot drains every live
  thread plus a deceased-thread queue (samples salvaged on thread exit,
  so short-lived workers don't lose attribution).
- `culpert::install`, `culpert::snapshot`. `Profile` / `ProfileEntry` / `Frame`
  public types.
- `pprof` module: `encode`, `encode_gzipped`, `decode_gzipped`, and the
  canonical `proto::Profile` types (hand-written prost-derived; no `protoc`
  build dependency).
- `CULPERT_DEBUG=1` env-gated tracing of the sampler/snapshot lifecycle.

#### `culpert-foundations`

- `FoundationsSpanContext` — reads `foundations::telemetry::tracing::rustracing_span()`
  and uses cf-rustracing's `span_id` as the stable identity.
- `install()` / `install_with_config()` convenience entry points.
- `pprof_route(path)` builder returning a `foundations::telemetry::TelemetryServerRoute`,
  registerable via `TelemetryConfig::custom_server_routes`.

#### `culpert-cli`

- `culpert report <file.pb.gz>` — top-spans table with three byte columns:
  raw `Layout::size()` sum, sample count, and bias-corrected estimate
  (corrects for counter-mod sampling's bias against small-but-frequent
  allocations).
- `--span <name>` — drill into top callsites within a named span.
- `--no-span` — drill into samples taken outside any foundations span
  (tokio runtime, framework internals, foundations' own trace reporter,
  uninstrumented code paths).
- Capture-machinery frames (backtrace, our sampler, the Rust allocator
  shim) are stripped from displayed leaves so the user sees real code.

#### Examples & benches

- `examples/mock-axum/` — synthetic foundations-instrumented axum service
  with five routes (`/cheap`, `/json`, `/strings`, `/vec`, `/nested`),
  plus `load.sh` to drive it. Used as the v0.1 dogfooding target.
- Three Criterion bench binaries (`baseline`, `tracking_off`, `tracking_on`)
  measuring overhead in the three modes — see the README for numbers.

### Fixed

- TLS teardown ordering: `culpert::sampler::observe` and
  `thread_state::handle` now use `LocalKey::try_with` rather than `with`,
  so allocations triggered during other crates' TLS destructors (notably
  the `thread_local` crate freeing thread IDs) don't panic with
  `AccessError`.
- `FoundationsSpanContext` identity: switched from `Arc<RwLock<Span>>::as_ptr`
  (unstable across span lifetimes — the Arc heap slot gets reused, collapsing
  every subsequent span into the first) to cf-rustracing's `span_id`,
  which is stable per span.
- `mock-axum` ctrl-c handler: `axum::serve(...).with_graceful_shutdown(...)`
  was waiting on HTTP/1.1 keep-alive sockets that the load script never
  closed. Replaced with a `tokio::select!` and `std::process::exit(0)` for
  clean demo shutdown.

### Documented

- `plan.md` — architectural decisions, phase plan, post-Phase-0 corrections
  (no `add_route` API in foundations 5.x; use `TelemetryConfig::custom_server_routes`),
  the relationship to `foundations::telemetry::MemoryProfiler`, the
  `default-features = false` requirement on foundations.
- `notes.md` — Phase 0 research-spike verdicts on the five load-bearing
  questions (allocator reentrancy, stable span identity, async propagation,
  lazy metadata access, binary route bodies).
- README + comparison table vs `dhat`, `bytehound`, `heaptrack`,
  `MemoryProfiler`, jemalloc heap profile.
- Dual MIT/Apache-2.0 license texts.

### Known limits (v0.1)

- No span hierarchy in the report — sub-spans show as siblings of their
  parent. cf-rustracing's parent references aren't surfaced through
  foundations' public API in a usable way; v0.2 work.
- Attribution gated on foundations trace sampling
  (`span_is_sampled() == true`). Allocations in unsampled traces land in
  the `(no span)` bucket. Workaround: bump foundations sampling, or build
  a custom `SpanContext`.
- No `culpert diff` CLI yet — that's the v0.2 headline (PR/CI workflows).
- Pure-allocation microbenches see significant overhead with profiling
  on (each sample triggers `backtrace::trace`). Realistic services see
  ~+24%; v0.2 may add frame-pointer-based stack capture.
