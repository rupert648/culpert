//! [`TrackingAllocator`] — drop-in `#[global_allocator]` wrapper.

use crate::sampler;
use std::alloc::{GlobalAlloc, Layout};

/// A `GlobalAlloc` wrapper that observes successful allocations.
///
/// Constructed in `static` position with [`TrackingAllocator::new`]. The
/// allocator is a pure forwarder until [`crate::install`] is called; before
/// install, the only overhead is one extra non-allocating function call per
/// alloc (which the optimiser can usually inline away).
///
/// # Example
///
/// ```ignore
/// use std::alloc::System;
/// use culpert::TrackingAllocator;
///
/// #[global_allocator]
/// static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);
/// ```
pub struct TrackingAllocator<A: GlobalAlloc> {
    inner: A,
}

impl<A: GlobalAlloc> TrackingAllocator<A> {
    /// Wrap an inner `GlobalAlloc` implementation.
    pub const fn new(inner: A) -> Self {
        Self { inner }
    }
}

unsafe impl<A: GlobalAlloc> GlobalAlloc for TrackingAllocator<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarding to the inner allocator with the caller's layout.
        let ptr = unsafe { self.inner.alloc(layout) };
        if !ptr.is_null() {
            sampler::observe(layout.size() as u64);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: pointer + layout invariants come from the caller.
        unsafe { self.inner.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarding.
        let ptr = unsafe { self.inner.alloc_zeroed(layout) };
        if !ptr.is_null() {
            sampler::observe(layout.size() as u64);
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarding.
        let new_ptr = unsafe { self.inner.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() && new_size > layout.size() {
            // Treat realloc-grow as a fresh alloc of the *delta*. Realloc-shrink
            // contributes zero bytes to the profile.
            sampler::observe((new_size - layout.size()) as u64);
        }
        new_ptr
    }
}
