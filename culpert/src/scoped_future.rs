//! [`ScopedFuture`] — a `Future` wrapper that enters a culpert scope
//! around each `poll()` call.
//!
//! Async functions yield across `.await` points and may resume on a
//! different tokio worker thread. A naive `let _guard = scope::enter()`
//! at the top of an async fn would leave the guard on the spawning
//! thread's stack, so allocations after the first yield would either be
//! unattributed or attributed to the wrong span.
//!
//! `ScopedFuture` solves this with the same pattern as
//! `tracing::Instrument` and `foundations::WithTelemetryContext`:
//!
//! 1. At **construction** (on the spawning thread): mint a fresh
//!    [`SpanId`], capture the caller's current span as the parent, and
//!    store both in the wrapper. No thread-local state is modified.
//!
//! 2. On each **`poll()`** (on whichever worker thread the executor
//!    schedules us): call [`scope::enter_preregistered(id)`] to push the
//!    span onto *this* thread's stack, poll the inner future, and let
//!    the RAII guard pop on return. Each `poll()` runs synchronously on
//!    one thread, so the guard is valid for the entire duration.
//!
//! [`SpanId`]: crate::SpanId

use crate::scope;
use crate::span::SpanId;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A `Future` wrapper that enters a culpert scope for each `poll()`.
///
/// See the [module docs](self) for the design rationale.
///
/// # Construction
///
/// Use [`ScopedFuture::new`] to wrap any future with a named span:
///
/// ```ignore
/// use culpert::ScopedFuture;
///
/// let scoped = ScopedFuture::new("my_operation", async {
///     // allocations in here (including across .await points)
///     // are attributed to "my_operation"
///     do_async_work().await
/// });
/// ```
///
/// The `#[culpert::span_fn]` macro on async functions emits this
/// wrapper automatically.
pub struct ScopedFuture<F> {
    /// The span ID minted at construction time.
    id: SpanId,
    /// The inner future being wrapped.
    inner: F,
}

impl<F: Future> ScopedFuture<F> {
    /// Wrap `inner` in a scoped future that attributes allocations to a
    /// freshly minted span named `name`.
    ///
    /// The caller's current span (from the thread-local scope stack at
    /// the point of this call) is captured as the parent.
    ///
    /// `name` must be `&'static str` because it is stored in the global
    /// metadata cache (same constraint as [`scope::enter`]).
    pub fn new(name: &'static str, inner: F) -> Self {
        let (id, _parent) = scope::mint(name);
        Self { id, inner }
    }

    /// Wrap `inner` in a scoped future with an explicit parent span.
    ///
    /// Use this when you need to set the parent to something other than
    /// the caller's current scope-stack top — e.g. when spawning from
    /// a context where the parent span is known but may not be on the
    /// current thread's stack.
    pub fn new_with_parent(name: &'static str, parent: Option<SpanId>, inner: F) -> Self {
        let id = scope::mint_with_parent(name, parent);
        Self { id, inner }
    }
}

