//! Sampler RNG helpers, backed by `fastrand`.
//!
//! `fastrand` keeps a thread-local wyrand state seeded from the OS
//! entropy source on first use, so different process runs produce
//! different sequences and different threads within a process produce
//! independent sequences — both load-bearing for the geometric sampler
//! (deterministic seeds would make sampler decisions identical across
//! runs of the same workload, giving a false impression of zero
//! variance, and identical across sibling processes, masking real
//! cross-tenant signal).

/// Draw the next sample interval (in bytes) from `Geometric(1/rate_bytes)`.
///
/// The geometric distribution is the discrete analog of the exponential:
/// "how many independent Bernoulli(p) trials until the first success".
/// Drawn via the inverse-CDF identity `gap = floor(-ln(u) / p)` with
/// `u ~ Uniform(0, 1]`. The exact formula is `floor(ln(u) / ln(1 - p))`;
/// for any reasonable sampling rate (>= a few KiB) the approximation
/// `ln(1 - p) ≈ -p` is accurate to better than 1 part in 100k and
/// avoids an extra `ln` call.
///
/// Returned interval is always `>= 1`.
#[inline]
pub(crate) fn geometric_interval(rate_bytes: u64) -> u64 {
    // .max(EPSILON) guards against the rare fastrand-returns-exact-0
    // that would give us `-ln(0) = +inf`.
    let u = fastrand::f64().max(f64::EPSILON);
    let gap = (-u.ln()) * rate_bytes as f64;
    (gap as u64).max(1)
}
