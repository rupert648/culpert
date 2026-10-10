# culpert

> [!WARNING]  
> Culpert is still very experimental and in development, use in production at your own risk.

## Introduction

> Per-span heap allocation profiler for Rust services.

The rust ecosystem already has many great tools for continous benchmarking, take https://bencher.dev/ as one example.
However I felt it was lacking in an easy to use tool to get the same kind of results for allocation performance/regressions. At my day job
we run some pretty hot, high load systems where memory usage is important. We already make extensive use of CPU/heap profiling in production,
but I wanted an earlier feedback loop before the code even hits prod.

Initially, I just chucked a script to pull down the profile at the end of a long running acceptance test run and did some analysis on the pprof generated, but
analysing this, and crucially comparing it to previous runs was a bit of a pain. If I wanted to check the hot paths or see the function stacks we cared
about, I had to maintain a hand crafted lift of these function names then have some cursed bash script to build the necessary pprof commands.
In my opinion, this was immediate code smell and extra maintenance burden. We already know which functions performance we care about - we trace them!

Hence I built culpert (cul~prit~pert). Below gives technical schpiel/a technical definition of what it does, but essentially it acts as a heap profiler
which automatically tags allocation samples with their already existing spans - from tracing, foundations or the in house culpert macro. It then ships
a CLI for beautiful diffing so you can compare and contrast heap allocations on your important functions between runs, with no extra config.

