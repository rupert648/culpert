//! [`CulpertLayer`] — `tracing_subscriber::Layer` that snapshots span
//! metadata at creation time.

use crate::shared::Shared;
use culpert::{SpanId, SpanMetadata};
use std::num::NonZeroU64;
use std::sync::Arc;
use tracing::span::{Attributes, Id};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Subscriber layer that records each span's `name` and parent `SpanId` in
/// culpert's shared metadata map.
///
/// Construct via [`crate::layer()`]; compose with whatever other
/// `tracing_subscriber::Layer`s you use.
pub struct CulpertLayer {
    shared: Arc<Shared>,
}

impl CulpertLayer {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

impl<S> Layer<S> for CulpertLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        // tracing::span::Id is documented to be non-zero, but defend.
        let Some(span_id): Option<SpanId> = NonZeroU64::new(id.into_u64()) else {
            return;
        };

        let name = attrs.metadata().name().to_string();

        // Parent: tracing's explicit parent (if `span!(parent: ..., ...)`) or
        // the implicit current span. tracing_subscriber's Context exposes
        // `lookup_current()` for the latter; we fall back to it when no
        // explicit parent is given on the Attributes.
        let parent: Option<SpanId> = attrs
            .parent()
            .and_then(|p| NonZeroU64::new(p.into_u64()))
            .or_else(|| {
                ctx.lookup_current()
                    .and_then(|cur| NonZeroU64::new(cur.id().into_u64()))
            });

        let mut by_id = self.shared.metadata.write();
        by_id
            .entry(span_id)
            .or_insert(SpanMetadata { name, parent });
    }
}
