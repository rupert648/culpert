//! `culpert::scope` — sampling-independent span attribution via a
//! culpert-owned thread-local stack.
//!
//! The foundations/tracing adapters read their span identity from the
//! external tracer's thread-local. That means allocation attribution is
//! only as good as the trace-sampling rate of the underlying system. For
//! services that want allocation profiling regardless of trace sampling,
//! this module provides:
//!
//! - [`enter`] — push a scope onto culpert's own thread-local stack,
//!   minting a fresh `SpanId`. Returns an RAII guard whose `Drop` pops.
//! - [`LocalSpanContext`] — a [`SpanContext`](crate::SpanContext) impl
//!   that reads from that thread-local stack. Install this with
//!   [`crate::install`] to use scope-driven attribution.
//!
//! The `#[culpert::span_fn]` proc-macro (from `culpert-macros`, re-exported
//! at `culpert::span_fn`) is the ergonomic front-end — it wraps a function
//! body with `let _guard = culpert::scope::enter(name);`.
//!
//! Hierarchy is built directly: each `enter()` reads the top of the stack
//! as the new scope's parent. Snapshots get a fully-populated
//! `SpanMetadata { name, parent }` for every observed span.

use crate::span::{SpanContext, SpanId, SpanMetadata};
use parking_lot::RwLock;
use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Global metadata for every span ever opened by [`enter`]. Grows
/// monotonically over the process lifetime; long-running services that
/// want bounded memory should call [`LocalSpanContext::clear_metadata`]
/// periodically. `LazyLock` because `HashMap::new` is not yet const.
static METADATA: LazyLock<RwLock<HashMap<SpanId, SpanMetadata>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Monotonic SpanId source. Starts at 1 so `NonZeroU64::new` always succeeds.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// Per-thread stack of currently-entered scope ids. Top of stack is
    /// the "current span" for the calling thread.
    static STACK: RefCell<Vec<SpanId>> = const { RefCell::new(Vec::new()) };
}

/// RAII guard returned by [`enter`]. Pops the thread-local stack on drop.
#[must_use = "scope is popped on drop; bind to a let _guard = ... variable"]
pub struct Scope {
    _priv: (),
}

impl Drop for Scope {
    fn drop(&mut self) {
        STACK.with(|s| {
            // try_borrow_mut: tolerate the unlikely case of dropping during
            // a (mid-)borrow on the same thread — safer than panicking.
            if let Ok(mut v) = s.try_borrow_mut() {
                v.pop();
            }
        });
    }
}

/// Open a new culpert scope on the calling thread. The returned [`Scope`]
/// guard must be held for the duration of the work to attribute to this
/// span; allocations on this thread between `enter` and the guard's drop
/// are tagged with the minted [`SpanId`].
///
/// `name` is `&'static str` so we can pin it into the metadata cache
/// without an extra allocation per call.
pub fn enter(name: &'static str) -> Scope {
    // Suppress sampling for the duration of our own bookkeeping —
    // `name.to_string()`, the METADATA HashMap insert (which may
    // `reserve_rehash`), and the STACK Vec push (which may grow). Without
    // this guard those allocations get attributed to whatever scope is
    // currently on top of the stack (i.e. the PARENT of the scope we're
    // about to enter), polluting its self-time with culpert's own setup
    // cost — typically showing as `hashbrown::raw::RawTable::reserve_rehash`
    // hanging off a user-visible span name. See `sampler::enter_reentry_zone`.
    let _reentry = crate::sampler::enter_reentry_zone();

    // Mint a new id. Atomic-only, no allocation.
    let raw = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let id = NonZeroU64::new(raw).expect("NEXT_ID starts at 1, only increments");

    // Capture parent BEFORE we push, so we don't borrow_mut STACK twice in
    // one call.
    let parent = STACK.with(|s| s.borrow().last().copied());

    // Store the metadata once. (We could defer this to the snapshot path,
    // but eager insertion makes the hot-path `current_span` a pure read.)
    {
        let mut meta = METADATA.write();
        meta.entry(id).or_insert(SpanMetadata {
            name: name.to_string(),
            parent,
        });
    }

    STACK.with(|s| s.borrow_mut().push(id));
    Scope { _priv: () }
}

/// [`SpanContext`] implementation that reads from culpert's own
/// thread-local scope stack. Pair with [`enter`] (or the
/// `#[culpert::span_fn]` macro) for sampling-independent attribution.
///
/// Cheap to construct (unit-shaped). All state lives in the
/// module-level statics; multiple instances would share the same store.
#[derive(Default)]
pub struct LocalSpanContext {
    _priv: (),
}

impl LocalSpanContext {
    pub fn new() -> Self {
        Self { _priv: () }
    }

    /// Drop the in-process metadata cache. The cache otherwise grows
    /// monotonically with the number of unique spans observed.
    pub fn clear_metadata(&self) {
        METADATA.write().clear();
    }
}

impl SpanContext for LocalSpanContext {
    fn current_span(&self) -> Option<SpanId> {
        // try_borrow: see MockSpanContext for the same rationale — a
        // concurrent push/pop on the same thread (during the brief Vec
        // resize window in `enter`) momentarily makes the cell
        // borrow-incompatible; we return None for that one sample instead
        // of panicking.
        STACK
            .with(|s| s.try_borrow().ok().and_then(|v| v.last().copied()))
    }

    fn metadata(&self, span: SpanId) -> Option<SpanMetadata> {
        METADATA.read().get(&span).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_span_is_top_of_stack() {
        let ctx = LocalSpanContext::new();
        assert_eq!(ctx.current_span(), None);

        let g1 = enter("first");
        let id1 = ctx.current_span().expect("first scope visible");
        assert_eq!(ctx.metadata(id1).unwrap().name, "first");
        assert_eq!(ctx.metadata(id1).unwrap().parent, None);

        let g2 = enter("second");
        let id2 = ctx.current_span().expect("second scope visible");
        assert_ne!(id1, id2);
        assert_eq!(ctx.metadata(id2).unwrap().name, "second");
        assert_eq!(ctx.metadata(id2).unwrap().parent, Some(id1));

        drop(g2);
        assert_eq!(ctx.current_span(), Some(id1));

        drop(g1);
        assert_eq!(ctx.current_span(), None);
    }
}
