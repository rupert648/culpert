//! Process-global profiler handle. The [`TrackingAllocator`](crate::TrackingAllocator)
//! looks this up on every alloc; if it's empty, the allocator is a pure
//! forwarder.

use crate::aggregator::{self, Profile};
use crate::config::Config;
use crate::span::SpanContext;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

pub(crate) struct Profiler {
    pub(crate) config: Config,
    pub(crate) ctx: Box<dyn SpanContext>,
}

static PROFILER: OnceLock<Profiler> = OnceLock::new();

/// Borrow the installed profiler, if any.
#[inline]
pub(crate) fn profiler() -> Option<&'static Profiler> {
    PROFILER.get()
}

/// Borrowed access to the installed config, if any.
#[allow(dead_code)] // reserved for future internal use
pub(crate) fn config() -> Option<&'static Config> {
    PROFILER.get().map(|p| &p.config)
}

static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

#[inline]
pub(crate) fn is_shutting_down() -> bool {
    SHUTTING_DOWN.load(Ordering::Relaxed)
}

pub(crate) fn shutdown() {
    SHUTTING_DOWN.store(true, Ordering::Relaxed);
}

extern "C" fn culpert_atexit() {
    SHUTTING_DOWN.store(true, Ordering::Relaxed);
}

// Bind directly to the C runtime's `atexit` so we don't pull in the
// `libc` crate for one declaration. Available on every host the Rust
// compiler targets.
unsafe extern "C" {
    fn atexit(cb: extern "C" fn()) -> i32;
}

/// Keeps allocation profiling active.
///
/// Dropping this guard stops the allocator observer before application and
/// thread-local teardown can begin. Keep it alive for as long as allocations
/// should be recorded.
#[must_use = "keep this guard alive while profiling"]
pub struct ProfilerGuard {
    _private: (),
}

impl Drop for ProfilerGuard {
    fn drop(&mut self) {
        shutdown();
    }
}

/// Install the profiler. Should be called once at service startup, after
/// any global lifecycle (e.g. `foundations::telemetry::init`) the
/// [`SpanContext`] depends on.
///
/// Returns a [`ProfilerGuard`] that must remain alive while profiling. Dropping
/// it permanently stops allocation observation for this process.
///
/// Registers a `libc::atexit` handler that flips an internal
/// "shutting down" flag so the observer can bail during process
/// teardown, avoiding recursive-mutex deadlocks in thread-local destructors.
///
/// Panics if called more than once. This mirrors `foundations::telemetry::init`.
#[track_caller]
pub fn install<C: SpanContext>(ctx: C, config: Config) -> ProfilerGuard {
    crate::debug::dbglog!(
        "install: rate_bytes={}, stack_depth={}, buffer_capacity={}",
        config.rate_bytes,
        config.stack_depth,
        config.buffer_capacity
    );
    let profiler = Profiler {
        config,
        ctx: Box::new(ctx),
    };
    if PROFILER.set(profiler).is_err() {
        panic!("culpert::install called more than once");
    }
    // SAFETY: `atexit` is part of the C runtime, always linked. Calling
    // it with an `extern "C"` fn-pointer that has the documented `() -> ()`
    // signature is sound. Doc-promise: the callback will not be invoked
    // twice; we'd want it once even if install ever happened twice
    // (which we panic on), so we don't bother de-duping registrations.
    unsafe {
        atexit(culpert_atexit);
    }

    ProfilerGuard { _private: () }
}

/// Take a destructive snapshot of accumulated samples. Returns an empty
/// profile if no profiler is installed.
pub fn snapshot() -> Profile {
    crate::debug::dbglog!("snapshot: entry");
    // Hold the per-thread reentrancy guard for the entire snapshot. Any
    // allocation performed during the snapshot — Vec growth, backtrace
    // symbol resolution, HashMap rehashes — will recurse into `observe()`
    // and short-circuit immediately, instead of trying to acquire a
    // per-thread Mutex this snapshot may already be holding.
    let _reentry = crate::sampler::enter_reentry_zone();

    let Some(p) = profiler() else {
        crate::debug::dbglog!("snapshot: no profiler installed, returning empty");
        return Profile {
            entries: Vec::new(),
            spans: std::collections::HashMap::new(),
            dropped_samples: 0,
            config: Config::default(),
        };
    };
    let profile = aggregator::snapshot(&p.config, &*p.ctx);
    crate::debug::dbglog!(
        "snapshot: exit, {} entries, {} spans, {} dropped",
        profile.entries.len(),
        profile.spans.len(),
        profile.dropped_samples
    );
    profile
}
