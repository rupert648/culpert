//! Per-thread sample buffer + global registry of weak handles + a "deceased
//! threads" queue for samples orphaned by thread exit.
//!
//! Lifecycle:
//!
//! 1. First sample on a thread: lazily allocate an `Arc<Mutex<ThreadState>>`,
//!    wrap it in a [`ThreadHandle`] kept in a thread-local, and register a
//!    `Weak` in the global registry. Hot path locks the per-thread `Mutex`
//!    (uncontended in steady state).
//! 2. [`crate::snapshot`] walks the registry, upgrades each `Weak`, drains
//!    live thread states, and prunes dead ones.
//! 3. On thread exit, the thread-local `ThreadHandle::drop` moves any
//!    leftover samples (and dropped-sample count) into [`DECEASED_SAMPLES`]
//!    so the next snapshot still attributes them. Without this step, samples
//!    accumulated on short-lived worker threads vanish when those threads
//!    finish.

use crate::config::Config;
use crate::sample::RawSample;
use parking_lot::Mutex;
use std::cell::OnceCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

pub(crate) struct ThreadState {
    /// Counts down by `layout.size()` per alloc; sample fires when it goes <= 0.
    pub(crate) bytes_until_next_sample: i64,
    /// Sample buffer. Pre-allocated to `buffer_capacity`; once full, further
    /// samples are dropped and counted in `dropped_samples`.
    pub(crate) samples: Vec<RawSample>,
    /// Cumulative dropped-sample count for this thread (since last drain).
    pub(crate) dropped_samples: u64,
    /// Cached for the buffer overflow check.
    buffer_capacity: usize,
}

impl ThreadState {
    fn new(config: &Config) -> Self {
        Self {
            bytes_until_next_sample: config.rate_bytes as i64,
            samples: Vec::with_capacity(config.buffer_capacity),
            dropped_samples: 0,
            buffer_capacity: config.buffer_capacity,
        }
    }

    pub(crate) fn try_push(&mut self, s: RawSample) {
        if self.samples.len() >= self.buffer_capacity {
            self.dropped_samples = self.dropped_samples.saturating_add(1);
            return;
        }
        self.samples.push(s);
    }
}

/// Wrapper that owns the per-thread Arc and runs salvage logic on thread exit.
/// Held inside a thread-local; its `Drop` runs as part of thread shutdown.
struct ThreadHandle {
    inner: Arc<Mutex<ThreadState>>,
}

impl Drop for ThreadHandle {
    fn drop(&mut self) {
        // Hold the reentrancy guard for the duration of the salvage. Any
        // allocation we trigger here (locking, Vec growth in DECEASED) would
        // otherwise re-enter `observe()` and try to access THREAD_HANDLE
        // (this very thread-local), which is already in the process of being
        // dropped.
        let _reentry = crate::sampler::enter_reentry_zone();

        // Take samples + dropped count out of the per-thread state.
        let (samples, dropped) = {
            let mut st = self.inner.lock();
            (
                std::mem::take(&mut st.samples),
                std::mem::replace(&mut st.dropped_samples, 0),
            )
        };

        crate::debug::dbglog!(
            "thread exit: salvaging {} samples ({} dropped) into deceased queue",
            samples.len(),
            dropped
        );

        if !samples.is_empty() {
            let mut dec = DECEASED_SAMPLES.lock();
            dec.extend(samples);
        }
        if dropped > 0 {
            DECEASED_DROPPED.fetch_add(dropped, Ordering::Relaxed);
        }
    }
}

/// Registry of weak handles to every per-thread state still alive.
pub(crate) static REGISTRY: Mutex<Vec<Weak<Mutex<ThreadState>>>> = Mutex::new(Vec::new());

/// Samples salvaged from threads that have exited since the last drain.
static DECEASED_SAMPLES: Mutex<Vec<RawSample>> = Mutex::new(Vec::new());

/// Dropped-sample count salvaged from exited threads.
static DECEASED_DROPPED: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// Strong owner of this thread's state. The `ThreadHandle::drop`
    /// runs at thread exit and pushes leftover samples into the
    /// deceased queue.
    static THREAD_HANDLE: OnceCell<ThreadHandle> = const { OnceCell::new() };
}

/// Get this thread's state handle, lazily initialising on first use.
pub(crate) fn handle(config: &Config) -> Arc<Mutex<ThreadState>> {
    THREAD_HANDLE.with(|cell| {
        Arc::clone(
            &cell
                .get_or_init(|| {
                    crate::debug::dbglog!("registering new thread state");
                    let arc = Arc::new(Mutex::new(ThreadState::new(config)));
                    REGISTRY.lock().push(Arc::downgrade(&arc));
                    ThreadHandle { inner: arc }
                })
                .inner,
        )
    })
}

/// Drain every live thread's samples + the deceased queue into `out`. Returns
/// the total dropped-sample count.
pub(crate) fn drain_all(out: &mut Vec<RawSample>) -> u64 {
    crate::debug::dbglog!("drain_all: start");

    // Snapshot weaks under a brief REGISTRY lock; iterate without it so that
    // other threads can keep registering during a long drain.
    let weaks: Vec<Weak<Mutex<ThreadState>>> = {
        let reg = REGISTRY.lock();
        crate::debug::dbglog!("drain_all: registry size = {}", reg.len());
        reg.iter().cloned().collect()
    };

    let mut dropped_total: u64 = 0;
    let mut alive_count = 0;
    let mut dead_count = 0;
    for weak in &weaks {
        if let Some(arc) = weak.upgrade() {
            alive_count += 1;
            let mut st = arc.lock();
            let n = st.samples.len();
            out.extend(st.samples.drain(..));
            dropped_total = dropped_total.saturating_add(st.dropped_samples);
            st.dropped_samples = 0;
            crate::debug::dbglog!("drain_all: drained {} samples from a live thread", n);
        } else {
            dead_count += 1;
        }
    }

    // Drain salvaged samples from threads that exited.
    let salvaged = {
        let mut dec = DECEASED_SAMPLES.lock();
        let n = dec.len();
        out.extend(dec.drain(..));
        n
    };
    let salvaged_dropped = DECEASED_DROPPED.swap(0, Ordering::Relaxed);
    dropped_total = dropped_total.saturating_add(salvaged_dropped);

    crate::debug::dbglog!(
        "drain_all: salvaged {} samples ({} dropped) from deceased queue",
        salvaged,
        salvaged_dropped
    );

    // Prune dead Weaks if any (separate brief lock so we don't hold while
    // doing potentially-allocating work).
    if dead_count > 0 {
        let mut reg = REGISTRY.lock();
        reg.retain(|w| w.upgrade().is_some());
    }

    crate::debug::dbglog!(
        "drain_all: done, alive={}, dead={}, total_samples={}, dropped={}",
        alive_count,
        dead_count,
        out.len(),
        dropped_total
    );
    dropped_total
}