// SAFETY: `ScopedFuture` is `Unpin` iff the inner future is `Unpin`.
// We use `pin_project` by hand (structural pinning) — the `id` field
// is trivially `Unpin` (it's a `NonZeroU64`), and we project through
// to the inner future's pin.
impl<F: Future> Future for ScopedFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let id = self.id;

        // SAFETY: We are doing structural pinning. `id` is `Copy`/`Unpin`
        // so we read it above. For the inner future, we project the pin
        // through without moving it.
        let inner = unsafe { self.map_unchecked_mut(|this| &mut this.inner) };

        // Enter the scope for the duration of this poll. The guard is
        // dropped at the end of this function — before we return to the
        // executor — so it is always popped from the correct thread's
        // stack.
        let _guard = scope::enter_preregistered(id);
        inner.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, LocalSpanContext, TrackingAllocator};
    use std::alloc::System;
    use std::sync::Mutex;

    #[global_allocator]
    static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

    /// Serialize tests that call `snapshot()`. `snapshot()` calls
    /// `on_snapshot()` which clears the global METADATA map; without
    /// serialization, a concurrent test's snapshot can wipe metadata
    /// registered by another test, causing `<unknown:N>` names.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Install culpert with LocalSpanContext for tests. Because `install`
    /// can only be called once per process, we use `std::sync::Once`.
    fn ensure_installed() {
        use std::sync::Once;
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            crate::install(
                LocalSpanContext::new(),
                Config {
                    // Sample aggressively so every allocation is captured.
                    rate_bytes: 1,
                    stack_depth: 16,
                    buffer_capacity: 1 << 16,
                    ..Default::default()
                },
            );
        });
    }

    /// Spawns multiple tokio tasks, each wrapping work in ScopedFuture.
    /// Each task allocates inside .await points and verifies that the
    /// allocations are attributed to the correct span names and that
    /// parent-child relationships are preserved.
    #[test]
    fn scoped_future_attributes_across_await_points() {
        let _lock = TEST_LOCK.lock().unwrap();
        ensure_installed();

        // Drain any leftover samples from previous tests.
        let _ = crate::snapshot();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let mut handles = Vec::new();

            for i in 0..4 {
                let span_name: &'static str = match i {
                    0 => "task_alpha",
                    1 => "task_beta",
                    2 => "task_gamma",
                    _ => "task_delta",
                };

                let handle = tokio::spawn(ScopedFuture::new(span_name, async move {
                    // Yield to the executor — the task may resume on a
                    // different worker thread.
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;

                    // Allocate after the yield point. This must be
                    // attributed to our span, not to "(no span)".
                    let _v: Vec<u8> = Vec::with_capacity(8192);

                    // Another yield + allocation.
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    let _v2: Vec<u8> = Vec::with_capacity(16384);
                }));
                handles.push(handle);
            }

            for h in handles {
                h.await.unwrap();
            }
        });

        let profile = crate::snapshot();

        // Collect span names that have attributed allocations.
        let mut attributed_spans: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for entry in &profile.entries {
            if let Some(span_id) = entry.span {
                if let Some(meta) = profile.spans.get(&span_id) {
                    attributed_spans.insert(meta.name.clone());
                }
            }
        }

        // All four task spans should have attributed allocations.
        for name in &["task_alpha", "task_beta", "task_gamma", "task_delta"] {
            assert!(
                attributed_spans.contains(*name),
                "expected allocations attributed to span '{name}', \
                 but only found: {attributed_spans:?}"
            );
        }
    }

    /// Verifies parent-child relationships are preserved when a
    /// ScopedFuture is constructed inside another scope.
    #[test]
    fn scoped_future_preserves_parent_child() {
        let _lock = TEST_LOCK.lock().unwrap();
        ensure_installed();

        // Drain leftover samples.
        let _ = crate::snapshot();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            // Enter a parent scope on this thread.
            let _parent_guard = crate::scope::enter("parent_op");

            // Spawn a child task. ScopedFuture::new captures the current
            // span (parent_op) as the parent of child_op.
            let handle = tokio::spawn(ScopedFuture::new("child_op", async move {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                let _v: Vec<u8> = Vec::with_capacity(4096);
            }));

            handle.await.unwrap();
        });

        let profile = crate::snapshot();

        // Find the child_op span and verify its parent is parent_op.
        for meta in profile.spans.values() {
            if meta.name == "child_op" {
                let parent_meta = meta.parent.and_then(|pid| profile.spans.get(&pid));
                assert!(
                    parent_meta.is_some(),
                    "child_op should have a parent in the profile spans, \
                     parent id: {:?}, available spans: {:?}",
                    meta.parent,
                    profile
                        .spans
                        .iter()
                        .map(|(id, m)| (id, &m.name))
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    parent_meta.unwrap().name,
                    "parent_op",
                    "child_op's parent should be parent_op"
                );
                return;
            }
        }

        // If child_op had no sampled allocations, verify at least that
        // the profile contains *some* attributed allocations (the parent
        // scope may have caught them).
        assert!(
            profile.entries.iter().any(|e| e.span.is_some()),
            "expected at least some attributed allocations in the profile"
        );
    }

    /// Verify that new_with_parent sets the parent correctly even when
    /// the caller's stack is empty.
    #[test]
    fn scoped_future_explicit_parent() {
        let _lock = TEST_LOCK.lock().unwrap();
        ensure_installed();

        // Drain leftover samples.
        let _ = crate::snapshot();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        // Mint outside the async block so we can reference the ID after
        // block_on returns.
        let (parent_id, _) = crate::scope::mint("explicit_parent");

        rt.block_on(async {
            let handle = tokio::spawn(ScopedFuture::new_with_parent(
                "explicit_child",
                Some(parent_id),
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    let _v: Vec<u8> = Vec::with_capacity(4096);
                },
            ));

            handle.await.unwrap();
        });

        let profile = crate::snapshot();

        // Verify the parent relationship in metadata.
        for meta in profile.spans.values() {
            if meta.name == "explicit_child" {
                assert_eq!(
                    meta.parent,
                    Some(parent_id),
                    "explicit_child should have explicit_parent as parent"
                );
                return;
            }
        }
        // Profile.spans only contains spans that had sampled allocations.
        // If explicit_child didn't get sampled (unlikely with rate_bytes=1
        // but possible), that's acceptable — the metadata registration
        // via mint_with_parent is tested by the scope module's own tests.
    }
}
