//! Span identity and the pluggable [`SpanContext`] trait.
//!
//! culpert is span-source-agnostic. A `SpanContext` impl supplies "what's the
//! current span on this thread, right now?" via [`SpanContext::current_span`]
//! and resolves metadata at first sight via [`SpanContext::metadata`].

use std::num::NonZeroU64;

/// Stable identifier for a span. Minted by the [`SpanContext`] impl.
///
/// `NonZeroU64` so that `Option<SpanId>` is one machine word — important
/// because every recorded sample carries one.
pub type SpanId = NonZeroU64;

/// Span metadata snapshot. Cheap to clone is *not* a goal — these are read
/// once at first sight or at export time, never on the alloc hot path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpanMetadata {
    /// Human-readable span name (e.g. `handle_request`, `render_template`).
    pub name: String,
    /// Parent span ID, if any. Used by exporters to reconstruct the span tree.
    pub parent: Option<SpanId>,
}

/// Plug in any source of "what's the current span on this thread".
///
/// Implementors:
///
/// - [`crate::MockSpanContext`] — deterministic, in-process; for tests.
/// - `culpert_foundations::FoundationsSpanContext` (separate crate) — reads
///   from `foundations::telemetry::TelemetryContext`.
///
/// # Hot-path constraints
///
/// [`current_span`] runs inside `TrackingAllocator::alloc` on every sampled
/// allocation. It **must not allocate in steady state**. The first call on a
/// fresh thread is permitted to allocate (e.g. lazy thread-local cell init);
/// `culpert`'s reentrancy guard absorbs the recursion.
///
/// [`current_span`]: SpanContext::current_span
pub trait SpanContext: Send + Sync + 'static {
    /// The current span for the calling thread. `None` outside any span scope.
    ///
    /// Called from `TrackingAllocator::alloc` on every sampled allocation.
    fn current_span(&self) -> Option<SpanId>;

    /// Resolve metadata for a span. Called at first sight (cache miss) and at
    /// export time. Allowed to allocate.
    fn metadata(&self, span: SpanId) -> Option<SpanMetadata>;

    /// Called by the aggregator at the end of every `snapshot()`, after it
    /// has finished copying metadata for the current sample set into the
    /// emitted [`crate::Profile`].
    ///
    /// Default impl is a no-op. Adapters that cache span metadata
    /// monotonically (the foundations / tracing / local-scope adapters all
    /// do, keyed by a per-span-instance id minted by the underlying tracer)
    /// should override this to drop entries that are no longer reachable —
    /// otherwise the cache grows for the entire process lifetime, leaking
    /// roughly 70–100 bytes per unique span observed.
    ///
    /// Correctness: by the time this is called the aggregator has already
    /// `.clone()`d every metadata entry it needed into the returned
    /// `Profile`, so clearing the source cache is safe. Subsequent samples
    /// on an active span re-take the first-sight slow path and re-cache;
    /// the cost is one HashMap write per still-running span per snapshot.
    fn on_snapshot(&self) {}
}
