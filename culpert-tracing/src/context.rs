//! [`TracingSpanContext`] — `culpert::SpanContext` impl backed by
//! `tracing::Span::current()` for the hot-path id lookup and the shared
//! metadata map filled by [`crate::CulpertLayer`] for snapshot-time
//! metadata.

use crate::shared::Shared;
use culpert::{SpanContext, SpanId, SpanMetadata};
use std::num::NonZeroU64;
use std::sync::Arc;

/// [`culpert::SpanContext`] implementation backed by the `tracing` crate.
///
/// Use [`crate::install`] / [`crate::install_with_config`] for the typical
/// case. Construct directly only if you need to wire culpert manually
/// (e.g. a composite SpanContext mixing tracing + foundations).
pub struct TracingSpanContext {
    shared: Arc<Shared>,
}

impl TracingSpanContext {
    pub(crate) fn from_shared(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

impl SpanContext for TracingSpanContext {
    fn current_span(&self) -> Option<SpanId> {
        // `tracing::Span::current()` is a thread-local read; `Span::id()`
        // returns `Option<Id>` where `Id` is a non-zero `u64`. Both are
        // alloc-free in steady state on the tested subscribers
        // (tracing-subscriber's Registry).
        let id = tracing::Span::current().id()?;
        NonZeroU64::new(id.into_u64())
    }

    fn metadata(&self, span: SpanId) -> Option<SpanMetadata> {
        self.shared.metadata.read().get(&span).cloned()
    }
}
