//! `foundations` integration for culpert.
//!
//! Maps `foundations::telemetry` spans into [`crate::SpanContext`] and
//! provides a [`TelemetryServerRoute`](foundations::telemetry::TelemetryServerRoute)
//! that serves culpert's gzipped pprof profile.
//!
//! # Wiring
//!
//! ```ignore
//! use culpert::TrackingAllocator;
//! use std::alloc::System;
//!
//! #[global_allocator]
//! static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);
//!
//! fn main() {
//!     let driver = foundations::telemetry::init(TelemetryConfig {
//!         service_info: &service_info!(),
//!         settings: &settings.telemetry,
//!         custom_server_routes: vec![
//!             culpert::foundations::pprof_route("/debug/alloc/profile"),
//!         ],
//!     }).unwrap();
//!
//!     culpert::foundations::install();
//!     // ... rest of app, with #[span_fn] instrumentation as usual.
//! }
//! ```
//!
//! # Sampling regime
//!
//! Allocation attribution is gated on foundations' trace sampling: only
//! sampled spans get attributed allocations. See `notes.md` Q2 for why
//! (the `Arc<RwLock<Span>>` pointer used as the cache key is unstable for
//! the inactive / unsampled variant).
//!
//! For services that want allocation profiling independent of trace
//! sampling, build a custom [`crate::SpanContext`] and pass it to
//! [`crate::install`] directly.

mod context;
mod route;

pub use context::FoundationsSpanContext;
pub use route::pprof_route;

use crate::Config;

/// Install culpert with a [`FoundationsSpanContext`] and the default
/// [`Config`]. Convenience wrapper for the common case.
///
/// Should be called once after `foundations::telemetry::init`. Panics if
/// called more than once (delegates to [`crate::install`]).
#[track_caller]
pub fn install() {
    install_with_config(Config::default());
}

/// As [`install`], but with a caller-supplied [`Config`] (e.g. a tighter
/// sample rate or a smaller stack-capture depth).
///
/// Installs a panic-hook filter so the foundations-internal re-entrancy
/// panics our `current_span` defends against don't print noisy
/// backtraces on stderr. The filter delegates to the previous hook for
/// any other panic. See `context::install_panic_hook_filter` for the
/// details.
#[track_caller]
pub fn install_with_config(config: Config) {
    context::install_panic_hook_filter();
    crate::install(FoundationsSpanContext::new(), config);
}
