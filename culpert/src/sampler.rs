//! Hot-path observer: invoked by [`TrackingAllocator::alloc`](crate::TrackingAllocator)
//! on every successful allocation. Decides sampling, captures stack, pushes
//! a [`RawSample`] into the per-thread buffer.
//!
//! Re-entrancy: the per-thread `IN_TRACKER` cell short-circuits any
//! allocation triggered by our own bookkeeping (e.g. foundations' lazy
//! thread-local cell init, the `Vec` growth in our sample buffer, etc.).
//! See `notes.md` Q1 for why this is necessary.

use crate::sample::RawSample;
use crate::{global, thread_state};
use smallvec::SmallVec;
use std::cell::Cell;

thread_local! {
    /// Per-thread reentrancy guard. `const`-initialised so first access on a
    /// thread does not heap-allocate.
    static IN_TRACKER: Cell<bool> = const { Cell::new(false) };
}

/// Entry point from [`TrackingAllocator::alloc`](crate::TrackingAllocator).
///
/// Returns immediately without doing anything if:
/// - no profiler is installed, or
/// - the process has begun its `exit()` teardown
///   (avoids a recursive-mutex deadlock against TLS destructors —
///   see [`crate::global::is_shutting_down`]), or
/// - we're already inside an `observe` call on this thread (recursion), or
/// - the snapshot path on this thread has explicitly raised the guard, or
/// - the calling thread's `IN_TRACKER` is mid-destruction (TLS shutdown).
#[inline]
pub(crate) fn observe(bytes: u64) {
    // Cheap shutdown gate first. An atexit handler flips this flag
    // BEFORE TLS destructors run, so an allocation triggered by a TLS
    // destructor (e.g. `thread_local`'s `ThreadGuard::drop` pushing onto
    // its global BinaryHeap, which grows and allocates) bails before
    // calling into the SpanContext — which might otherwise attempt to
    // re-lock a `std::sync::Mutex` the TLS destructor is already holding.
    if crate::global::is_shutting_down() {
        return;
    }
    // try_with: if IN_TRACKER is mid-destruction (only happens during
    // thread teardown), treat as "already in tracker" and bail.
    let already = IN_TRACKER.try_with(|c| c.replace(true)).unwrap_or(true);
    if already {
        return;
    }
    // SAFETY net: clear guard even if do_observe panics.
    let _reset = ReentryReset { prior: false };
    do_observe(bytes);
}

/// RAII guard that raises the per-thread reentrancy flag for the duration of
/// a critical section, restoring its previous value on drop.
///
/// Used by the snapshot path to prevent allocations performed during the
/// snapshot itself (Vec growth, backtrace::resolve, etc.) from re-entering
/// `observe` and deadlocking on per-thread locks the snapshot is already
/// holding.
pub(crate) fn enter_reentry_zone() -> ReentryReset {
    // try_with: same TLS-shutdown caveat as `observe`.
    let prior = IN_TRACKER.try_with(|c| c.replace(true)).unwrap_or(true);
    ReentryReset { prior }
}

pub(crate) struct ReentryReset {
    prior: bool,
}

impl Drop for ReentryReset {
    fn drop(&mut self) {
        let prior = self.prior;
        // try_with: silently ignore TLS-shutdown failure on drop.
        let _ = IN_TRACKER.try_with(|c| c.set(prior));
    }
}

fn do_observe(bytes: u64) {
    // Cheap path when no profiler installed: just return.
    let Some(profiler) = global::profiler() else {
        return;
    };

    // None means our TLS is mid-destruction; nothing to do.
    let Some(handle) = thread_state::handle(&profiler.config) else {
        return;
    };

    // Lock briefly: update countdown, decide whether to sample, drop lock.
    // We do NOT want to hold the per-thread mutex across `backtrace::trace`.
    let should_sample = {
        let mut st = handle.lock();
        st.bytes_until_next_sample = st.bytes_until_next_sample.saturating_sub(bytes as i64);
        if st.bytes_until_next_sample > 0 {
            false
        } else {
            // Rearm the countdown. We *add* rather than replace so a single huge
            // alloc that overshoots by N rate intervals still only produces one
            // sample. Acceptable bias at extreme alloc sizes for v0.1.
            st.bytes_until_next_sample = st
                .bytes_until_next_sample
                .saturating_add(profiler.config.rate_bytes as i64);
            true
        }
    };

    if !should_sample {
        return;
    }

    let span = profiler.ctx.current_span();
    let frames = capture_stack(profiler.config.stack_depth);

    handle.lock().try_push(RawSample {
        span,
        bytes,
        frames,
    });
}

fn capture_stack(depth: usize) -> SmallVec<[usize; 32]> {
    let mut out: SmallVec<[usize; 32]> = SmallVec::new();
    backtrace::trace(|frame| {
        if out.len() >= depth {
            return false;
        }
        out.push(frame.ip() as usize);
        true
    });
    out
}
