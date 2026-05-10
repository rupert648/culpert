//! Q3: Async propagation across worker threads.
//!
//! "When a future moves between worker threads via `WithTelemetryContext`,
//!  how does the span context follow? What hook fires?"
//!
//! Source-level answer (read in `telemetry_context.rs:39-43`):
//!     impl Future for WithTelemetryContext {
//!         fn poll(mut self, cx) -> Poll<_> {
//!             let _scope = self.ctx.scope();   // pushes onto thread-local stack
//!             self.inner.as_mut().poll(cx)     // user code sees correct current_span()
//!         }                                     // _scope drops, pops the stack
//!     }
//!
//! That is: the scope is re-entered on every poll. Per-thread current span
//! tracks the polling thread automatically. This experiment confirms it
//! empirically by spawning a future onto a multi-thread runtime and reading
//! `rustracing_span()` from inside, possibly on a different worker.

use foundations::reexports_for_macros::cf_rustracing::span::InspectableSpan;
use foundations::telemetry::TelemetryContext;
use foundations::telemetry::tracing::{self, rustracing_span};
use std::sync::Arc;
use tokio::time::Duration;

pub async fn run() {
    println!("Q3: async propagation across worker threads");
    println!("-------------------------------------------");

    // Use the test telemetry context so spans are real (non-Inactive) and have
    // visible names. This makes the empirical evidence non-trivial.
    let test_ctx = TelemetryContext::test();
    let _ctx_scope = test_ctx.scope();

    let _root = tracing::span("root_q3");
    let outer = rustracing_span().unwrap();
    let outer_name = outer.read().operation_name().to_owned();
    let outer_thread = std::thread::current().id();
    println!("outer: thread={outer_thread:?}, span_name={outer_name:?}");

    // Spawn onto the runtime, wrapping the future with the current TelemetryContext.
    let task = tokio::spawn(TelemetryContext::current().apply(async move {
        // Force migration: yield repeatedly + sleep to let the scheduler park us.
        for _ in 0..5 {
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let inside = rustracing_span().expect("span should be live inside spawned task");
        let name = inside.read().operation_name().to_owned();
        let ptr = Arc::as_ptr(&inside) as usize;
        let tid = std::thread::current().id();

        // Take a child span and confirm hierarchy still works inside.
        let _child = tracing::span("child_in_task");
        let child_name = rustracing_span().unwrap().read().operation_name().to_owned();
        (name, ptr, tid, child_name)
    }));

    let (name_in, ptr_in, tid_in, child_name) = task.await.unwrap();
    println!("inside: thread={tid_in:?}, span_name={name_in:?}, ptr=0x{ptr_in:x}");
    println!("inside: child span name={child_name:?}");

    println!();
    if name_in == outer_name {
        println!(
            "VERDICT: span context follows the future across worker threads (poll() \
             re-enters the scope, current_span() resolves to the task's span)."
        );
    } else {
        println!(
            "UNEXPECTED: span name diverged across .spawn(). Investigate before Phase 1."
        );
    }
    if outer_thread != tid_in {
        println!(
            "Bonus: confirmed thread migration across {outer_thread:?} -> {tid_in:?}."
        );
    } else {
        println!(
            "Note: scheduler kept the task on the same thread this run. The poll-based \
             scope re-entry is still the mechanism, just not visibly cross-thread here."
        );
    }
}
