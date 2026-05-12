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
use std::cell::Cell;
use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::Once;

thread_local! {
    /// Set around our `catch_unwind` so the global panic hook can skip
    /// the diagnostic print for the foundations-borrow_mut conflict
    /// described in [`FoundationsSpanContext::current_span`]. Other
    /// panics on other threads (or on this thread outside the catch
    /// window) print normally.
    static SUPPRESS_PANIC_HOOK: Cell<bool> = const { Cell::new(false) };
}

static HOOK_INSTALLED: Once = Once::new();

/// Wrap the existing panic hook so it stays silent for panics that
/// happen inside our `catch_unwind` block on this thread. Idempotent;
/// the first call wins.
pub(crate) fn install_panic_hook_filter() {
    HOOK_INSTALLED.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if SUPPRESS_PANIC_HOOK.with(|c| c.get()) {
                return;
            }
            prev(info);
        }));
    });
}

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
        // Defend against the foundations re-entrancy bug:
        //
        //   `Scope::Drop` holds `borrow_mut` on its per-thread scope-stack
        //   RefCell while popping the just-finished span. Dropping that
        //   span sends a finish-event over a `tokio::mpsc` channel, which
        //   can allocate a new mpsc block. The allocation calls our
        //   observer, which calls back into `tracing::span_is_sampled` /
        //   `rustracing_span` — both of which `borrow()` the same RefCell
        //   foundations is still holding `borrow_mut` on, and panic.
        //
        // The panic frame is entirely inside our `current_span` call; we
        // can catch it, return `None` (lose attribution for this one
        // sample), and let foundations' outer `Scope::Drop` continue
        // normally. The borrow_mut belongs to a higher stack frame so our
        // unwind doesn't disturb it. The thread-local
        // `SUPPRESS_PANIC_HOOK` flag tells our installed hook to skip the
        // diagnostic print on stderr for this expected panic.
        SUPPRESS_PANIC_HOOK.with(|c| c.set(true));
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.current_span_inner()));
        SUPPRESS_PANIC_HOOK.with(|c| c.set(false));
        result.ok().flatten()
    }

    fn metadata(&self, span: SpanId) -> Option<SpanMetadata> {
        self.by_id.read().get(&span).cloned()
    }

    fn on_snapshot(&self) {
        // The aggregator has already copied every metadata entry it needs
        // into the emitted `Profile`. Drop the by-id cache so it doesn't
        // accumulate one entry per cf-rustracing span_id for the entire
        // process lifetime (a real leak in long-running services — see the
        // trait's docs).
        self.by_id.write().clear();
    }
}

impl FoundationsSpanContext {
    fn current_span_inner(&self) -> Option<SpanId> {
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

        // Slow path (first sight on this span): snapshot the name + parent.
        let name = span.operation_name().to_string();
        // cf-rustracing exposes parent relationships as SpanReferences. We
        // take the first ChildOf reference's span_id as the parent; in
        // typical foundations usage there's exactly one (created when
        // tracing::span(...) calls span.child(name)). FollowsFrom-style
        // references are ignored as not-quite-parents.
        let parent = span
            .references()
            .iter()
            .find(|r| r.is_child_of())
            .and_then(|r| NonZeroU64::new(r.span().span_id()));
        drop(span);

        let mut by_id = self.by_id.write();
        by_id
            .entry(span_id)
            .or_insert(SpanMetadata { name, parent });
        Some(span_id)
    }
}
