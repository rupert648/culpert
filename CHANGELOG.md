# Changelog

All notable changes to this project will be documented in this file. The
format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Pre-release. Everything below will become the `0.1.0` entry on first publish
to crates.io. Until then, depend on this project from a git URL.

### Added

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
