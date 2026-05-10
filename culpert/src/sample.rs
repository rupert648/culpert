//! Internal sample shape. Not part of the public API; the public surface is
//! [`Profile`](crate::Profile) emitted by [`snapshot`](crate::snapshot).

use crate::span::SpanId;
use smallvec::SmallVec;

/// A single sampled allocation event. Recorded on the per-thread fast path,
/// drained by `snapshot()` into the global aggregator.
///
/// Frames are kept as raw instruction pointers (`usize`); symbol resolution is
/// deferred to export time (see [`crate::aggregator`]).
pub(crate) struct RawSample {
    pub(crate) span: Option<SpanId>,
    /// The size of the underlying allocation in bytes (not the sample weight).
    pub(crate) bytes: u64,
    /// Captured callsite as raw IPs, top-of-stack first.
    pub(crate) frames: SmallVec<[usize; 32]>,
}