See [culpert-archive](https://github.com/rupert648/culpert-archive) for an example cloudflare worker for storing the collected pprofs based on commit hash, such that you can bake this stuff
into CI. I also then provide an [example action](https://github.com/rupert648/culpert/blob/main/.github/actions/culpert-diff/action.yml) so you can see how this could
be integrated into your CI.

Culpert has already helped us capture multiple allocation regressions and even a memory leak before we hit production. We can, at a glance
decide if a regression in allocations is intentional and worthwhile, and likewise see if our efforts to reduce allocations pay off.

I'd also like to note that this, more than anything, was a learning project for myself to understand the world of heap allocations and stack tracing. This
was built, and is maintained, in my free time. That said, if you have any feature requests or find any bugs, please open an issue.

## What is it.

A `#[global_allocator]` wrapper that attributes every sampled allocation to
the **span** it happened inside, exports pprof-format profiles so the
existing tool ecosystem (stock `pprof`, Speedscope, Pyroscope, Polar Signals)
keeps working, and ships a CLI with unbiased per-span reports plus a `diff`
subcommand for CI/PR workflows. Geometric sampling with the Bernstein
correction means `bytes` figures are directly meaningful — no separate
"raw vs estimated" columns to interpret.

Three integration paths — pick whichever matches your service:

| Your service uses… | Adapter | Cost |
|---|---|---|
| [`foundations`](https://github.com/cloudflare/foundations) | `culpert-foundations` | Zero instrumentation change. Existing `#[span_fn]` annotations become attribution keys. |
| The [`tracing`](https://crates.io/crates/tracing) crate | `culpert-tracing` | Compose a `tracing_subscriber::Layer` into your subscriber stack. Existing `#[tracing::instrument]` annotations become attribution keys. |
| Neither, or you want attribution independent of trace sampling | `culpert::scope` + `#[culpert::span_fn]` | One macro on the functions you want attributed. No external tracer. |

## Install

Library:

```sh
cargo add culpert                # core
cargo add culpert-foundations    # if your service uses foundations
cargo add culpert-tracing        # if your service uses the tracing crate
```

`culpert-macros` is re-exported by `culpert` (don't depend on it directly).

CLI:

```sh
cargo install --locked culpert-cli
# → installs the `culpert` binary into ~/.cargo/bin
culpert --help

# Optional feature for talking to a culpert-archive instance that
# sits behind Cloudflare Access — adds --cf-access-client-id /
# --cf-access-client-secret flags (with CF_ACCESS_CLIENT_ID /
# CF_ACCESS_CLIENT_SECRET env-var fallbacks) to `upload` / `pull`:
cargo install --locked --features cloudflare-access culpert-cli
```

## Quickstart — standalone (`#[culpert::span_fn]`)

The smallest viable setup. No external tracer:

```toml
[dependencies]
culpert = "0.1.6"
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
culpert              = "0.1.6"
culpert-foundations  = "0.1.6"
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
  Tree shows span_name groupings under their parents. `bytes` is the
  Bernstein-corrected, unbiased estimate of allocated bytes (see
  CHANGELOG: geometric sampling). Use --flat for a simple sorted table.

vec                                            1.30 GB   70.48%  (self 1.30 GB)
(no span)                                    246.45 MB   13.07%  (self 246.45 MB)
json                                         180.92 MB    9.59%  (self 180.92 MB)
nested                                         3.55 MB    0.19%  (self 170.62 KB)
├─ parse_payload                              59.77 MB    3.17%  (self 59.77 MB)
├─ validate_payload                           59.57 MB    3.16%  (self 59.57 MB)
└─ build_response                            604.00 KB    0.03%  (self 604.00 KB)
strings                                        5.87 MB    0.31%  (self 5.87 MB)
```

`--flat` switches to a sorted-by-bytes table for users who prefer it.
The `bytes` column is the unbiased estimate of total bytes allocated
under each span — each underlying sample is weighted by
`1 / (1 − exp(−bytes/rate))` (the Bernstein correction for geometric
sampling). No raw column is shown: with geometric sampling the
corrected value is the only one that means anything meaningful.

### Drill into one span — `--span <name>`

```sh
$ culpert report /tmp/mock-axum.pb.gz --span json --top 4

Top callsites within span "json" (sample rate 4.00 KB/alloc):
  callsite                                                  samples         bytes  bytes %
  -------------------------------------------------------  --------  ------------  -------
  alloc::vec::Vec::from_iter::SpecFromIter                   23000      89.84 MB   50.00%
  alloc::fmt::format::{closure}                              23000      89.84 MB   50.00%
```

### Drill into the unattributed bucket — `--no-span`

Useful for "is this my code's fault, or the runtime's?". Shows the top
callsites of allocations that fired outside any span — i.e. tokio runtime
work, framework internals, foundations' or `tracing`'s own reporters, or
code paths you haven't yet annotated.

```sh
$ culpert report /tmp/mock-axum.pb.gz --no-span --top 4

Top callsites in unattributed samples — outside any span:
  callsite                                                  samples         bytes  bytes %
  -------------------------------------------------------  --------  ------------  -------
  cf_rustracing_jaeger::Tag as Clone>::clone                  8720      34.06 MB   33.74%
  alloc::vec::Vec::append_elements                            6678      26.09 MB   25.84%
  alloc::boxed::Box::new_uninit                               6551      25.59 MB   25.34%
  bytes::bytes_mut::BytesMut::reserve_inner                   2307      16.00 MB   15.85%
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

To suppress spans that allocate less than **20 MiB in both profiles**, add
`--min-span-bytes 20971520` (default: `0`, disabled). A span reaching exactly
20 MiB on either side remains eligible, including new and disappeared spans.
The existing delta-size and percentage gates still apply; whole-profile totals
include all spans. JSON retains suppressed rows as `quiet`. In `--tree` output,
the minimum applies to the displayed subtree totals, including children.

```sh
culpert diff before.pb.gz after.pb.gz --format markdown \
  --min-span-bytes 20971520 --threshold-bytes 1048576 --threshold-pct 10
```

CI can set `CULPERT_MIN_SPAN_BYTES=20971520` instead of passing the flag. An
explicit `--min-span-bytes` overrides the environment, including `0` to disable
it. CI must install a CLI release containing this option.

### Stock `pprof` works too

The on-disk format is canonical pprof, so everything in the ecosystem reads
it:

```sh
pprof -tags        /tmp/mock-axum.pb.gz   # totals grouped by span_name + span_id
pprof -tagfocus="span_name:json" -text /tmp/mock-axum.pb.gz
pprof -http=:8090  /tmp/mock-axum.pb.gz   # interactive flame graph + source view
```

### Persist profiles for CI — `culpert upload` / `culpert pull`

For CI workflows you usually want last week's profiles to compare against.
The companion [`culpert-archive`](https://github.com/rupert648/culpert-archive)
Cloudflare Worker stores `.pb.gz` files keyed by commit SHA; the
`upload` / `pull` subcommands of `culpert-cli` are its first-party
client. Endpoint and token are picked up from environment variables
(`CULPERT_ARCHIVE` / `CULPERT_TOKEN`), commit SHA / branch from
`GITHUB_SHA` / `GITHUB_REF_NAME` so the GitHub Actions step body is
short:

```sh
# Push the just-captured profile under this commit
culpert upload /tmp/profile.pb.gz

# Pull last build's main-branch profile as a baseline. --allow-missing
# exits 0 (writing nothing) on 404 so the very first main run doesn't
# fail the build.
culpert pull --latest-of main -o /tmp/baseline.pb.gz --allow-missing

# Diff. Markdown to $GITHUB_STEP_SUMMARY, plus a fail-gated text run.
culpert diff /tmp/baseline.pb.gz /tmp/profile.pb.gz \
  --format markdown >> "$GITHUB_STEP_SUMMARY"
culpert diff /tmp/baseline.pb.gz /tmp/profile.pb.gz \
  --threshold-bytes 1048576 --threshold-pct 10   # exit 1 on regression
```

For drop-in CI use, this repo ships a reusable **composite action**
that wraps the whole flow:

```yaml
# In your workflow, after you've captured a profile:
- uses: rupert648/culpert/.github/actions/culpert-diff@main
  with:
    archive-url:   ${{ vars.CULPERT_ARCHIVE }}
    archive-token: ${{ secrets.CULPERT_TOKEN }}
    profile:       /tmp/my-service-profile.pb.gz
    culpert-cli:   ./target/release/culpert    # path to the binary
```

That one block does: `culpert info` (sanity check in the run log) →
`culpert pull --latest-of main --allow-missing` → `culpert diff
--format markdown` (posted to `$GITHUB_STEP_SUMMARY` and, on
`pull_request` events, as a sticky PR comment) → `culpert upload` as
the new baseline. Override defaults with the action's inputs —
`baseline-branch`, `threshold-bytes`, `threshold-pct`,
`fail-on-regression`, etc. See
[`.github/actions/culpert-diff/action.yml`](.github/actions/culpert-diff/action.yml)
for the full input schema.

Culpert's own [`rust.yml`](.github/workflows/rust.yml) profiles
`example-macros` and invokes the same action — that's the worked
example. Currently warn-only (`fail-on-regression: "false"`) until
enough main-branch runs have accumulated to make gating meaningful.

The worker never parses the pprof bytes — it's dumb storage. Sample
attribution, Bernstein correction, threshold logic all run in this CLI.
See [culpert-archive's README](https://github.com/rupert648/culpert-archive)
for the deploy recipe and HTTP surface.

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

## License

Dual-licensed under MIT or Apache-2.0.
