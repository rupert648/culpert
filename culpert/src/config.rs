//! Profiler configuration.

/// Configuration for the [installed profiler](crate::install).
#[derive(Clone, Debug)]
pub struct Config {
    /// Mean bytes between samples. Higher = lower overhead, coarser signal.
    /// Default 512 KiB, matching jemalloc's heap profiler.
    pub rate_bytes: u64,

    /// Maximum number of stack frames captured per sample. Frames beyond
    /// this depth are dropped.
    pub stack_depth: usize,

    /// Per-thread sample buffer capacity. When full, additional samples on
    /// that thread are dropped and counted via
    /// [`Profile::dropped_samples`](crate::Profile::dropped_samples).
    pub buffer_capacity: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            rate_bytes: 1 << 19, // 512 KiB
            stack_depth: 32,
            buffer_capacity: 1024,
        }
    }
}
