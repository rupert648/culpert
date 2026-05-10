//! culpert-foundations — adapter that maps `foundations::telemetry`
//! spans into `culpert`'s [`SpanContext`](culpert::SpanContext), and
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
//!             culpert_foundations::pprof_route("/debug/alloc/profile"),
//!         ],
//!     }).unwrap();
//!
//!     culpert_foundations::install();
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
//! sampling, build a custom [`SpanContext`](culpert::SpanContext) and pass
//! it to [`culpert::install`] directly.

mod context;
mod route;

pub use context::FoundationsSpanContext;
pub use route::pprof_route;

use culpert::Config;

/// Install culpert with a [`FoundationsSpanContext`] and the default
/// [`Config`]. Convenience wrapper for the common case.
///
/// Should be called once after `foundations::telemetry::init`. Panics if
/// called more than once (delegates to [`culpert::install`]).
#[track_caller]
pub fn install() {
    install_with_config(Config::default());
}

/// As [`install`], but with a caller-supplied [`Config`] (e.g. a tighter
/// sample rate or a smaller stack-capture depth).
#[track_caller]
pub fn install_with_config(config: Config) {
    culpert::install(FoundationsSpanContext::new(), config);
}
