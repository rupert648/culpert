//! Snapshot path: drain per-thread sample buffers, bucket by `(span, frames)`,
//! resolve symbols, return a [`Profile`].

use crate::config::Config;
use crate::sample::RawSample;
use crate::span::{SpanContext, SpanId, SpanMetadata};
use crate::thread_state;
use std::collections::HashMap;

/// One resolved stack frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Instruction pointer.
    pub ip: usize,
    /// Demangled symbol name, if resolved.
    pub name: Option<String>,
    /// Source file path, if available.
    pub filename: Option<String>,
    /// Source line number, if available.
    pub lineno: Option<u32>,
}

/// One aggregated bucket of allocation samples sharing the same span and
/// callsite.
#[derive(Clone, Debug)]
pub struct ProfileEntry {
    /// Span attribution. `None` for samples taken outside any span scope.
    pub span: Option<SpanId>,
    /// Resolved callsite, top-of-stack first.
    pub frames: Vec<Frame>,
    /// Bernstein-weighted, **unbiased** estimate of total bytes allocated
    /// in this `(span, callsite)` bucket.
    ///
    /// Each underlying sample contributes `bytes / (1 − exp(−bytes/rate_bytes))`,
    /// the inverse of the per-alloc sampling probability. Across many
    /// samples this sum converges to the true total bytes allocated;
    /// for individual buckets the Monte-Carlo standard error scales as
    /// `~sqrt(rate_bytes × bytes_total)`.
    ///
    /// (Pre-v0.2 versions of culpert exposed raw `Layout::size()` sums here
    /// and required downstream tooling to apply a heuristic correction;
    /// see [`crate::Config::rate_bytes`] and the v0.2 changelog entry on
    /// geometric sampling for the full story.)
    pub bytes_total: u64,
    /// Number of samples in this bucket.
    pub samples: u64,
}

/// A snapshot of accumulated allocation samples since the last snapshot.
///
/// Note: snapshots are *destructive* — taking a snapshot drains every
/// thread's per-thread sample buffer.
#[derive(Clone, Debug)]
pub struct Profile {
    /// One entry per unique `(span, callsite)` pair.
    pub entries: Vec<ProfileEntry>,
    /// Metadata for every span ID referenced by `entries`.
    pub spans: HashMap<SpanId, SpanMetadata>,
    /// Total samples dropped due to per-thread buffer overflow.
    /// A non-zero value means the profile under-counts allocations
    /// proportionally on the affected threads — increase
    /// [`Config::buffer_capacity`] or snapshot more frequently.
    pub dropped_samples: u64,
    /// Copy of the config the profile was taken with, for downstream tools.
    pub config: Config,
}

