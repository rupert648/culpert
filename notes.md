# Phase 0 spike — research notes

Answers the five load-bearing technical questions in `plan.md` § "Phase 0 —
research spike". All five resolve **PASS** — Phase 1 is unblocked. Two
plan amendments are recorded at the end.

Foundations version under test: `5.6.5` (current latest on crates.io as of
writing). All file refs are paths into
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/foundations-5.6.5/src/`,
shortened in this doc to e.g. `telemetry/scope.rs:30`.

The empirical evidence comes from `spike/`, an in-tree throwaway crate.

```
cargo run -p spike -- q1
cargo run -p spike -- q2
cargo run -p spike -- q3
cargo run -p spike -- q5
```

---

## Q1 — Allocator reentrancy with `TelemetryContext::current()`

> *"Does `TelemetryContext::current()` allocate? If yes, can we read a
> thread-local pointer to the current span ID without going through
> foundations' API?"*

**Verdict: PASS. Steady state is alloc-free; the very first call on each
fresh thread allocates ~3 cells via lazy thread-local init. A per-thread
re-entrancy guard in `TrackingAllocator` is required — already on the Phase
1 plan.**

### Source-level analysis

`TelemetryContext::current()` (telemetry/telemetry_context.rs:107-118) is
just three thread-local reads bundled into a struct:

```rust
pub fn current() -> Self {
    Self {
        log: current_log(),
        span: current_span(),
        test_tracer: current_test_tracer(),
    }
}
```

Each component reads a `ScopeStack<T>` (telemetry/scope.rs:29-32):

```rust
pub(crate) fn current(&self) -> Option<T> {
    self.0.get_or_default().borrow().last().cloned()
}
```

`ScopeStack` wraps `ThreadLocal<RefCell<Vec<T>>>` from the `thread_local`
crate. Steady-state cost on a thread that has already initialised its cell:

- `ThreadLocal::get_or_default()` — pointer compare, no alloc
- `RefCell::borrow()` — no alloc
- `Vec::last()` — no alloc
- `Option::cloned()` — clones an `Option<SharedSpan>`. `SharedSpan` is an
  `Arc<RwLock<Span>>` (telemetry/tracing/internal.rs:53-72) so this is a
  refcount bump (one atomic increment), no allocation

The first call on a fresh thread additionally pays the
`ThreadLocal::get_or_default()` cell-creation cost (typically a couple of
heap allocations inside `thread_local`).

### Empirical measurement

`spike/src/q1.rs` installs a counting global allocator and probes:

```
steady-state main thread: 100000 calls -> 0 allocs
fresh thread: 1st call -> 3 allocs; next 1000 -> 0 allocs
```

Three allocations on the very first `TelemetryContext::current()` call on
a fresh thread, none thereafter. Matches the source-level analysis.

### Implication for Phase 1

`TrackingAllocator` will run on every `alloc()` call, including ones
triggered by foundations' own thread-local init. We need a per-thread
re-entrancy guard:

```rust
thread_local! {
    static IN_TRACKER: Cell<bool> = const { Cell::new(false) };
}
```

Inside our `alloc()` hook, check `IN_TRACKER.get()` first; if true, fall
straight through to the inner allocator. Otherwise, set the cell, do
attribution work (which may include reading the current span and thus
trigger foundations' lazy init), and clear the cell.

This is standard for tracking allocators (see `dhat-rs`, `bytehound`).

The re-entrancy guard handles a second case as well: foundations' span
push/pop machinery itself can allocate (e.g. `Vec::push` reallocating the
scope stack). Without the guard, this would recurse.

---

## Q2 — Stable span identity

> *"Does `TelemetryContext` expose a stable span ID? If not, do we mint our
> own and track it via the lifecycle hooks?"*

**Verdict: PASS. Multiple paths are reachable. Recommendation: mint our
own monotonic IDs in culpert because foundations' answer changes by
sampling regime.**

### What's reachable through public API

`foundations::telemetry::tracing::rustracing_span()`
(telemetry/tracing/mod.rs:425-427) returns
`Option<Arc<parking_lot::RwLock<Span>>>` for the current span. From there
we have two stable identifiers:

- `Arc::as_ptr(&arc)` — stable for the Arc's lifetime
- `span.context().map(|c| c.state().span_id())` — `u64` from cf-rustracing

Both are demonstrated in `spike/src/q2.rs`:

```
inside scope:  ptr=0x103e2cd00, name="root_q2", span_id=Some(5794150665348171122)
child scope:   ptr=0x103e2cfa0, name="child_q2"
post-child:    ptr=0x103e2cd00, name="root_q2", span_id=Some(5794150665348171122)
```

Returning to the root scope produces the **same** Arc pointer and same
span_id — identity is stable across child enter/exit.

### Caveat: the `Inactive` regime

`SharedSpan` wraps `SharedSpanHandle` which has three variants
(telemetry/tracing/internal.rs:19-23): `Tracked`, `Untracked`, and
`Inactive`. `Inactive` is what we get from the noop tracing harness
(no `init()` call, or `NullSampler` in effect for an unsampled trace).

For `Inactive`, `From<SharedSpanHandle> for Arc<RwLock<Span>>`
(telemetry/tracing/internal.rs:41-51) explicitly says:

```rust
// This is only used in `rustracing_span()`, which should rarely
// need to be called. Allocating a fresh Arc every time is thus fine.
SharedSpanHandle::Inactive => Arc::new(RwLock::new(Span::inactive())),
```

In the noop run (without `TelemetryContext::test()`) we observed exactly
this: every `rustracing_span()` returned a different Arc, all wrapping a
`Span::inactive()` whose `operation_name()` is empty and `context()` is
`None`. Demoed in the first run of `q2` before we switched it to a test
context — kept in mind because production services with low trace sampling
can hit Inactive too.

### Recommended implementation

Don't depend on cf-rustracing identity. **Mint our own monotonic `u64`
span IDs** at culpert's first sight of a (foundations) span scope. Cache
metadata at the same moment.

Two ways to detect "first sight":

1. **Wrap-the-macro path:** ship a `#[culpert::span_fn(name)]` /
   `culpert::span(name)` shim that creates a foundations span scope **and**
   pushes onto culpert's own thread-local span ID stack. Users replace
   `foundations::telemetry::tracing::span_fn` at instrumentation sites
   they care about.

