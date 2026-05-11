//! Shared metadata state for the layer + the SpanContext.
//!
//! Singleton via `OnceLock` so [`crate::layer`] and [`crate::install`] are
//! order-independent (and idempotent — calling either twice returns the
//! same backing store).

use culpert::{SpanId, SpanMetadata};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[derive(Default)]
pub(crate) struct Shared {
    /// Filled by `CulpertLayer::on_new_span`; read by
    /// `TracingSpanContext::metadata`. Grows monotonically for the lifetime
    /// of the process; long-running services may want a periodic eviction
    /// pass (a follow-up item — same as in `culpert-foundations`).
    pub(crate) metadata: RwLock<HashMap<SpanId, SpanMetadata>>,
}

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

/// Borrow the process-global shared state, lazily initialising on first use.
pub(crate) fn shared() -> &'static Arc<Shared> {
    SHARED.get_or_init(|| Arc::new(Shared::default()))
}
