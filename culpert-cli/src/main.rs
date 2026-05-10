//! culpert-cli — `culpert report` for now, `culpert diff` planned for v0.2.
//!
//! Reads a culpert pprof profile (gzipped) and prints a human-readable
//! summary. Two views:
//!
//! - top spans (default): one row per `span_name`, sorted by bytes
//! - per-span callsites (`--span <name>`): top callsites within that span

use clap::{Parser, Subcommand};
use culpert::pprof::{self, proto};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "culpert",
    version,
    about = "Per-span heap allocation profile reporter."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Pretty-print a culpert pprof profile.
    Report {
        /// Path to the .pb.gz profile file.
        file: PathBuf,

        /// Filter to a specific span_name and show top callsites within it.
        #[arg(long, value_name = "NAME", conflicts_with = "no_span")]
        span: Option<String>,

        /// Filter to samples that have NO span_name label (i.e. allocations
        /// taken outside any foundations span — tokio runtime, framework
        /// internals, uninstrumented code paths).
        #[arg(long)]
        no_span: bool,

        /// Limit number of rows in the output.
        #[arg(long, default_value = "20")]
        top: usize,
    },
}

fn main() {
    let cli = Cli::parse();
    let res = match cli.cmd {
        Cmd::Report {
            file,
            span,
            no_span,
            top,
        } => {
            if let Some(name) = span {
                run_callsites(&file, Filter::WithSpan(name), top)
            } else if no_span {
                run_callsites(&file, Filter::NoSpan, top)
            } else {
                run_top_spans(&file, top)
            }
        }
    };
    if let Err(e) = res {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Selection criterion for a callsite report.
enum Filter {
    /// Only include samples tagged with `span_name = <this>`.
    WithSpan(String),
    /// Only include samples that have no `span_name` label.
    NoSpan,
}

fn load(path: &PathBuf) -> Result<proto::Profile, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    Ok(pprof::decode_gzipped(&bytes)?)
}

// ---- top spans report --------------------------------------------------

struct SpanRow {
    name: String,
    samples: u64,
    bytes_total: u64,
    /// Bias-corrected estimate. See module-level explanation.
    estimated_bytes: u64,
}

fn run_top_spans(path: &PathBuf, top: usize) -> Result<(), Box<dyn std::error::Error>> {
    let profile = load(path)?;
    let rate_bytes = profile.period.max(1) as u64;
    let mut rows = aggregate_by_span(&profile, rate_bytes);

    rows.sort_by(|a, b| b.estimated_bytes.cmp(&a.estimated_bytes));

    let total_bytes: u64 = rows.iter().map(|r| r.bytes_total).sum();
    let total_estimated: u64 = rows.iter().map(|r| r.estimated_bytes).sum();
    let total_samples: u64 = rows.iter().map(|r| r.samples).sum();

    println!(
        "Top spans by allocation ({} samples, sample rate {}/alloc):",
        total_samples,
        format_bytes(rate_bytes)
    );
    println!(
        "  raw_bytes        = sum of Layout::size() over sampled allocations.\n  \
           estimated_bytes  = bias-corrected: each sample of size < rate counts for `rate`.\n  \
           Use estimated_bytes for total-volume comparisons; raw_bytes is what stock pprof shows."
    );
    println!();
    print_span_table(&rows, total_bytes, total_estimated, top);
    Ok(())
}

fn aggregate_by_span(profile: &proto::Profile, rate_bytes: u64) -> Vec<SpanRow> {
    let span_name_key = string_index(profile, "span_name");

    let mut by_name: HashMap<String, SpanRow> = HashMap::new();
    for sample in &profile.sample {
        let name = span_name_key
            .and_then(|k| sample.label.iter().find(|l| l.key == k))
            .and_then(|label| profile.string_table.get(label.str as usize))
            .cloned()
            .unwrap_or_else(|| "(no span)".to_string());

        let count = sample.value.first().copied().unwrap_or(0).max(0) as u64;
        let bytes = sample.value.get(1).copied().unwrap_or(0).max(0) as u64;

        // Bias-corrected estimate per sample bucket.
        // avg = bytes / count is the mean Layout::size() in this bucket.
        // Each underlying alloc contributes max(avg, rate) to the unbiased total.
        let avg = if count == 0 { 0 } else { bytes / count };
        let estimated = if avg < rate_bytes {
            count.saturating_mul(rate_bytes)
        } else {
            bytes
        };

        let row = by_name.entry(name.clone()).or_insert(SpanRow {
            name,
            samples: 0,
            bytes_total: 0,
            estimated_bytes: 0,
        });
        row.samples = row.samples.saturating_add(count);
        row.bytes_total = row.bytes_total.saturating_add(bytes);
        row.estimated_bytes = row.estimated_bytes.saturating_add(estimated);
    }

    by_name.into_values().collect()
}

fn print_span_table(rows: &[SpanRow], total_bytes: u64, total_estimated: u64, top: usize) {
    let name_w = rows
        .iter()
        .take(top)
        .map(|r| r.name.len())
        .max()
        .unwrap_or(10)
        .max(10);

    println!(
        "  {:<name_w$}  {:>10}  {:>14}  {:>7}  {:>14}  {:>7}",
        "span", "samples", "raw_bytes", "raw %", "est_bytes", "est %",
        name_w = name_w
    );
    println!("  {:-<name_w$}  {:->10}  {:->14}  {:->7}  {:->14}  {:->7}", "", "", "", "", "", "", name_w = name_w);

    for row in rows.iter().take(top) {
        let raw_pct = pct(row.bytes_total, total_bytes);
        let est_pct = pct(row.estimated_bytes, total_estimated);
        println!(
            "  {:<name_w$}  {:>10}  {:>14}  {:>6.2}%  {:>14}  {:>6.2}%",
            row.name,
            row.samples,
            format_bytes(row.bytes_total),
            raw_pct,
            format_bytes(row.estimated_bytes),
            est_pct,
            name_w = name_w,
        );
    }

    if rows.len() > top {
        println!("  ... ({} more rows hidden)", rows.len() - top);
    }
}

// ---- per-span callsites report -----------------------------------------

struct CallsiteRow {
    label: String,
    samples: u64,
    bytes_total: u64,
    estimated_bytes: u64,
}

fn run_callsites(
    path: &PathBuf,
    filter: Filter,
    top: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let profile = load(path)?;
    let rate_bytes = profile.period.max(1) as u64;
    let mut rows = aggregate_callsites(&profile, &filter, rate_bytes);

    rows.sort_by(|a, b| b.estimated_bytes.cmp(&a.estimated_bytes));

    let total_bytes: u64 = rows.iter().map(|r| r.bytes_total).sum();
    let total_estimated: u64 = rows.iter().map(|r| r.estimated_bytes).sum();

    let header = match &filter {
        Filter::WithSpan(name) => {
            format!(
                "Top callsites within span {name:?} (sample rate {}/alloc):",
                format_bytes(rate_bytes)
            )
        }
        Filter::NoSpan => format!(
            "Top callsites in unattributed samples — outside any foundations span \
             (sample rate {}/alloc):",
            format_bytes(rate_bytes)
        ),
    };

    if rows.is_empty() {
        match &filter {
            Filter::WithSpan(name) => println!("No samples found for span_name = {name:?}."),
            Filter::NoSpan => println!("No unattributed samples — every sample has a span_name."),
        }
        return Ok(());
    }

    println!("{header}");
    println!();
    print_callsite_table(&rows, total_bytes, total_estimated, top);
    Ok(())
}

fn aggregate_callsites(
    profile: &proto::Profile,
    filter: &Filter,
    rate_bytes: u64,
) -> Vec<CallsiteRow> {
    let span_name_key = string_index(profile, "span_name");

    // For WithSpan, look up the target string index; bail early if the name
    // isn't in the string table.
    let target_idx: Option<i64> = match filter {
        Filter::WithSpan(name) => match profile.string_table.iter().position(|s| s == name) {
            Some(i) => Some(i as i64),
            None => return Vec::new(),
        },
        Filter::NoSpan => None,
    };

    // Predicate: include this sample?
    let want = |sample: &proto::Sample| -> bool {
        match (filter, span_name_key, target_idx) {
            (Filter::WithSpan(_), Some(key), Some(target)) => sample
                .label
                .iter()
                .any(|l| l.key == key && l.str == target),
            (Filter::NoSpan, Some(key), _) => {
                // Include if no span_name label is attached.
                !sample.label.iter().any(|l| l.key == key)
            }
            (Filter::NoSpan, None, _) => true, // no span_name key at all -> all samples are no-span
            (Filter::WithSpan(_), _, _) => false,
        }
    };

    // Build id -> Location and id -> Function lookup tables to avoid
    // O(n*m) scans on every sample.
    let location_by_id: HashMap<u64, &proto::Location> =
        profile.location.iter().map(|l| (l.id, l)).collect();
    let function_by_id: HashMap<u64, &proto::Function> =
        profile.function.iter().map(|f| (f.id, f)).collect();

    let mut by_callsite: HashMap<String, CallsiteRow> = HashMap::new();

    for sample in &profile.sample {
        if !want(sample) {
            continue;
        }

        let count = sample.value.first().copied().unwrap_or(0).max(0) as u64;
        let bytes = sample.value.get(1).copied().unwrap_or(0).max(0) as u64;
        let avg = if count == 0 { 0 } else { bytes / count };
        let estimated = if avg < rate_bytes {
            count.saturating_mul(rate_bytes)
        } else {
            bytes
        };

        let callsite_label = format_leaf_callsite(sample, profile, &location_by_id, &function_by_id);

        let row = by_callsite.entry(callsite_label.clone()).or_insert(CallsiteRow {
            label: callsite_label,
            samples: 0,
            bytes_total: 0,
            estimated_bytes: 0,
        });
        row.samples = row.samples.saturating_add(count);
        row.bytes_total = row.bytes_total.saturating_add(bytes);
        row.estimated_bytes = row.estimated_bytes.saturating_add(estimated);
    }

    by_callsite.into_values().collect()
}

/// Names matching any of these prefixes are stack-capture machinery (our
/// own observe/sampler path, backtrace internals, the rust allocator
/// shim) and should be skipped when picking a "leaf" callsite to show
/// the user. The first frame that isn't machinery is the real one.
/// Substrings (not just prefixes) that mark stack-capture / allocator
/// shim frames. We want to skip these and stop on the first frame that's
/// real user / framework code.
///
/// Rust 1.95+ mangles symbols as `<crate>[<build_hash>]::<path>` so we
/// match both the old (`alloc::raw_vec::...`) and new (`]::raw_vec::...`)
/// forms.
const MACHINERY_NEEDLES: &[&str] = &[
    "backtrace::",
    "culpert::sampler",
    "culpert::allocator",
    "<culpert::allocator::TrackingAllocator",
    "__rust_alloc",
    "__rust_realloc",
    "__rust_alloc_zeroed",
    "__rustc[",
    // Old-form (no build-hash bracket).
    "alloc::alloc::alloc",
    "alloc::alloc::Global",
    "alloc::alloc::realloc",
    "alloc::raw_vec::",
    // New-form (post-bracket).
    "]::alloc::alloc",
    "]::alloc::Global",
    "]::alloc::realloc",
    "]::raw_vec::",
];

fn is_machinery(name: &str) -> bool {
    MACHINERY_NEEDLES.iter().any(|n| name.contains(n))
}

fn format_leaf_callsite(
    sample: &proto::Sample,
    profile: &proto::Profile,
    location_by_id: &HashMap<u64, &proto::Location>,
    function_by_id: &HashMap<u64, &proto::Function>,
) -> String {
    // Walk leaf-to-root, skipping capture-machinery frames, and stop on
    // the first frame that looks like real user / framework code.
    for &loc_id in &sample.location_id {
        let Some(location) = location_by_id.get(&loc_id) else {
            continue;
        };
        let Some(line) = location.line.first() else {
            continue;
        };
        let Some(function) = function_by_id.get(&line.function_id) else {
            continue;
        };

        let name = profile
            .string_table
            .get(function.name as usize)
            .map(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("<anon>");

        if is_machinery(name) {
            continue;
        }

        let file = profile
            .string_table
            .get(function.filename as usize)
            .map(|s| s.as_str())
            .filter(|s| !s.is_empty());

        return match (file, line.line) {
            (Some(file), 0) => format!("{name}  ({file})"),
            (Some(file), n) => format!("{name}  ({file}:{n})"),
            (None, _) => name.to_string(),
        };
    }
    "<all-machinery stack>".to_string()
}

fn print_callsite_table(rows: &[CallsiteRow], total_bytes: u64, total_estimated: u64, top: usize) {
    let label_w = rows
        .iter()
        .take(top)
        .map(|r| r.label.len())
        .max()
        .unwrap_or(20)
        .min(80)
        .max(20);

    println!(
        "  {:<label_w$}  {:>10}  {:>14}  {:>7}  {:>14}  {:>7}",
        "callsite", "samples", "raw_bytes", "raw %", "est_bytes", "est %",
        label_w = label_w
    );
    println!("  {:-<label_w$}  {:->10}  {:->14}  {:->7}  {:->14}  {:->7}", "", "", "", "", "", "", label_w = label_w);

    for row in rows.iter().take(top) {
        let truncated = if row.label.len() > label_w {
            format!("{}…", &row.label[..label_w.saturating_sub(1)])
        } else {
            row.label.clone()
        };
        let raw_pct = pct(row.bytes_total, total_bytes);
        let est_pct = pct(row.estimated_bytes, total_estimated);
        println!(
            "  {:<label_w$}  {:>10}  {:>14}  {:>6.2}%  {:>14}  {:>6.2}%",
            truncated,
            row.samples,
            format_bytes(row.bytes_total),
            raw_pct,
            format_bytes(row.estimated_bytes),
            est_pct,
            label_w = label_w,
        );
    }

    if rows.len() > top {
        println!("  ... ({} more rows hidden)", rows.len() - top);
    }
}

// ---- helpers -----------------------------------------------------------

fn string_index(profile: &proto::Profile, needle: &str) -> Option<i64> {
    profile
        .string_table
        .iter()
        .position(|s| s == needle)
        .map(|i| i as i64)
}

fn pct(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        100.0 * part as f64 / whole as f64
    }
}

fn format_bytes(b: u64) -> String {
    const K: u64 = 1024;
    const M: u64 = 1024 * K;
    const G: u64 = 1024 * M;

    if b >= G {
        format!("{:.2} GB", b as f64 / G as f64)
    } else if b >= M {
        format!("{:.2} MB", b as f64 / M as f64)
    } else if b >= K {
        format!("{:.2} KB", b as f64 / K as f64)
    } else {
        format!("{b} B")
    }
}
