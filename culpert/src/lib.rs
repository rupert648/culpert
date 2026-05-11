//! culpert — per-span heap allocation profiler core.
//!
//! Sampled, per-`(span, callsite)` allocation attribution. Span source is
//! pluggable via the [`SpanContext`] trait; the production adapter is
//! `culpert-foundations`, which reads from `foundations::telemetry`. Tests
//! use [`MockSpanContext`].
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
//!     culpert_foundations::install();   // wires culpert::install internally
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
pub mod span;

mod aggregator;
mod allocator;
mod global;
mod sample;
mod sampler;
mod thread_state;

// Debug logging gated by CULPERT_DEBUG=1 in env. Read once at startup; the
// hot-path check is one Relaxed atomic load.
mod debug {
    use std::sync::atomic::{AtomicBool, Ordering};
    static ENABLED: AtomicBool = AtomicBool::new(false);
    static INIT: std::sync::Once = std::sync::Once::new();

    pub(crate) fn enabled() -> bool {
        INIT.call_once(|| {
            if std::env::var_os("CULPERT_DEBUG").is_some() {
                ENABLED.store(true, Ordering::Relaxed);
            }
        });
        ENABLED.load(Ordering::Relaxed)
    }

    macro_rules! dbglog {
        ($($arg:tt)*) => {
            if $crate::debug::enabled() {
                use std::io::Write as _;
                let _ = writeln!(std::io::stderr(), "[culpert tid={:?}] {}",
                    std::thread::current().id(), format_args!($($arg)*));
            }
        };
    }

    pub(crate) use dbglog;
}

pub use aggregator::{Frame, Profile, ProfileEntry};
pub use allocator::TrackingAllocator;
pub use config::Config;
pub use culpert_macros::span_fn;
pub use global::{install, snapshot};
pub use mock::{MockSpanContext, MockSpanGuard};
pub use scope::{LocalSpanContext, Scope};
pub use span::{SpanContext, SpanId, SpanMetadata};
