//! [`FoundationsSpanContext`] — `culpert::SpanContext` impl that reads from
//! `foundations::telemetry::tracing`.
//!
//! Identity strategy: use cf-rustracing's own `span_id` (a `u64` minted by
//! the tracer at span creation time). It is stable for the span's lifetime
//! and unique within a trace, which is what culpert needs.
//!
//! An earlier draft used `Arc<RwLock<Span>>::as_ptr` as the identity. That
//! is **wrong** for long-running services: when a span ends its Arc drops,
//! the heap slot is reused, and a later unrelated span's Arc may land at
//! the same address. Mock-axum surfaced this immediately — every request
//! after the first returned the same SpanId from cache. cf-rustracing's
//! span_id has none of that risk.
//!
//! Sampling regime: only attributes when `span_is_sampled()` is true.
//! Inactive / unsampled spans return `None`. This is because cf-rustracing
//! only attaches a span context (and therefore a span_id) to sampled
//! spans. The gate is cheap (single thread-local read).

use culpert::{SpanContext, SpanId, SpanMetadata};
use foundations::reexports_for_macros::cf_rustracing::span::InspectableSpan;
use foundations::telemetry::tracing::{rustracing_span, span_is_sampled};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::num::NonZeroU64;

/// `culpert::SpanContext` implementation backed by foundations' tracing.
///
/// Construct with [`FoundationsSpanContext::new`] and pass to
/// [`culpert::install`], or use the [`crate::install`] convenience.
pub struct FoundationsSpanContext {
    /// Span metadata keyed by cf-rustracing's `span_id`. Filled lazily on
    /// first sight of a span; cleared when the user calls [`Self::clear_metadata`].
    by_id: RwLock<HashMap<SpanId, SpanMetadata>>,
}

impl Default for FoundationsSpanContext {
    fn default() -> Self {
        Self::new()
    }
}

impl FoundationsSpanContext {
    pub fn new() -> Self {
        Self {
            by_id: RwLock::new(HashMap::new()),
        }
    }

    /// Drop the span-metadata cache. The cache grows monotonically over the
    /// process lifetime as new spans are observed; long-running services
    /// can call this periodically (e.g. after a snapshot) to bound memory.
    /// v0.1 does not do this automatically.
    pub fn clear_metadata(&self) {
        self.by_id.write().clear();
    }
}

impl SpanContext for FoundationsSpanContext {
    fn current_span(&self) -> Option<SpanId> {
        // Cheap thread-local read; bails on the no-span and inactive cases.
        if !span_is_sampled() {
            return None;
        }

        // Sampled spans have a non-empty SpanContext from cf-rustracing.
        let arc = rustracing_span()?;
        let span = arc.read();
        let span_id_u64 = span.context()?.state().span_id();
        let span_id = NonZeroU64::new(span_id_u64)?;

        // Fast path: metadata already snapshot.
        if self.by_id.read().contains_key(&span_id) {
            return Some(span_id);
        }

        // Slow path (first sight on this span): snapshot the name.
        let name = span.operation_name().to_string();
        drop(span);

        let mut by_id = self.by_id.write();
        by_id.entry(span_id).or_insert_with(|| SpanMetadata {
            name,
            // Parent: cf-rustracing's references list is not surfaced
            // through foundations' public API in a way that maps cleanly
            // to a culpert SpanId. v0.1 leaves parent: None; hierarchy is
            // recoverable from the call stack frames in the pprof output.
            parent: None,
        });
        Some(span_id)
    }

    fn metadata(&self, span: SpanId) -> Option<SpanMetadata> {
        self.by_id.read().get(&span).cloned()
    }
}
