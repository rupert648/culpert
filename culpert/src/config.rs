//! Profiler configuration.

use std::collections::HashMap;

/// How [`TrackingAllocator`](crate::TrackingAllocator) walks the call
/// stack on each sampled allocation.
///
/// See [`crate::stack_capture`] for the underlying mechanics. tl;dr:
/// `Backtrace` is portable and exact at the cost of ~100 ns–several µs
/// per frame; `FramePointer` is ~5–10 ns per frame on x86_64 / aarch64
/// builds compiled with `-C force-frame-pointers=yes` (and falls back
/// to `Backtrace` elsewhere).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StackCaptureStrategy {
    /// Use the [`backtrace`] crate, which consults the compiler-emitted
    /// Call Frame Information tables to unwind. Works on every
    /// supported platform regardless of build flags. Costs roughly a
    /// few hundred nanoseconds to several microseconds per frame
    /// because the unwinder has to look up the current instruction
    /// pointer in a metadata table and evaluate a small program per
    /// frame. **This is the default.**
    #[default]
    Backtrace,

    /// Walk the frame-pointer linked list directly: two memory loads
    /// per frame, no metadata table lookup. Requires the program (and
    /// any dependencies whose frames you want to see) to have been
    /// compiled with `RUSTFLAGS="-C force-frame-pointers=yes"` —
    /// otherwise the walk terminates at the first frame-pointer-less
    /// function and you get a shallow stack.
    ///
    /// Implemented for `target_arch = "x86_64"` and `target_arch =
    /// "aarch64"`. On other architectures this variant silently falls
    /// back to [`Backtrace`](Self::Backtrace) — the user asked for the
    /// fast path, we give them the best available one without breaking
    /// the build.
    FramePointer,
}

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

    /// How to walk the stack on each sample. See [`StackCaptureStrategy`]
    /// for trade-offs. Default [`Backtrace`](StackCaptureStrategy::Backtrace).
    pub stack_capture_strategy: StackCaptureStrategy,

    /// Arbitrary key/value pairs to embed in every exported profile.
    ///
    /// Written verbatim into the pprof file's `comment` field (one entry
    /// per `key=value` line, see the pprof spec). Read back by
    /// `culpert::pprof::metadata()` and surfaced by `culpert info`.
    ///
    /// Intended for build / git / CI annotations so a profile is
    /// self-describing on the wire — typical entries:
    ///
    /// ```rust
    /// # use culpert::Config;
    /// # let _ =
    /// Config {
    ///     metadata: [
    ///         ("commit_sha", env!("CARGO_PKG_VERSION")),
    ///         ("service",    "edge-worker"),
    ///         ("branch",     "main"),
    ///     ]
    ///     .into_iter()
    ///     .map(|(k, v)| (k.to_string(), v.to_string()))
    ///     .collect(),
    ///     ..Default::default()
    /// }
    /// # ;
    /// ```
    ///
    /// Default is empty. Keys must not contain `=` (the separator is
    /// recovered by splitting on the first `=`). Values are arbitrary.
    pub metadata: HashMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            rate_bytes: 1 << 19, // 512 KiB
            stack_depth: 32,
            buffer_capacity: 1024,
            stack_capture_strategy: StackCaptureStrategy::Backtrace,
            metadata: HashMap::new(),
        }
    }
}
