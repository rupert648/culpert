//! [`MockSpanContext`] — a deterministic in-process [`SpanContext`] for unit
//! tests. Maintains a thread-local stack of span IDs that test code pushes /
//! pops via [`MockSpanContext::enter`].

use crate::span::{SpanContext, SpanId, SpanMetadata};
use parking_lot::Mutex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

thread_local! {
    /// Per-thread stack of currently-entered mock spans, top is innermost.
    /// `RefCell` so `current_span` (called from the alloc hot path) can use
    /// `try_borrow` and degrade gracefully if a push/pop is in flight.
    static MOCK_STACK: RefCell<Vec<SpanId>> = const { RefCell::new(Vec::new()) };
}

/// In-process mock for [`SpanContext`]. Cheap to clone.
#[derive(Clone)]
pub struct MockSpanContext {
    metadata: Arc<Mutex<HashMap<SpanId, SpanMetadata>>>,
}

impl Default for MockSpanContext {
    fn default() -> Self {
        Self::new()
    }
}

impl MockSpanContext {
    pub fn new() -> Self {
        Self {
            metadata: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Push `id` onto the calling thread's mock-span stack, register its
    /// metadata, and return an RAII guard that pops on drop.
    pub fn enter(&self, id: SpanId, name: &str, parent: Option<SpanId>) -> MockSpanGuard {
        self.metadata.lock().insert(
            id,
            SpanMetadata {
                name: name.to_owned(),
                parent,
            },
        );
        MOCK_STACK.with(|s| s.borrow_mut().push(id));
        MockSpanGuard { _priv: () }
    }
}

/// RAII guard returned by [`MockSpanContext::enter`]. Pops the thread-local
/// mock-span stack on drop.
#[must_use = "MockSpanGuard pops the mock span on drop"]
pub struct MockSpanGuard {
    _priv: (),
}

impl Drop for MockSpanGuard {
    fn drop(&mut self) {
        MOCK_STACK.with(|s| {
            // try_borrow_mut to avoid panicking if reentrant, though that's
            // unlikely on the drop path.
            if let Ok(mut v) = s.try_borrow_mut() {
                v.pop();
            }
        });
    }
}

impl SpanContext for MockSpanContext {
    fn current_span(&self) -> Option<SpanId> {
        // try_borrow: if a `push` or `pop` is mid-flight on this thread, just
        // return None for this sample. The window is single-instruction; at
        // worst we lose attribution for one alloc per enter/exit.
        MOCK_STACK.with(|s| s.try_borrow().ok()?.last().copied())
    }

    fn metadata(&self, span: SpanId) -> Option<SpanMetadata> {
        self.metadata.lock().get(&span).cloned()
    }
}
