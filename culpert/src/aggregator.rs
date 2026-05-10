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
    /// Sum of `Layout::size()` across all samples in this bucket. This is
    /// raw allocated bytes — not weighted by sample rate. Multiply by the
    /// sample rate to get an unbiased estimate of total bytes (or use the
    /// `bytes_total * rate_bytes` convention pprof exporters use).
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
    let mut buckets: HashMap<(Option<SpanId>, u64), Bucket> = HashMap::new();
    for s in raw {
        let key = (s.span, hash_frames(&s.frames));
        let entry = buckets.entry(key).or_insert_with(|| Bucket {
            frames: s.frames.iter().copied().collect(),
            bytes_total: 0,
            samples: 0,
        });
        entry.bytes_total = entry.bytes_total.saturating_add(s.bytes);
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

    let mut spans = HashMap::new();
    for entry in &entries {
        if let Some(id) = entry.span {
            spans.entry(id).or_insert_with(|| {
                ctx.metadata(id).unwrap_or_else(|| SpanMetadata {
                    name: format!("<unknown:{}>", id.get()),
                    parent: None,
                })
            });
        }
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
