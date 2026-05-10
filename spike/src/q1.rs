//! Q1: Allocator reentrancy with foundations.
//!
//! "Does `TelemetryContext::current()` allocate? If yes, can we read a
//!  thread-local pointer to the current span ID without going through
//!  foundations' API?"
//!
//! Approach: install a counting global allocator, call
//! `TelemetryContext::current()` in a tight loop, count allocations.
//! Repeat on a fresh thread to surface lazy thread-local init costs.

use foundations::telemetry::TelemetryContext;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

pub static ALLOCS: AtomicUsize = AtomicUsize::new(0);

/// Global allocator that defers to System and bumps a counter on each alloc.
pub struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarding to the system allocator with the same layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: pointer was produced by System.alloc with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

pub fn run() {
    println!("Q1: allocator reentrancy with TelemetryContext::current()");
    println!("---------------------------------------------------------");

    // Warm-up so any one-off thread-local init on the main thread is paid before measurement.
    let _ = std::hint::black_box(TelemetryContext::current());

    let n = 100_000;
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..n {
        std::hint::black_box(TelemetryContext::current());
    }
    let main_thread = ALLOCS.load(Ordering::Relaxed) - before;
    println!("steady-state main thread: {n} calls -> {main_thread} allocs");

    // Fresh thread: how much does the very first call cost? This is the
    // "thread-local cell init" charge that re-entrancy protection must absorb.
    let handle = std::thread::spawn(|| {
        let before = ALLOCS.load(Ordering::Relaxed);
        let _ = std::hint::black_box(TelemetryContext::current());
        let mid = ALLOCS.load(Ordering::Relaxed);
        for _ in 0..1000 {
            std::hint::black_box(TelemetryContext::current());
        }
        let after = ALLOCS.load(Ordering::Relaxed);
        (mid - before, after - mid)
    });
    let (first, next_1k) = handle.join().unwrap();
    println!("fresh thread: 1st call -> {first} allocs; next 1000 -> {next_1k} allocs");

    println!();
    if main_thread == 0 {
        println!("VERDICT (steady state): TelemetryContext::current() is alloc-free.");
    } else {
        println!(
            "VERDICT (steady state): {:.4} allocs/call on average.",
            main_thread as f64 / n as f64
        );
    }
    if first > 0 {
        println!(
            "First call on a fresh thread allocates ({first}). Mitigation: a per-thread \
             reentrancy guard in TrackingAllocator must short-circuit recursive calls."
        );
    }
}
