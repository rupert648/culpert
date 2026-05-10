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
    /// Human-readable span name (e.g. `flow_handler`, `templates::render`).
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
}
