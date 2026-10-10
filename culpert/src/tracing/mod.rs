//! `tracing` integration for culpert.
//!
//! Maps `tracing`-crate spans into [`crate::SpanContext`].
//!
//! # Wiring
//!
//! ```ignore
//! use culpert::TrackingAllocator;
//! use std::alloc::System;
//! use tracing_subscriber::prelude::*;
//!
//! #[global_allocator]
//! static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);
//!
//! fn main() {
//!     // Register culpert's Layer alongside whichever subscriber
//!     // layers you already use. The Registry must be at the bottom of
//!     // the stack — culpert-tracing needs `LookupSpan` for parent lookup.
//!     tracing_subscriber::registry()
//!         .with(culpert::tracing::layer())
//!         .with(/* your fmt / OTLP / etc. layers */)
//!         .init();
//!
//!     culpert::tracing::install();
//!
//!     // ... your app, with #[tracing::instrument] / info_span!/... as usual.
//! }
//! ```
//!
//! # How it works
//!
//! `tracing` itself doesn't store span metadata — it just hands span IDs to
//! whichever subscribers are registered. To resolve a `SpanId` back to a
//! `SpanMetadata { name, parent }`, culpert-tracing registers a
//! [`tracing_subscriber::Layer`] that captures every span's name and parent
//! at creation time and stashes them in a shared `HashMap`. The
//! [`crate::SpanContext`] impl then just reads
//! `tracing::Span::current().id()` for the hot path and looks up metadata
//! when culpert snapshots.
//!
//! State is shared between the [`layer`] and the installed
//! [`crate::SpanContext`] via a process-global `Arc`, so
//! it's fine to call [`layer`] and [`install`] in any order.

mod context;
mod layer;
mod shared;

pub use context::TracingSpanContext;
pub use layer::CulpertLayer;

use crate::Config;
use shared::shared;

/// Build a [`tracing_subscriber::Layer`] that captures span metadata for
/// culpert. Compose it with the rest of your subscriber stack.
pub fn layer() -> CulpertLayer {
    CulpertLayer::new(shared().clone())
}

/// Install a [`TracingSpanContext`] into culpert with the default
/// [`Config`]. Should be called once at service startup, after
/// `tracing_subscriber::registry().with(layer()).init()`.
///
/// Panics if culpert has already been installed (delegates to
/// [`crate::install`]).
#[track_caller]
pub fn install() {
    install_with_config(Config::default());
}

/// As [`install`], but with a caller-supplied [`Config`].
#[track_caller]
pub fn install_with_config(config: Config) {
    crate::install(TracingSpanContext::from_shared(shared().clone()), config);
}