pub(crate) fn snapshot(config: &Config, ctx: &dyn SpanContext) -> Profile {
    crate::debug::dbglog!("aggregator::snapshot: starting drain");

    // Drain every live thread.
    let mut raw: Vec<RawSample> = Vec::new();
    let dropped_samples = thread_state::drain_all(&mut raw);

    crate::debug::dbglog!("aggregator::snapshot: drain done, {} raw samples", raw.len());

    // Bucket by (span, hash of frames). We keep raw IPs for the bucket key
    // and the canonical frame list, then resolve symbols once per bucket
    // at the end (resolution is by far the most expensive step).
    //
    // Each sample's contribution to the per-bucket `bytes_total` is the
    // Bernstein-corrected weight, not the raw `Layout::size()`. See
    // `bernstein_weight` below.
    let mut buckets: HashMap<(Option<SpanId>, u64), Bucket> = HashMap::new();
    for s in raw {
        let key = (s.span, hash_frames(&s.frames));
        let entry = buckets.entry(key).or_insert_with(|| Bucket {
            frames: s.frames.iter().copied().collect(),
            bytes_total: 0,
            samples: 0,
        });
        entry.bytes_total = entry
            .bytes_total
            .saturating_add(bernstein_weight(s.bytes, config.rate_bytes));
        entry.samples = entry.samples.saturating_add(1);
    }

    crate::debug::dbglog!("aggregator::snapshot: {} buckets, resolving symbols", buckets.len());

    let entries: Vec<ProfileEntry> = buckets
        .into_iter()
        .map(|((span, _), b)| ProfileEntry {
            span,
            frames: resolve_frames(&b.frames),
            bytes_total: b.bytes_total,
            samples: b.samples,
        })
        .collect();

    crate::debug::dbglog!("aggregator::snapshot: resolved, looking up span metadata");

    // Seed the spans map with metadata for every span referenced by a
    // sample, then walk parent chains transitively so the report can
    // build a complete tree even when a parent span had no allocations
    // of its own (and therefore no direct samples). The `entry`-based
    // dedupe handles cycles automatically: if a node is already in the
    // map, the `or_insert_with` closure doesn't run and we don't
    // re-enqueue its parent.
    let mut spans: HashMap<SpanId, SpanMetadata> = HashMap::new();
    let mut frontier: Vec<SpanId> = Vec::new();
    for entry in &entries {
        if let Some(id) = entry.span {
            spans.entry(id).or_insert_with(|| {
                let meta = ctx.metadata(id).unwrap_or_else(|| SpanMetadata {
                    name: format!("<unknown:{}>", id.get()),
                    parent: None,
                });
                if let Some(parent) = meta.parent {
                    frontier.push(parent);
                }
                meta
            });
        }
    }
    while let Some(parent_id) = frontier.pop() {
        spans.entry(parent_id).or_insert_with(|| {
            let meta = ctx.metadata(parent_id).unwrap_or_else(|| SpanMetadata {
                name: format!("<unknown:{}>", parent_id.get()),
                parent: None,
            });
            if let Some(grandparent) = meta.parent {
                frontier.push(grandparent);
            }
            meta
        });
    }

    crate::debug::dbglog!("aggregator::snapshot: done");

    Profile {
        entries,
        spans,
        dropped_samples,
        config: config.clone(),
    }
}

struct Bucket {
    frames: Vec<usize>,
    bytes_total: u64,
    samples: u64,
}

/// FNV-1a 64-bit. Deterministic, fast, no DOS-resistance needed for IP hashes.
fn hash_frames(frames: &[usize]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for ip in frames {
        h ^= *ip as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Bernstein correction for geometric sampling.
///
/// Under geometric sampling, an allocation of `bytes` bytes is observed with
/// probability `p = 1 − exp(−bytes/rate)` (the chance that at least one
/// sample point falls within the alloc). Weighting each observed sample by
/// `1/p` yields an unbiased estimator of total bytes allocated:
///
/// ```text
/// E[weight | sampled] · P(sampled) = bytes/p · p = bytes
/// ```
///
/// For `bytes >> rate` (alloc guaranteed sampled) this reduces to `bytes`.
/// For `bytes << rate` it inflates to ~`rate`, statistically representing
/// the many similar small allocs that *weren't* sampled.
///
/// `rate == 0` falls back to raw bytes (sampling disabled, no correction
/// needed). Numerical underflow on absurdly small allocs falls back to raw
/// bytes too — the under-attribution is bounded by `f64::EPSILON · bytes`,
/// well below any meaningful resolution.
fn bernstein_weight(bytes: u64, rate: u64) -> u64 {
    if rate == 0 {
        return bytes;
    }
    let b = bytes as f64;
    let r = rate as f64;
    let p_sampled = 1.0 - (-b / r).exp();
    (b / p_sampled.max(f64::EPSILON)) as u64
}

fn resolve_frames(ips: &[usize]) -> Vec<Frame> {
    ips.iter()
        .map(|&ip| {
            let mut name: Option<String> = None;
            let mut filename: Option<String> = None;
            let mut lineno: Option<u32> = None;
            // SAFETY: backtrace::resolve takes a *mut c_void which it does not
            // dereference; it consults debug info. The cast is safe.
            backtrace::resolve(ip as *mut _, |sym| {
                if name.is_none() {
                    name = sym.name().map(|n| n.to_string());
                }
                if filename.is_none() {
                    filename = sym.filename().map(|p| p.to_string_lossy().into_owned());
                }
                if lineno.is_none() {
                    lineno = sym.lineno();
                }
            });
            Frame {
                ip,
                name,
                filename,
                lineno,
            }
        })
        .collect()
}
