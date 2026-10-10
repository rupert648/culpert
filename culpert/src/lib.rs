//! culpert — per-span heap allocation profiler core.
//!
//! Sampled, per-`(span, callsite)` allocation attribution. Span source is
//! pluggable via the [`SpanContext`] trait. Enable the `foundations` or
//! `tracing` feature to use an existing telemetry stack, or use
//! [`LocalSpanContext`] for standalone instrumentation.
//!
//! # End-user shape
//!
//! ```ignore
//! use std::alloc::System;
//! use culpert::TrackingAllocator;
//!
//! #[global_allocator]
//! static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);
//!
//! fn main() {
//!     foundations::telemetry::init(...).unwrap();
//!     let _profiler = culpert::foundations::install();
//!     // ...
//! }
//! ```
//!
//! The allocator is a pure forwarder until [`install`] is called. After
//! install it samples ~1-in-512KiB by default and feeds samples into the
//! per-thread buffer. [`snapshot`] drains every thread's buffer and returns a
//! [`Profile`].
//!
//! # Hot path overview
//!
//! On every `alloc()`:
//! 1. Forward to the inner allocator.
//! 2. If non-null, call `sampler::observe(size)`.
//! 3. `observe` checks a per-thread reentrancy guard, the per-thread sample
//!    countdown, and pushes a sample with current span + stack if it crosses.
//!
//! No global state is read or written on the *non-sampling* path beyond the
//! `OnceLock` profiler check and a thread-local cell for the reentrancy
//! guard.
//!
//! # Module map
//!
//! - [`span`] — `SpanId`, `SpanMetadata`, [`SpanContext`] trait
//! - [`config`] — [`Config`] knobs
//! - [`mock`] — [`MockSpanContext`] for tests
//! - [`pprof`] — pprof protobuf encoder + decoder. Exposed publicly so
//!   downstream tooling (`culpert-cli`, third-party analysis code) can read
//!   profiles without vendoring its own copy of the schema.
//! - The other modules are crate-internal:
//!   - `allocator` — [`TrackingAllocator`]
//!   - `sampler` — hot-path observer + reentrancy guard
//!   - `thread_state` — per-thread buffer + global registry
//!   - `aggregator` — snapshot + symbol resolution
//!   - `global` — [`install`] / [`snapshot`] entry points
//!
//! # Debugging
//!
//! Set `CULPERT_DEBUG=1` in the environment to enable internal trace logs
//! (sampler/snapshot lifecycle to stderr). Off by default; the hot-path
//! check is one relaxed atomic load.

pub mod config;
pub mod mock;
pub mod pprof;
pub mod scope;
pub mod scoped_future;
pub mod span;
pub mod stack_capture;

mod aggregator;
mod allocator;
mod debug;
mod global;
mod rng;
mod sample;
mod sampler;
mod thread_state;

#[cfg(feature = "foundations")]
pub mod foundations;
#[cfg(feature = "tracing")]
pub mod tracing;

pub use aggregator::{Frame, Profile, ProfileEntry};
pub use allocator::TrackingAllocator;
pub use config::{Config, StackCaptureStrategy};
pub use culpert_macros::span_fn;
pub use global::{install, snapshot, ProfilerGuard};
pub use mock::{MockSpanContext, MockSpanGuard};
pub use scope::{LocalSpanContext, Scope};
pub use scoped_future::ScopedFuture;
pub use span::{SpanContext, SpanId, SpanMetadata};
