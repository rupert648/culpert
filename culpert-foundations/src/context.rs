//! [`FoundationsSpanContext`] — `culpert::SpanContext` impl that reads from
//! `foundations::telemetry::tracing`.
//!
//! Identity strategy (per `notes.md` Q2): culpert mints its own monotonic
//! `u64` SpanIds, keyed by the `Arc<RwLock<Span>>` pointer of foundations'
//! current span. The cache is filled lazily on first sight.
//!
//! Sampling regime: only attributes when `span_is_sampled()` is true.
//! Inactive / unsampled spans return `None`. This is because
//! `rustracing_span()` allocates a fresh `Arc` on every call for inactive
//! spans, so the `Arc::as_ptr` cache key would be useless. The gate is
//! cheap (single thread-local read).

use culpert::{SpanContext, SpanId, SpanMetadata};
use foundations::reexports_for_macros::cf_rustracing::span::InspectableSpan;
use foundations::telemetry::tracing::{rustracing_span, span_is_sampled};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Cache keyed by `Arc<RwLock<Span>>` raw pointer (stable for the Arc's
/// lifetime in the Tracked / Untracked variants — see notes.md Q2).
struct State {
    by_arc: HashMap<usize, SpanId>,
    by_id: HashMap<SpanId, SpanMetadata>,
}

/// `culpert::SpanContext` implementation backed by foundations' tracing.
///
/// Construct with [`FoundationsSpanContext::new`] and pass to
/// [`culpert::install`], or use the [`crate::install`] convenience.
pub struct FoundationsSpanContext {
    state: RwLock<State>,
    next_id: AtomicU64,
}

impl Default for FoundationsSpanContext {
    fn default() -> Self {
        Self::new()
    }
}

impl FoundationsSpanContext {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(State {
                by_arc: HashMap::new(),
                by_id: HashMap::new(),
            }),
            // SpanIds start at 1 (NonZeroU64).
            next_id: AtomicU64::new(1),
        }
    }

    fn mint_id(&self) -> SpanId {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        // Safe: we initialise next_id at 1 and only increment.
        NonZeroU64::new(n).expect("next_id starts at 1, never zero")
    }
}

impl SpanContext for FoundationsSpanContext {
    fn current_span(&self) -> Option<SpanId> {
        // Cheap thread-local read; bails on the no-span and inactive cases
        // before we touch the per-Span lock or the cache write path.
        if !span_is_sampled() {
            return None;
        }

        // For sampled spans this returns the canonical Arc<RwLock<Span>>
        // (Tracked/Untracked variant) — Arc::as_ptr is stable for lifetime.
        let arc = rustracing_span()?;
        let key = Arc::as_ptr(&arc) as usize;

        // Hot path: read-locked lookup.
        if let Some(&id) = self.state.read().by_arc.get(&key) {
            return Some(id);
        }

        // Cache miss: snapshot metadata and insert under a write lock. We
        // re-check under the write lock in case another thread inserted.
        let id = self.mint_id();
        let name = {
            let span = arc.read();
            span.operation_name().to_string()
        };

        let mut state = self.state.write();
        if let Some(&existing) = state.by_arc.get(&key) {
            return Some(existing);
        }
        state.by_arc.insert(key, id);
        // Parent: cf-rustracing exposes references but mapping them to a
        // culpert SpanId requires walking an upstream Arc chain that isn't
        // surfaced through the public API. v0.1 leaves parent: None;
        // hierarchy is recoverable from the call stack frames in the
        // pprof output, which is the richer view anyway.
        state.by_id.insert(
            id,
            SpanMetadata {
                name,
                parent: None,
            },
        );
        Some(id)
    }

    fn metadata(&self, span: SpanId) -> Option<SpanMetadata> {
        self.state.read().by_id.get(&span).cloned()
    }
}
