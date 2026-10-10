use culpert::{Config, LocalSpanContext, TrackingAllocator};
use std::alloc::System;

#[global_allocator]
static GLOBAL: TrackingAllocator<System> = TrackingAllocator::new(System);

#[test]
fn dropping_profiler_guard_stops_allocation_observation() {
    let profiler = culpert::install(
        LocalSpanContext::new(),
        Config {
            rate_bytes: 1,
            stack_depth: 8,
            buffer_capacity: 1 << 12,
            ..Default::default()
        },
    );

    for _ in 0..16 {
        let allocation = Vec::<u8>::with_capacity(4096);
        std::hint::black_box(&allocation);
    }

    let observed_before_drop = !culpert::snapshot().entries.is_empty();

    // Drain anything allocated between the first snapshot and guard drop so
    // the final snapshot can only contain post-drop observations.
    drop(culpert::snapshot());
    drop(profiler);

    for _ in 0..16 {
        let allocation = Vec::<u8>::with_capacity(4096);
        std::hint::black_box(&allocation);
    }

    let after_drop = culpert::snapshot();
    assert!(observed_before_drop, "profiler should observe allocations");
    assert!(
        after_drop.entries.is_empty(),
        "dropping ProfilerGuard should stop allocation observation"
    );
}