2. **Lazy-on-sample path:** at each sampled allocation, call
   `rustracing_span()` and look up the resulting Arc pointer in a map. On
   miss, mint a new ID and snapshot metadata. This works for
   Tracked/Untracked spans where Arc identity is stable. For Inactive, we
   can fall back to "no attribution" (record under a single `inactive`
   bucket) without breaking the rest of the profile.

Phase 1 ships path 2 (zero instrumentation changes for users — matches the
plan's "drop-in" pitch). Path 1 stays as an optional escape hatch for
services that run with very low trace sampling.

---

## Q3 — Async propagation across worker threads

> *"When a future moves between worker threads via
> `WithTelemetryContext`, how does the span context follow? What hook
> fires?"*

**Verdict: PASS by construction. `WithTelemetryContext::poll()` re-enters
the scope on every poll, so the per-thread current-span resolves correctly
on whichever worker is polling. No extra wiring needed in culpert.**

### Source-level proof

`telemetry/telemetry_context.rs:36-44`:

```rust
impl<T> Future for WithTelemetryContext<'_, T> {
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let _telemetry_scope = self.ctx.scope();
        self.inner.as_mut().poll(cx)
    }
}
```

`ctx.scope()` (telemetry/telemetry_context.rs:167-178) returns a
`TelemetryScope` whose drop pops the span off the per-thread stack
(`telemetry/scope.rs:56-62`). So every poll: push current span, run user
code, pop. While the user code runs (including any allocations it
triggers), `current_span()` resolves correctly on whatever worker is
polling.

### Empirical confirmation

`spike/src/q3.rs` enters span `"root_q3"` on the parent thread, spawns a
task that yields and sleeps to force migration, reads the span name from
inside the task:

```
outer: thread=ThreadId(1), span_name="root_q3"
inside: thread=ThreadId(5), span_name="root_q3", ptr=0x1039ad210
inside: child span name="child_in_task"
```

Task runs on a *different* worker (ThreadId 5 vs 1), still resolves to the
correct outer span name, and a child span pushed inside the task is also
visible. The mechanism works.

### Implication for Phase 1

None. We just call `rustracing_span()` (or the lighter
`current_span()` if we end up exposing/forking it) on each sample event.
Foundations does the propagation work for us.

---

## Q4 — Lazy span metadata access at export time

> *"Once we have a span ID at alloc time, can we resolve name/attributes
> lazily at export time without holding refs?"*

**Verdict: PASS via snapshot-at-first-sight. We do **not** need to hold the
Arc.**

### Approach

When culpert mints a new span ID (Q2 path 2 above), in the same code path
we read `arc.read().operation_name()` and copy it into a culpert-owned
`String` in a metadata cache keyed by our minted ID. Same for parent ID
(`span.parent()` exists on `cf-rustracing::span::Span` via `InspectableSpan`)
and any tags we want to retain.

Keeping the Arc alive in our cache would prevent the span from being
collected by foundations' active-roots tracker, which we don't want
(memory growth, interference with `/debug/traces`). Snapshotting is
strictly better.

### Empirical confirmation

`spike/src/q2.rs` snapshots inside the scope, then drops the scope, then
prints the snapshot:

```
inside scope:  ptr=0x103e2cd00, name="root_q2", span_id=Some(5794150665348171122)
...
after drop:    snapshot.name="root_q2", snapshot.span_id=Some(5794150665348171122), snapshot.ptr=0x103e2cd00
```

Snapshot survives drop of the underlying scope as expected.

### Implication for Phase 1

`SpanMetadataCache` in culpert core: `HashMap<CulpertSpanId, SpanMeta>`
where `SpanMeta { name: String, parent: Option<CulpertSpanId>, ... }`.
Filled lazily on first sight, never evicted within a profiling window.

---

## Q5 — `TelemetryServerRoute` binary body

> *"Can `TelemetryServerRoute` serve a binary `application/octet-stream`
> body (gzipped pprof), not just text/json?"*

**Verdict: PASS, no workaround needed. The body type is binary by design.**

### Source-level

`telemetry/server/router.rs:20-21`:

```rust
pub type TelemetryRouteBody = BoxBody<Bytes, crate::Error>;
pub type TelemetryRouteHandlerFuture =
    BoxFuture<'static, Result<Response<TelemetryRouteBody>, Infallible>>;
```

`Bytes` is a binary buffer. `BoxBody<Bytes, _>` is a binary HTTP body.
There's already in-tree precedent: `/pprof/heap` serves
`application/x-gperftools-profile` (telemetry/server/router.rs:84-97)
with the same machinery.

### Empirical confirmation

`spike/src/q5.rs` registers a custom route returning all 256 byte values
(`0x00..=0xFF`) under `application/octet-stream`, brings up the telemetry
server on an ephemeral port, hits it via raw TCP, byte-compares the body:

```
server bound at Tcp(127.0.0.1:49967)
status 200 OK:           true
content-type honoured:   true
binary body round-trip:  true (256 bytes)
```

All 256 bytes round-trip cleanly.

### Plan amendment (route registration)

The plan's wiring example writes:

```rust
foundations::telemetry::add_route(TelemetryServerRoute { ... });
```

**This API does not exist in foundations 5.6.5.** Routes are registered
**at init time** via `TelemetryConfig::custom_server_routes`
(telemetry/mod.rs:271-285):

```rust
let driver = foundations::telemetry::init(TelemetryConfig {
    service_info: &service_info!(),
    settings: &settings.telemetry,
    custom_server_routes: vec![
        culpert_foundations::pprof_route("/debug/alloc/profile"),
    ],
})?;
```

This is actually nicer (no global mutable state, no init-order foot-guns).
The plan's quickstart in the README needs updating.

### Implication for Phase 1

`culpert_foundations::pprof_route(path: &str) -> TelemetryServerRoute` —
that's the public surface. Internally, the handler reads from culpert
core's profile aggregator, gzips a pprof protobuf, returns
`Response<TelemetryRouteBody>` with `application/x-gperftools-profile`
(matching the convention foundations already uses for `/pprof/heap`).

---

## Plan amendments captured

1. **`add_route` is not a thing.** Update the wiring example in `plan.md`
   "What 'adding it to a service' looks like" to use
   `TelemetryConfig::custom_server_routes` at init.

2. **Phase 1 must include a re-entrancy guard.** Already implied by the
   plan's risks list (item 1) but worth promoting to the Phase 1
   deliverables.

3. **Span identity strategy is "mint our own".** The plan's Phase 1 line
   item "Thread-local accumulator: `(span_id, callsite_addr) → bytes`"
   should be read as *culpert's* span_id, not foundations'. Foundations
   spans/Arcs are the *trigger* for minting an ID, not the ID itself.

---

## Summary table

| # | Question | Verdict | Mitigation in Phase 1 |
|---|----------|---------|----------------------|
| Q1 | Reentrancy from `current()` | PASS (steady-state alloc-free) | Re-entrancy guard thread-local |
| Q2 | Stable span identity | PASS (multiple paths) | Mint our own `u64` IDs |
| Q3 | Async propagation | PASS by construction | None — works automatically |
| Q4 | Lazy metadata access | PASS (snapshot-at-first-sight) | `SpanMetadataCache` in core |
| Q5 | Binary route body | PASS, no workaround | Provide `pprof_route()` builder |

Phase 0 is complete. Phase 1 is unblocked.
