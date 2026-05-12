//! culpert-cli — `culpert report` and `culpert diff`.
//!
//! `report` reads one culpert pprof profile and prints a human-readable
//! summary. Four views:
//!
//! - **tree** (default): hierarchical breakdown using `span_parent_id`
//!   labels emitted by the foundations adapter
//! - **flat** (`--flat`): one row per `span_name`, sorted by bytes
//! - **callsites in span** (`--span <name>`): top callsites within that span
//! - **callsites with no span** (`--no-span`): top callsites in samples
//!   taken outside any foundations span
//!
//! `diff` compares two profiles by `span_name`, computes per-span
//! regressions / improvements over a configurable threshold, and renders
//! a text or markdown table. The markdown form is intended for PR
//! comments (a GitHub Action can write it straight into
//! `$GITHUB_STEP_SUMMARY`).

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
        #[arg(long, value_name = "NAME", conflicts_with_all = ["no_span", "flat"])]
        span: Option<String>,

        /// Filter to samples that have NO span_name label (i.e. allocations
        /// taken outside any foundations span — tokio runtime, framework
        /// internals, uninstrumented code paths).
        #[arg(long, conflicts_with = "flat")]
        no_span: bool,

        /// Use the flat top-spans table instead of the default hierarchical
        /// tree. Useful when you don't care about parent-child relationships
        /// and just want to see the biggest spans sorted by bytes.
        #[arg(long)]
        flat: bool,

        /// Limit number of rows in the output.
        #[arg(long, default_value = "20")]
        top: usize,
    },

    /// Compare two culpert profiles by span and report regressions /
    /// improvements over thresholds. Intended for CI / PR-comment workflows.
    Diff {
        /// "Before" profile.
        before: PathBuf,
        /// "After" profile.
        after: PathBuf,

        /// Maximum rows to show in each (regressions / improvements) section.
        #[arg(long, default_value = "20")]
        top: usize,

        /// Suppress changes whose absolute delta is smaller than this many bytes.
        /// Combined with --threshold-pct via AND: both gates must pass.
        #[arg(long, default_value = "4096")]
        threshold_bytes: u64,

        /// Suppress changes whose absolute relative delta is smaller than this percent.
        /// Combined with --threshold-bytes via AND: both gates must pass.
        #[arg(long, default_value = "5.0")]
        threshold_pct: f64,

        /// Output format. `text` is human-readable; `markdown` is designed for
        /// PR comments (e.g. piped into `$GITHUB_STEP_SUMMARY` from a CI step).
        #[arg(long, value_enum, default_value = "text")]
        format: DiffFormat,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum DiffFormat {
    Text,
    Markdown,
}

fn main() {
    let cli = Cli::parse();
    let res = match cli.cmd {
        Cmd::Report {
            file,
            span,
            no_span,
            flat,
            top,
        } => {
            if let Some(name) = span {
                run_callsites(&file, Filter::WithSpan(name), top)
            } else if no_span {
                run_callsites(&file, Filter::NoSpan, top)
            } else if flat {
                run_top_spans(&file, top)
            } else {
                run_tree(&file, top)
            }
        }
        Cmd::Diff {
            before,
            after,
            top,
            threshold_bytes,
            threshold_pct,
            format,
        } => run_diff(&before, &after, top, threshold_bytes, threshold_pct, format),
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

// ---- hierarchical tree report (default view) ---------------------------

struct TreeNode {
    name: String,
    samples: u64,
    bytes_total: u64,
    children: Vec<TreeNode>,
}

#[derive(Default)]
struct Acc {
    samples: u64,
    bytes_total: u64,
}

fn run_tree(path: &PathBuf, top: usize) -> Result<(), Box<dyn std::error::Error>> {
    let profile = load(path)?;
    let rate_bytes = profile.period.max(1) as u64;
    let roots = build_tree(&profile);

    let total_samples: u64 = roots.iter().map(subtree_samples).sum();
    let total_bytes: u64 = roots.iter().map(subtree_bytes).sum();

    println!(
        "Hierarchical span report ({} samples, sample rate {}/alloc):",
        total_samples,
        format_bytes(rate_bytes)
    );
    println!(
        "  Tree shows span_name groupings under their parents (from\n  \
           `span_parent_id` labels emitted by whichever SpanContext was\n  \
           installed). `bytes` is the Bernstein-corrected, unbiased\n  \
           estimate of allocated bytes (see CHANGELOG: geometric sampling).\n  \
           Use --flat for a simple sorted-by-bytes table without hierarchy."
    );
    println!();

    for (i, root) in roots.iter().enumerate() {
        render_node(root, "", i + 1 == roots.len(), 0, top, total_bytes);
    }
    Ok(())
}

fn subtree_bytes(node: &TreeNode) -> u64 {
    node.bytes_total
        .saturating_add(node.children.iter().map(subtree_bytes).sum())
}

fn subtree_samples(node: &TreeNode) -> u64 {
    node.samples
        .saturating_add(node.children.iter().map(subtree_samples).sum())
}

fn build_tree(profile: &proto::Profile) -> Vec<TreeNode> {
    let span_id_key = string_index(profile, "span_id");
    let span_name_key = string_index(profile, "span_name");
    let span_parent_id_key = string_index(profile, "span_parent_id");

    // span_id -> first-seen span_name, used to resolve parent_id labels to
    // parent names. We rely on every sample tagged with a span_id also being
    // tagged with the matching span_name (the encoder always emits them as
    // a pair).
    let mut id_to_name: HashMap<i64, String> = HashMap::new();
    for sample in &profile.sample {
        let id = span_id_key.and_then(|k| label_num(sample, k));
        let name = span_name_key.and_then(|k| label_str(sample, k, profile));
        if let (Some(id), Some(name)) = (id, name) {
            id_to_name.entry(id).or_insert_with(|| name.to_string());
        }
    }

    // Aggregate by (parent_name, name) — a span called from two different
    // parents shows up as two nodes, once under each.
    let mut by_pair: HashMap<(Option<String>, String), Acc> = HashMap::new();
    for sample in &profile.sample {
        let name = span_name_key
            .and_then(|k| label_str(sample, k, profile))
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(no span)".to_string());
        let parent_name = span_parent_id_key
            .and_then(|k| label_num(sample, k))
            .and_then(|pid| id_to_name.get(&pid).cloned());

        let count = sample.value.first().copied().unwrap_or(0).max(0) as u64;
        // `bytes` arrives Bernstein-corrected from the aggregator under
        // geometric sampling (see culpert::aggregator). No further
        // correction needed in the CLI — it's already the unbiased
        // estimate of bytes allocated for this `(span, callsite)`.
        let bytes = sample.value.get(1).copied().unwrap_or(0).max(0) as u64;

        let acc = by_pair.entry((parent_name, name)).or_default();
        acc.samples = acc.samples.saturating_add(count);
        acc.bytes_total = acc.bytes_total.saturating_add(bytes);
    }

    // Index by parent_name -> children
    let mut by_parent: HashMap<Option<String>, Vec<(String, Acc)>> = HashMap::new();
    for ((parent_name, name), acc) in by_pair {
        by_parent.entry(parent_name).or_default().push((name, acc));
    }

    build_subtree(None, &by_parent)
}

fn build_subtree(
    parent_name: Option<&str>,
    by_parent: &HashMap<Option<String>, Vec<(String, Acc)>>,
) -> Vec<TreeNode> {
    let key = parent_name.map(|s| s.to_string());
    let Some(children) = by_parent.get(&key) else {
        return Vec::new();
    };
    let mut nodes: Vec<TreeNode> = children
        .iter()
        .map(|(name, acc)| TreeNode {
            name: name.clone(),
            samples: acc.samples,
            bytes_total: acc.bytes_total,
            children: build_subtree(Some(name), by_parent),
        })
        .collect();
    // Sort by *subtree* total so the visual ordering matches dominance.
    nodes.sort_by_key(|n| std::cmp::Reverse(subtree_bytes(n)));
    nodes
}

fn render_node(
    node: &TreeNode,
    prefix: &str,
    is_last: bool,
    depth: usize,
    top: usize,
    total_bytes: u64,
) {
    let connector = if depth == 0 {
        ""
    } else if is_last {
        "└─ "
    } else {
        "├─ "
    };
    let subtree = subtree_bytes(node);
    let pct = if total_bytes == 0 {
        0.0
    } else {
        100.0 * subtree as f64 / total_bytes as f64
    };
    let self_only = node.bytes_total;
    println!(
        "{prefix}{connector}{:<40}  {:>12}  {:>6.2}%  (self {})",
        node.name,
        format_bytes(subtree),
        pct,
        format_bytes(self_only),
    );

    let new_prefix = if depth == 0 {
        String::new()
    } else {
        format!("{prefix}{}", if is_last { "   " } else { "│  " })
    };

    let visible: Vec<&TreeNode> = node.children.iter().take(top).collect();
    for (i, child) in visible.iter().enumerate() {
        render_node(
            child,
            &new_prefix,
            i + 1 == visible.len(),
            depth + 1,
            top,
            total_bytes,
        );
    }
    if node.children.len() > top {
        println!(
            "{new_prefix}... ({} more children hidden)",
            node.children.len() - top
        );
    }
}

fn label_num(sample: &proto::Sample, key: i64) -> Option<i64> {
    sample.label.iter().find(|l| l.key == key).map(|l| l.num)
}

fn label_str<'a>(
    sample: &'a proto::Sample,
    key: i64,
    profile: &'a proto::Profile,
) -> Option<&'a str> {
    sample
        .label
        .iter()
        .find(|l| l.key == key)
        .and_then(|l| profile.string_table.get(l.str as usize).map(String::as_str))
}

// ---- top spans report --------------------------------------------------

struct SpanRow {
    name: String,
    samples: u64,
    /// Bernstein-corrected, unbiased estimate of total bytes allocated under
    /// this span (sum of per-sample weights, applied in the aggregator).
    bytes_total: u64,
}

fn run_top_spans(path: &PathBuf, top: usize) -> Result<(), Box<dyn std::error::Error>> {
    let profile = load(path)?;
    let rate_bytes = profile.period.max(1) as u64;
    let mut rows = aggregate_by_span(&profile);

    rows.sort_by_key(|r| std::cmp::Reverse(r.bytes_total));

    let total_bytes: u64 = rows.iter().map(|r| r.bytes_total).sum();
    let total_samples: u64 = rows.iter().map(|r| r.samples).sum();

    println!(
        "Top spans by allocation ({} samples, sample rate {}/alloc):",
        total_samples,
        format_bytes(rate_bytes)
    );
    println!(
        "  `bytes` is the Bernstein-corrected, unbiased estimate of total bytes\n  \
           allocated under each span (see CHANGELOG: geometric sampling)."
    );
    println!();
    print_span_table(&rows, total_bytes, top);
    Ok(())
}

fn aggregate_by_span(profile: &proto::Profile) -> Vec<SpanRow> {
    let span_name_key = string_index(profile, "span_name");

    let mut by_name: HashMap<String, SpanRow> = HashMap::new();
    for sample in &profile.sample {
        let name = span_name_key
            .and_then(|k| sample.label.iter().find(|l| l.key == k))
            .and_then(|label| profile.string_table.get(label.str as usize))
            .cloned()
            .unwrap_or_else(|| "(no span)".to_string());

        let count = sample.value.first().copied().unwrap_or(0).max(0) as u64;
        // Bernstein-corrected bytes arrive in sample.value[1] from the
        // aggregator under geometric sampling. No further correction needed.
        let bytes = sample.value.get(1).copied().unwrap_or(0).max(0) as u64;

        let row = by_name.entry(name.clone()).or_insert(SpanRow {
            name,
            samples: 0,
            bytes_total: 0,
        });
        row.samples = row.samples.saturating_add(count);
        row.bytes_total = row.bytes_total.saturating_add(bytes);
    }

    by_name.into_values().collect()
}

fn print_span_table(rows: &[SpanRow], total_bytes: u64, top: usize) {
    let name_w = rows
        .iter()
        .take(top)
        .map(|r| r.name.len())
        .max()
        .unwrap_or(10)
        .max(10);

    println!(
        "  {:<name_w$}  {:>10}  {:>14}  {:>7}",
        "span", "samples", "bytes", "bytes %",
        name_w = name_w
    );
    println!("  {:-<name_w$}  {:->10}  {:->14}  {:->7}", "", "", "", "", name_w = name_w);

    for row in rows.iter().take(top) {
        let bytes_pct = pct(row.bytes_total, total_bytes);
        println!(
            "  {:<name_w$}  {:>10}  {:>14}  {:>6.2}%",
            row.name,
            row.samples,
            format_bytes(row.bytes_total),
            bytes_pct,
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
    /// Bernstein-corrected, unbiased estimate of total bytes allocated at
    /// this callsite. See [`SpanRow::bytes_total`].
    bytes_total: u64,
}

fn run_callsites(
    path: &PathBuf,
    filter: Filter,
    top: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let profile = load(path)?;
    let rate_bytes = profile.period.max(1) as u64;
    let mut rows = aggregate_callsites(&profile, &filter);

    rows.sort_by_key(|r| std::cmp::Reverse(r.bytes_total));

    let total_bytes: u64 = rows.iter().map(|r| r.bytes_total).sum();

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
    print_callsite_table(&rows, total_bytes, top);
    Ok(())
}

fn aggregate_callsites(profile: &proto::Profile, filter: &Filter) -> Vec<CallsiteRow> {
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
        // Bernstein-corrected bytes arrive in sample.value[1] from the
        // aggregator under geometric sampling. No further correction
        // needed here.
        let bytes = sample.value.get(1).copied().unwrap_or(0).max(0) as u64;

        let callsite_label = format_leaf_callsite(sample, profile, &location_by_id, &function_by_id);

        let row = by_callsite.entry(callsite_label.clone()).or_insert(CallsiteRow {
            label: callsite_label,
            samples: 0,
            bytes_total: 0,
        });
        row.samples = row.samples.saturating_add(count);
        row.bytes_total = row.bytes_total.saturating_add(bytes);
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
    "culpert::stack_capture",
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

fn print_callsite_table(rows: &[CallsiteRow], total_bytes: u64, top: usize) {
    let label_w = rows
        .iter()
        .take(top)
        .map(|r| r.label.len())
        .max()
        .unwrap_or(20)
        .clamp(20, 80);

    println!(
        "  {:<label_w$}  {:>10}  {:>14}  {:>7}",
        "callsite", "samples", "bytes", "bytes %",
        label_w = label_w
    );
    println!("  {:-<label_w$}  {:->10}  {:->14}  {:->7}", "", "", "", "", label_w = label_w);

    for row in rows.iter().take(top) {
        let truncated = if row.label.len() > label_w {
            format!("{}…", &row.label[..label_w.saturating_sub(1)])
        } else {
            row.label.clone()
        };
        let bytes_pct = pct(row.bytes_total, total_bytes);
        println!(
            "  {:<label_w$}  {:>10}  {:>14}  {:>6.2}%",
            truncated,
            row.samples,
            format_bytes(row.bytes_total),
            bytes_pct,
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

fn format_signed_bytes(delta: i64) -> String {
    if delta >= 0 {
        format!("+{}", format_bytes(delta as u64))
    } else {
        format!("-{}", format_bytes(delta.unsigned_abs()))
    }
}

// ---- diff --------------------------------------------------------------

/// Per-span delta entry for the diff report.
struct DiffRow {
    name: String,
    before: u64,
    after: u64,
    delta: i64,
    /// `None` means the span only appeared on one side and a percentage is
    /// not meaningful (NEW or GONE). `Some(p)` is signed: positive = grew,
    /// negative = shrank.
    pct: Option<f64>,
    kind: DiffKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DiffKind {
    /// Span got bigger by both gates.
    Regression,
    /// Span got smaller by both gates.
    Improvement,
    /// Span appeared in `after` but not in `before`.
    New,
    /// Span was in `before` but is absent from `after`.
    Gone,
    /// Change is below at least one threshold.
    Quiet,
}

fn run_diff(
    before_path: &PathBuf,
    after_path: &PathBuf,
    top: usize,
    threshold_bytes: u64,
    threshold_pct: f64,
    format: DiffFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let before_profile = load(before_path)?;
    let after_profile = load(after_path)?;

    if before_profile.period != after_profile.period {
        return Err(format!(
            "sample rates differ: before={} bytes/sample, after={} bytes/sample. \
             Profiles taken at different rates aren't directly comparable; re-take \
             both with the same Config::rate_bytes.",
            before_profile.period, after_profile.period
        )
        .into());
    }
    // Bytes arrive Bernstein-corrected under geometric sampling, so the
    // diff doesn't need the rate at all — but we still error out above
    // if the two profiles' rates differ, since correction-then-compare
    // across different rates would mix incompatible distributions.

    let before_rows = aggregate_by_span(&before_profile);
    let after_rows = aggregate_by_span(&after_profile);

    let before_total: u64 = before_rows.iter().map(|r| r.bytes_total).sum();
    let after_total: u64 = after_rows.iter().map(|r| r.bytes_total).sum();

    use std::collections::HashMap;
    let before_by_name: HashMap<String, u64> = before_rows
        .into_iter()
        .map(|r| (r.name, r.bytes_total))
        .collect();
    let after_by_name: HashMap<String, u64> = after_rows
        .into_iter()
        .map(|r| (r.name, r.bytes_total))
        .collect();

    // Union of span names present in either profile.
    let mut all_names: Vec<&String> = before_by_name
        .keys()
        .chain(after_by_name.keys())
        .collect();
    all_names.sort();
    all_names.dedup();

    let mut diffs: Vec<DiffRow> = all_names
        .into_iter()
        .map(|name| {
            let before = before_by_name.get(name).copied().unwrap_or(0);
            let after = after_by_name.get(name).copied().unwrap_or(0);
            let delta = after as i64 - before as i64;
            let pct = if before == 0 {
                None
            } else {
                Some(100.0 * delta as f64 / before as f64)
            };
            let kind = classify(before, after, delta, pct, threshold_bytes, threshold_pct);
            DiffRow {
                name: name.clone(),
                before,
                after,
                delta,
                pct,
                kind,
            }
        })
        .collect();

    // Sort all rows by |delta| descending so the most impactful changes
    // surface first in each section.
    diffs.sort_by_key(|d| std::cmp::Reverse(d.delta.unsigned_abs()));

    let regressions: Vec<&DiffRow> = diffs
        .iter()
        .filter(|d| matches!(d.kind, DiffKind::Regression | DiffKind::New))
        .take(top)
        .collect();
    let improvements: Vec<&DiffRow> = diffs
        .iter()
        .filter(|d| matches!(d.kind, DiffKind::Improvement | DiffKind::Gone))
        .take(top)
        .collect();
    let quiet_count = diffs
        .iter()
        .filter(|d| matches!(d.kind, DiffKind::Quiet))
        .count();

    let summary = DiffSummary {
        before_path,
        after_path,
        before_total,
        after_total,
        rate_bytes: before_profile.period.max(1) as u64,
        threshold_bytes,
        threshold_pct,
        quiet_count,
    };

    match format {
        DiffFormat::Text => render_diff_text(&summary, &regressions, &improvements),
        DiffFormat::Markdown => render_diff_markdown(&summary, &regressions, &improvements),
    }
    Ok(())
}

fn classify(
    before: u64,
    after: u64,
    delta: i64,
    pct: Option<f64>,
    threshold_bytes: u64,
    threshold_pct: f64,
) -> DiffKind {
    if before == 0 && after > 0 {
        return if after >= threshold_bytes {
            DiffKind::New
        } else {
            DiffKind::Quiet
        };
    }
    if before > 0 && after == 0 {
        return if before >= threshold_bytes {
            DiffKind::Gone
        } else {
            DiffKind::Quiet
        };
    }
    let abs_bytes = delta.unsigned_abs();
    let passes_bytes = abs_bytes >= threshold_bytes;
    let passes_pct = pct.is_some_and(|p| p.abs() >= threshold_pct);
    if !(passes_bytes && passes_pct) {
        return DiffKind::Quiet;
    }
    if delta > 0 {
        DiffKind::Regression
    } else {
        DiffKind::Improvement
    }
}

struct DiffSummary<'a> {
    before_path: &'a PathBuf,
    after_path: &'a PathBuf,
    before_total: u64,
    after_total: u64,
    rate_bytes: u64,
    threshold_bytes: u64,
    threshold_pct: f64,
    quiet_count: usize,
}

fn render_diff_text(summary: &DiffSummary, regressions: &[&DiffRow], improvements: &[&DiffRow]) {
    let total_delta = summary.after_total as i64 - summary.before_total as i64;
    let total_pct = if summary.before_total == 0 {
        0.0
    } else {
        100.0 * total_delta as f64 / summary.before_total as f64
    };

    println!("Allocation diff:");
    println!("  before:  {}", summary.before_path.display());
    println!("           total {} (estimated)", format_bytes(summary.before_total));
    println!("  after:   {}", summary.after_path.display());
    println!(
        "           total {} (estimated)  Δ = {}  ({:+.2}%)",
        format_bytes(summary.after_total),
        format_signed_bytes(total_delta),
        total_pct
    );
    println!(
        "  rate:    {}/alloc",
        format_bytes(summary.rate_bytes)
    );
    println!(
        "  filter:  show changes ≥ {} AND ≥ {:.2}%",
        format_bytes(summary.threshold_bytes),
        summary.threshold_pct
    );
    println!();

    print_diff_section_text("Regressions", regressions);
    println!();
    print_diff_section_text("Improvements", improvements);

    if summary.quiet_count > 0 {
        println!();
        println!(
            "{} span(s) suppressed by thresholds.",
            summary.quiet_count
        );
    }
}

fn print_diff_section_text(title: &str, rows: &[&DiffRow]) {
    if rows.is_empty() {
        println!("{title}: none");
        return;
    }
    let name_w = rows.iter().map(|r| r.name.len()).max().unwrap_or(20).max(20);
    println!("{title}:");
    println!(
        "  {:<name_w$}  {:>12}  {:>12}  {:>12}  {:>8}",
        "span", "before", "after", "Δ", "Δ%",
        name_w = name_w
    );
    println!(
        "  {:-<name_w$}  {:->12}  {:->12}  {:->12}  {:->8}",
        "", "", "", "", "",
        name_w = name_w
    );
    for d in rows {
        let pct_cell = match (d.kind, d.pct) {
            (DiffKind::New, _) => "NEW".to_string(),
            (DiffKind::Gone, _) => "GONE".to_string(),
            (_, Some(p)) => format!("{p:+.2}%"),
            (_, None) => "—".to_string(),
        };
        println!(
            "  {:<name_w$}  {:>12}  {:>12}  {:>12}  {:>8}",
            d.name,
            format_bytes(d.before),
            format_bytes(d.after),
            format_signed_bytes(d.delta),
            pct_cell,
            name_w = name_w
        );
    }
}

fn render_diff_markdown(
    summary: &DiffSummary,
    regressions: &[&DiffRow],
    improvements: &[&DiffRow],
) {
    let total_delta = summary.after_total as i64 - summary.before_total as i64;
    let total_pct = if summary.before_total == 0 {
        0.0
    } else {
        100.0 * total_delta as f64 / summary.before_total as f64
    };

    println!("### culpert: allocation diff");
    println!();
    println!(
        "- **before:** `{}` — total {} (estimated)",
        summary.before_path.display(),
        format_bytes(summary.before_total)
    );
    println!(
        "- **after:**  `{}` — total {} (estimated)",
        summary.after_path.display(),
        format_bytes(summary.after_total)
    );
    println!(
        "- **net Δ:** {} ({:+.2}%)",
        format_signed_bytes(total_delta),
        total_pct
    );
    println!(
        "- **sample rate:** {}/alloc",
        format_bytes(summary.rate_bytes)
    );
    println!(
        "- **filter:** show changes ≥ {} AND ≥ {:.2}%",
        format_bytes(summary.threshold_bytes),
        summary.threshold_pct
    );
    println!();

    print_diff_section_markdown("Regressions", regressions);
    println!();
    print_diff_section_markdown("Improvements", improvements);

    if summary.quiet_count > 0 {
        println!();
        println!(
            "<sub>{} span(s) suppressed by thresholds.</sub>",
            summary.quiet_count
        );
    }
}

fn print_diff_section_markdown(title: &str, rows: &[&DiffRow]) {
    if rows.is_empty() {
        println!("#### {title}");
        println!();
        println!("_None._");
        return;
    }
    println!("#### {title}");
    println!();
    println!("| Span | Before | After | Δ | Δ% |");
    println!("|------|-------:|------:|--:|---:|");
    for d in rows {
        let pct_cell = match (d.kind, d.pct) {
            (DiffKind::New, _) => "_new_".to_string(),
            (DiffKind::Gone, _) => "_gone_".to_string(),
            (_, Some(p)) => format!("{p:+.2}%"),
            (_, None) => "—".to_string(),
        };
        println!(
            "| `{}` | {} | {} | {} | {} |",
            d.name,
            format_bytes(d.before),
            format_bytes(d.after),
            format_signed_bytes(d.delta),
            pct_cell,
        );
    }
}
