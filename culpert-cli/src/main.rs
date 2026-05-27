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

mod archive;

use clap::{Parser, Subcommand};
use culpert::pprof::{self, is_machinery, proto};
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

        /// Output format. `text` is human-readable; `markdown` is designed
        /// for PR comments (e.g. piped into `$GITHUB_STEP_SUMMARY` from a
        /// CI step); `json` is structured output for further processing
        /// (e.g. uploading to a profile store, machine-driven gating).
        #[arg(long, value_enum, default_value = "text")]
        format: DiffFormat,

        /// Exit with code 0 even when regressions are found. By default
        /// `culpert diff` exits 1 if any row classifies as a regression
        /// (or NEW) after thresholds, so CI can gate on the result. Pass
        /// this flag when you just want the report without the fail signal
        /// (e.g. local debugging, or a CI step that always succeeds and
        /// posts a comment).
        #[arg(long)]
        no_fail: bool,

        /// Show regressions nested under their parent span, using the same
        /// tree layout as `culpert report`. Applies to `--format text` and
        /// `--format markdown`; `--format json` is always flat.
        #[arg(long)]
        tree: bool,
    },

    /// Generate a flamegraph from a culpert profile.
    ///
    /// By default emits folded-stack text (semicolon-separated, one line per
    /// unique stack) which any standard flamegraph tool can consume. Pass
    /// `--format svg` to render the SVG directly via the inferno crate
    /// without any external tooling.
    ///
    /// Span names are prepended as synthetic root frames so the flamegraph
    /// groups allocations by span at the top level. Pass `--no-annotate-spans`
    /// for raw call stacks without span grouping.
    Flamegraph {
        /// Path to the .pb.gz profile file.
        file: PathBuf,

        /// Output format.
        #[arg(long, value_enum, default_value = "folded")]
        format: FlamegraphFormat,

        /// What to use as the per-sample weight.
        #[arg(long, value_enum, default_value = "bytes")]
        weight: FlamegraphWeight,

        /// Only include samples from the named span.
        #[arg(long, value_name = "NAME")]
        span: Option<String>,

        /// Prepend the span name as a synthetic root frame so allocations
        /// are grouped by span at the top of the flamegraph. Enabled by
        /// default; pass --no-annotate-spans for raw call stacks.
        #[arg(long, default_value = "true", action = clap::ArgAction::Set)]
        annotate_spans: bool,

        /// Write output to this file instead of stdout.
        #[arg(long, short = 'o', value_name = "FILE")]
        output: Option<PathBuf>,
    },

    /// Print embedded metadata + a short summary of a culpert profile.
    /// Useful as a first look at a captured profile (`culpert info
    /// foo.pb.gz`) and in CI logs to confirm the right artefact was
    /// uploaded ("for commit abc123, took at ...").
    Info {
        /// Path to the .pb.gz profile file.
        file: PathBuf,
    },

    /// Upload a profile to a culpert-archive instance, keyed by commit
    /// SHA. Designed for CI: capture a profile right after a build,
    /// upload it under `$GITHUB_SHA` so the next run on this branch
    /// has a baseline to diff against.
    ///
    /// Endpoint and token can come from env (`CULPERT_ARCHIVE` /
    /// `CULPERT_TOKEN`) so the GitHub Actions step body is short.
    Upload {
        /// Path to the .pb.gz profile to upload.
        file: PathBuf,

        /// culpert-archive base URL, e.g. https://culpert-archive.example.workers.dev
        #[arg(long, env = "CULPERT_ARCHIVE")]
        endpoint: String,

        /// Bearer token (the `AUTH_TOKEN` secret set on the archive).
        #[arg(long, env = "CULPERT_TOKEN")]
        token: String,

        /// Project identifier. Scopes the profile to a specific project
        /// so multiple repos can share one archive deployment.
        #[arg(long, env = "CULPERT_PROJECT")]
        project: String,

        /// Commit SHA to key this upload by. Required.
        #[arg(long, value_name = "SHA", env = "GITHUB_SHA")]
        commit_sha: String,

        /// Branch the profile was captured on. Optional but strongly
        /// recommended — the archive uses it to answer
        /// `pull --latest-of <branch>` later.
        #[arg(long, value_name = "NAME", env = "GITHUB_REF_NAME")]
        branch: Option<String>,

        /// Cloudflare Access service-token Client ID. Sent as the
        /// `CF-Access-Client-Id` header when contacting an archive
        /// instance behind Cloudflare Access. Both this and
        /// `--cf-access-client-secret` must be set (or neither — the
        /// CLI errors at parse time if only one is provided).
        #[cfg(feature = "cloudflare-access")]
        #[arg(
            long,
            env = "CF_ACCESS_CLIENT_ID",
            requires = "cf_access_client_secret"
        )]
        cf_access_client_id: Option<String>,

        /// Cloudflare Access service-token Client Secret. See
        /// `--cf-access-client-id`.
        #[cfg(feature = "cloudflare-access")]
        #[arg(
            long,
            env = "CF_ACCESS_CLIENT_SECRET",
            requires = "cf_access_client_id"
        )]
        cf_access_client_secret: Option<String>,
    },

    /// Pull a profile from a culpert-archive instance, either by exact
    /// commit SHA or by "latest on branch". Writes the raw `.pb.gz`
    /// bytes to a file (`-o`) or stdout.
    ///
    /// Typical CI use:
    ///
    /// ```sh
    /// culpert pull --latest-of main -o /tmp/baseline.pb.gz --allow-missing
    /// ```
    Pull {
        /// culpert-archive base URL.
        #[arg(long, env = "CULPERT_ARCHIVE")]
        endpoint: String,

        /// Bearer token.
        #[arg(long, env = "CULPERT_TOKEN")]
        token: String,

        /// Project identifier.
        #[arg(long, env = "CULPERT_PROJECT")]
        project: String,

        /// Pull by exact commit SHA. Mutually exclusive with `--latest-of`.
        #[arg(long, value_name = "SHA", conflicts_with = "latest_of")]
        sha: Option<String>,

        /// Pull the latest profile on the named branch. Mutually
        /// exclusive with `--sha`.
        #[arg(long, value_name = "BRANCH")]
        latest_of: Option<String>,

        /// Write the body to this path. Without it, the bytes go to
        /// stdout (useful for `culpert pull ... | culpert info /dev/stdin`).
        #[arg(short = 'o', long, value_name = "PATH")]
        output: Option<PathBuf>,

        /// Treat HTTP 404 as success (writing nothing). For the common
        /// CI case where the very first run on `main` has no baseline
        /// yet — the diff step can then `[ -f baseline.pb.gz ] && ...`
        /// without failing the build.
        #[arg(long)]
        allow_missing: bool,

        /// Cloudflare Access service-token Client ID. See `culpert
        /// upload --help` for the full explanation; same shape here.
        #[cfg(feature = "cloudflare-access")]
        #[arg(
            long,
            env = "CF_ACCESS_CLIENT_ID",
            requires = "cf_access_client_secret"
        )]
        cf_access_client_id: Option<String>,

        /// Cloudflare Access service-token Client Secret. See
        /// `--cf-access-client-id`.
        #[cfg(feature = "cloudflare-access")]
        #[arg(
            long,
            env = "CF_ACCESS_CLIENT_SECRET",
            requires = "cf_access_client_id"
        )]
        cf_access_client_secret: Option<String>,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum DiffFormat {
    Text,
    Markdown,
    Json,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum FlamegraphFormat {
    /// Folded-stacks text: one line per unique stack, frames separated by
    /// semicolons, weight appended. Compatible with flamegraph.pl, Speedscope,
    /// and `inferno-flamegraph`.
    Folded,
    /// SVG flamegraph rendered by the inferno crate. No external tooling needed.
    Svg,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum FlamegraphWeight {
    /// Use Bernstein-corrected estimated bytes (default).
    Bytes,
    /// Use raw sample count.
    Samples,
}

fn main() {
    let cli = Cli::parse();
    // Each subcommand returns the exit code on success: 0 for plain
    // success, 1 from `diff` when regressions are found and `--no-fail`
    // isn't set. Usage / I/O errors propagate via `Err` and use 2.
    let res: Result<i32, Box<dyn std::error::Error>> = match cli.cmd {
        Cmd::Report {
            file,
            span,
            no_span,
            flat,
            top,
        } => {
            if let Some(name) = span {
                run_callsites(&file, Filter::WithSpan(name), top).map(|()| 0)
            } else if no_span {
                run_callsites(&file, Filter::NoSpan, top).map(|()| 0)
            } else if flat {
                run_top_spans(&file, top).map(|()| 0)
            } else {
                run_tree(&file, top).map(|()| 0)
            }
        }
        Cmd::Diff {
            before,
            after,
            top,
            threshold_bytes,
            threshold_pct,
            format,
            no_fail,
            tree,
        } => run_diff(
            &before,
            &after,
            top,
            threshold_bytes,
            threshold_pct,
            format,
            tree,
        )
        .map(
            |had_regressions| {
                if had_regressions && !no_fail {
                    1
                } else {
                    0
                }
            },
        ),
        Cmd::Flamegraph {
            file,
            format,
            weight,
            span,
            annotate_spans,
            output,
        } => {
            run_flamegraph(&file, format, weight, span, annotate_spans, output.as_ref()).map(|()| 0)
        }
        Cmd::Info { file } => run_info(&file).map(|()| 0),
        Cmd::Upload {
            file,
            endpoint,
            token,
            project,
            commit_sha,
            branch,
            #[cfg(feature = "cloudflare-access")]
            cf_access_client_id,
            #[cfg(feature = "cloudflare-access")]
            cf_access_client_secret,
        } => {
            let cf_access = build_cf_access(
                #[cfg(feature = "cloudflare-access")]
                cf_access_client_id,
                #[cfg(feature = "cloudflare-access")]
                cf_access_client_secret,
            );
            run_upload(
                &file,
                &endpoint,
                &token,
                &project,
                &commit_sha,
                branch.as_deref(),
                cf_access,
            )
            .map(|()| 0)
        }
        Cmd::Pull {
            endpoint,
            token,
            project,
            sha,
            latest_of,
            output,
            allow_missing,
            #[cfg(feature = "cloudflare-access")]
            cf_access_client_id,
            #[cfg(feature = "cloudflare-access")]
            cf_access_client_secret,
        } => {
            let cf_access = build_cf_access(
                #[cfg(feature = "cloudflare-access")]
                cf_access_client_id,
                #[cfg(feature = "cloudflare-access")]
                cf_access_client_secret,
            );
            run_pull(
                &endpoint,
                &token,
                &project,
                sha.as_deref(),
                latest_of.as_deref(),
                output.as_ref(),
                allow_missing,
                cf_access,
            )
        }
    };
    match res {
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
        Ok(code) => std::process::exit(code),
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
        let count = sample.value.first().copied().unwrap_or(0).max(0) as u64;
        // Bernstein-corrected bytes arrive in sample.value[1] from the
        // aggregator under geometric sampling. No further correction needed.
        let bytes = sample.value.get(1).copied().unwrap_or(0).max(0) as u64;

        // Encoder emits zero-value synthetic samples for parent spans with
        // no direct ProfileEntry of their own (so the tree builder can
        // resolve their names). Real samples always have count >= 1; skip
        // synthetics so they don't show up as 0-byte rows in the flat
        // top-spans table.
        if count == 0 && bytes == 0 {
            continue;
        }

        let name = span_name_key
            .and_then(|k| sample.label.iter().find(|l| l.key == k))
            .and_then(|label| profile.string_table.get(label.str as usize))
            .cloned()
            .unwrap_or_else(|| "(no span)".to_string());

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
        "span",
        "samples",
        "bytes",
        "bytes %",
        name_w = name_w
    );
    println!(
        "  {:-<name_w$}  {:->10}  {:->14}  {:->7}",
        "",
        "",
        "",
        "",
        name_w = name_w
    );

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
            (Filter::WithSpan(_), Some(key), Some(target)) => {
                sample.label.iter().any(|l| l.key == key && l.str == target)
            }
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

        // Skip synthetic parent-span samples (see aggregate_by_span).
        if count == 0 && bytes == 0 {
            continue;
        }

        let callsite_label =
            format_leaf_callsite(sample, profile, &location_by_id, &function_by_id);

        let row = by_callsite
            .entry(callsite_label.clone())
            .or_insert(CallsiteRow {
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
        "callsite",
        "samples",
        "bytes",
        "bytes %",
        label_w = label_w
    );
    println!(
        "  {:-<label_w$}  {:->10}  {:->14}  {:->7}",
        "",
        "",
        "",
        "",
        label_w = label_w
    );

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

/// Run the diff command. Returns `true` if any row classifies as a
/// regression (or NEW) after the threshold gates pass — the caller
/// translates that into an exit code unless `--no-fail` was set.
fn run_diff(
    before_path: &PathBuf,
    after_path: &PathBuf,
    top: usize,
    threshold_bytes: u64,
    threshold_pct: f64,
    format: DiffFormat,
    tree: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
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
    let mut all_names: Vec<&String> = before_by_name.keys().chain(after_by_name.keys()).collect();
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

    let had_regressions = diffs
        .iter()
        .any(|d| matches!(d.kind, DiffKind::Regression | DiffKind::New));

    if tree && !matches!(format, DiffFormat::Json) {
        let roots = build_diff_tree(&before_profile, &after_profile);
        match format {
            DiffFormat::Text => render_diff_tree_text(&summary, &roots, top),
            DiffFormat::Markdown => render_diff_tree_markdown(&summary, &roots, top),
            DiffFormat::Json => unreachable!(),
        }
    } else {
        match format {
            DiffFormat::Text => render_diff_text(&summary, &regressions, &improvements),
            DiffFormat::Markdown => render_diff_markdown(&summary, &regressions, &improvements),
            DiffFormat::Json => render_diff_json(
                &summary,
                &diffs,
                &before_profile,
                &after_profile,
                had_regressions,
            )?,
        }
    }
    Ok(had_regressions)
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
    println!(
        "           total {} (estimated)",
        format_bytes(summary.before_total)
    );
    println!("  after:   {}", summary.after_path.display());
    println!(
        "           total {} (estimated)  Δ = {}  ({:+.2}%)",
        format_bytes(summary.after_total),
        format_signed_bytes(total_delta),
        total_pct
    );
    println!("  rate:    {}/alloc", format_bytes(summary.rate_bytes));
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
        println!("{} span(s) suppressed by thresholds.", summary.quiet_count);
    }
}

fn print_diff_section_text(title: &str, rows: &[&DiffRow]) {
    if rows.is_empty() {
        println!("{title}: none");
        return;
    }
    let name_w = rows
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(20)
        .max(20);
    println!("{title}:");
    println!(
        "  {:<name_w$}  {:>12}  {:>12}  {:>12}  {:>8}",
        "span",
        "before",
        "after",
        "Δ",
        "Δ%",
        name_w = name_w
    );
    println!(
        "  {:-<name_w$}  {:->12}  {:->12}  {:->12}  {:->8}",
        "",
        "",
        "",
        "",
        "",
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

/// JSON output for `culpert diff --format json`. Designed for ingestion
/// by CI tooling and the planned culpert-store worker: every numeric
/// field is a plain number (not pretty-printed bytes) so the consumer
/// can do their own formatting, and the schema is flat for easy parsing.
///
/// Stability: the top-level keys (`schema_version`, `before`, `after`,
/// `rate_bytes`, `thresholds`, `rows`, `summary`) and the per-row keys
/// (`name`, `before`, `after`, `delta`, `pct`, `kind`) are part of the
/// public CLI contract — additive changes are fine, but renames are
/// breaking. `schema_version` bumps on any breaking change.
fn render_diff_json(
    summary: &DiffSummary,
    diffs: &[DiffRow],
    before_profile: &proto::Profile,
    after_profile: &proto::Profile,
    had_regressions: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use serde_json::{json, Value};

    let kind_str = |k: DiffKind| -> &'static str {
        match k {
            DiffKind::Regression => "regression",
            DiffKind::Improvement => "improvement",
            DiffKind::New => "new",
            DiffKind::Gone => "gone",
            DiffKind::Quiet => "quiet",
        }
    };

    let rows: Vec<Value> = diffs
        .iter()
        .map(|d| {
            json!({
                "name": d.name,
                "before": d.before,
                "after": d.after,
                "delta": d.delta,
                "pct": d.pct,
                "kind": kind_str(d.kind),
            })
        })
        .collect();

    let regressions = diffs
        .iter()
        .filter(|d| matches!(d.kind, DiffKind::Regression | DiffKind::New))
        .count();
    let improvements = diffs
        .iter()
        .filter(|d| matches!(d.kind, DiffKind::Improvement | DiffKind::Gone))
        .count();

    let out = json!({
        "schema_version": 1,
        "before": {
            "path": summary.before_path.display().to_string(),
            "total_bytes": summary.before_total,
            "metadata": pprof::metadata(before_profile),
        },
        "after": {
            "path": summary.after_path.display().to_string(),
            "total_bytes": summary.after_total,
            "metadata": pprof::metadata(after_profile),
        },
        "rate_bytes": summary.rate_bytes,
        "thresholds": {
            "bytes": summary.threshold_bytes,
            "pct": summary.threshold_pct,
        },
        "rows": rows,
        "summary": {
            "regressions": regressions,
            "improvements": improvements,
            "quiet": summary.quiet_count,
            "had_regressions": had_regressions,
            "total_delta_bytes":
                summary.after_total as i64 - summary.before_total as i64,
        },
    });

    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

// ---- diff tree ---------------------------------------------------------

struct DiffTreeNode {
    name: String,
    self_before: u64,
    self_after: u64,
    children: Vec<DiffTreeNode>,
}

fn subtree_diff_before(n: &DiffTreeNode) -> u64 {
    n.self_before
        .saturating_add(n.children.iter().map(subtree_diff_before).sum())
}

fn subtree_diff_after(n: &DiffTreeNode) -> u64 {
    n.self_after
        .saturating_add(n.children.iter().map(subtree_diff_after).sum())
}

/// Extract `(parent_name, child_name) → bytes` from a profile.
/// Mirrors the aggregation pass inside `build_tree` but returns raw pairs
/// so the diff builder can merge before/after without constructing two
/// independent `TreeNode` trees.
fn span_pairs(profile: &proto::Profile) -> HashMap<(Option<String>, String), u64> {
    let span_id_key = string_index(profile, "span_id");
    let span_name_key = string_index(profile, "span_name");
    let span_parent_id_key = string_index(profile, "span_parent_id");

    let mut id_to_name: HashMap<i64, String> = HashMap::new();
    for sample in &profile.sample {
        let id = span_id_key.and_then(|k| label_num(sample, k));
        let name = span_name_key.and_then(|k| label_str(sample, k, profile));
        if let (Some(id), Some(name)) = (id, name) {
            id_to_name.entry(id).or_insert_with(|| name.to_string());
        }
    }

    let mut pairs: HashMap<(Option<String>, String), u64> = HashMap::new();
    for sample in &profile.sample {
        let name = span_name_key
            .and_then(|k| label_str(sample, k, profile))
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(no span)".to_string());
        let parent_name = span_parent_id_key
            .and_then(|k| label_num(sample, k))
            .and_then(|pid| id_to_name.get(&pid).cloned());
        let bytes = sample.value.get(1).copied().unwrap_or(0).max(0) as u64;
        *pairs.entry((parent_name, name)).or_default() += bytes;
    }
    pairs
}

fn build_diff_tree(
    before_profile: &proto::Profile,
    after_profile: &proto::Profile,
) -> Vec<DiffTreeNode> {
    let before_pairs = span_pairs(before_profile);
    let after_pairs = span_pairs(after_profile);

    // Union all (parent, name) pairs from both profiles.
    let mut combined: HashMap<(Option<String>, String), (u64, u64)> = HashMap::new();
    for (key, &bytes) in &before_pairs {
        combined.entry(key.clone()).or_default().0 = bytes;
    }
    for (key, &bytes) in &after_pairs {
        combined.entry(key.clone()).or_default().1 = bytes;
    }

    let mut by_parent: HashMap<Option<String>, Vec<(String, u64, u64)>> = HashMap::new();
    for ((parent, name), (before, after)) in combined {
        by_parent
            .entry(parent)
            .or_default()
            .push((name, before, after));
    }

    build_diff_subtree(None, &by_parent)
}

fn build_diff_subtree(
    parent: Option<&str>,
    by_parent: &HashMap<Option<String>, Vec<(String, u64, u64)>>,
) -> Vec<DiffTreeNode> {
    let key = parent.map(|s| s.to_string());
    let Some(children) = by_parent.get(&key) else {
        return Vec::new();
    };
    let mut nodes: Vec<DiffTreeNode> = children
        .iter()
        .map(|(name, before, after)| DiffTreeNode {
            name: name.clone(),
            self_before: *before,
            self_after: *after,
            children: build_diff_subtree(Some(name), by_parent),
        })
        .collect();
    // Sort by |subtree delta| descending — largest changes surface first.
    nodes.sort_by_key(|n| {
        let d = subtree_diff_after(n) as i64 - subtree_diff_before(n) as i64;
        std::cmp::Reverse(d.unsigned_abs())
    });
    nodes
}

fn render_diff_node(node: &DiffTreeNode, prefix: &str, is_last: bool, depth: usize, top: usize) {
    let connector = if depth == 0 {
        ""
    } else if is_last {
        "└─ "
    } else {
        "├─ "
    };
    let before = subtree_diff_before(node);
    let after = subtree_diff_after(node);
    let delta = after as i64 - before as i64;
    let pct = if before == 0 {
        None
    } else {
        Some(100.0 * delta as f64 / before as f64)
    };
    let pct_str = match pct {
        Some(p) => format!("{p:+.2}%"),
        None if after > 0 => "NEW".to_string(),
        None => "—".to_string(),
    };
    println!(
        "{prefix}{connector}{:<40}  {:>12}  {:>12}  {:>12}  {:>9}",
        node.name,
        format_bytes(before),
        format_bytes(after),
        format_signed_bytes(delta),
        pct_str,
    );

    let new_prefix = if depth == 0 {
        String::new()
    } else {
        format!("{prefix}{}", if is_last { "   " } else { "│  " })
    };

    let visible: Vec<&DiffTreeNode> = node.children.iter().take(top).collect();
    for (i, child) in visible.iter().enumerate() {
        render_diff_node(child, &new_prefix, i + 1 == visible.len(), depth + 1, top);
    }
    if node.children.len() > top {
        println!(
            "{new_prefix}... ({} more children hidden)",
            node.children.len() - top
        );
    }
}

fn render_diff_tree_text(summary: &DiffSummary, roots: &[DiffTreeNode], top: usize) {
    let total_delta = summary.after_total as i64 - summary.before_total as i64;
    let total_pct = if summary.before_total == 0 {
        0.0
    } else {
        100.0 * total_delta as f64 / summary.before_total as f64
    };

    println!("Allocation diff (tree view):");
    println!("  before:  {}", summary.before_path.display());
    println!(
        "           total {} (estimated)",
        format_bytes(summary.before_total)
    );
    println!("  after:   {}", summary.after_path.display());
    println!(
        "           total {} (estimated)  Δ = {}  ({:+.2}%)",
        format_bytes(summary.after_total),
        format_signed_bytes(total_delta),
        total_pct
    );
    println!("  rate:    {}/alloc", format_bytes(summary.rate_bytes));
    println!();
    println!(
        "{:<40}  {:>12}  {:>12}  {:>12}  {:>9}",
        "span", "before", "after", "Δ", "Δ%"
    );
    println!(
        "{:-<40}  {:->12}  {:->12}  {:->12}  {:->9}",
        "", "", "", "", ""
    );
    for (i, root) in roots.iter().enumerate() {
        render_diff_node(root, "", i + 1 == roots.len(), 0, top);
    }
}

fn render_diff_tree_markdown(summary: &DiffSummary, roots: &[DiffTreeNode], top: usize) {
    let total_delta = summary.after_total as i64 - summary.before_total as i64;
    let total_pct = if summary.before_total == 0 {
        0.0
    } else {
        100.0 * total_delta as f64 / summary.before_total as f64
    };

    println!("### culpert: allocation diff (tree view)");
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
    println!();
    println!("```");
    println!(
        "{:<40}  {:>12}  {:>12}  {:>12}  {:>9}",
        "span", "before", "after", "Δ", "Δ%"
    );
    println!(
        "{:-<40}  {:->12}  {:->12}  {:->12}  {:->9}",
        "", "", "", "", ""
    );
    for (i, root) in roots.iter().enumerate() {
        render_diff_node(root, "", i + 1 == roots.len(), 0, top);
    }
    println!("```");
}

// ---- flamegraph --------------------------------------------------------

fn run_flamegraph(
    path: &PathBuf,
    format: FlamegraphFormat,
    weight: FlamegraphWeight,
    span_filter: Option<String>,
    annotate_spans: bool,
    output: Option<&PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let profile = load(path)?;

    let function_by_id: HashMap<u64, &proto::Function> =
        profile.function.iter().map(|f| (f.id, f)).collect();
    let location_by_id: HashMap<u64, &proto::Location> =
        profile.location.iter().map(|l| (l.id, l)).collect();
    let span_name_key = string_index(&profile, "span_name");

    // Aggregate identical folded stacks so inferno gets one line per unique
    // stack rather than one per raw sample.
    let mut stacks: HashMap<String, u64> = HashMap::new();

    for sample in &profile.sample {
        // Skip zero-value synthetic parent-span marker samples.
        let bytes = sample.value.get(1).copied().unwrap_or(0).max(0) as u64;
        let count = sample.value.first().copied().unwrap_or(0).max(0) as u64;
        if bytes == 0 && count == 0 {
            continue;
        }

        let span_name = span_name_key.and_then(|k| label_str(sample, k, &profile));

        if let Some(filter) = &span_filter {
            if span_name != Some(filter.as_str()) {
                continue;
            }
        }

        let w = match weight {
            FlamegraphWeight::Bytes => bytes,
            FlamegraphWeight::Samples => count,
        };
        if w == 0 {
            continue;
        }

        // pprof stores location_id leaf-to-root; reverse for root-to-leaf.
        let mut frames: Vec<&str> = sample
            .location_id
            .iter()
            .rev()
            .filter_map(|&id| location_by_id.get(&id))
            .filter_map(|loc| loc.line.first())
            .filter_map(|line| function_by_id.get(&line.function_id))
            .filter_map(|func| {
                profile
                    .string_table
                    .get(func.name as usize)
                    .map(String::as_str)
            })
            .filter(|name| !name.is_empty())
            .collect();

        if annotate_spans {
            if let Some(name) = span_name {
                frames.insert(0, name);
            }
        }

        if frames.is_empty() {
            continue;
        }

        *stacks.entry(frames.join(";")).or_default() += w;
    }

    // Sort for stable, reproducible output.
    let mut lines: Vec<String> = stacks
        .into_iter()
        .map(|(stack, w)| format!("{stack} {w}"))
        .collect();
    lines.sort();

    let mut out: Box<dyn std::io::Write> = match output {
        Some(p) => Box::new(std::fs::File::create(p)?),
        None => Box::new(std::io::stdout()),
    };

    match format {
        FlamegraphFormat::Folded => {
            for line in &lines {
                writeln!(out, "{line}")?;
            }
        }
        FlamegraphFormat::Svg => {
            let mut opts = inferno::flamegraph::Options::default();
            opts.title = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("culpert profile")
                .to_string();
            if matches!(weight, FlamegraphWeight::Bytes) {
                opts.count_name = "bytes".to_string();
            }
            inferno::flamegraph::from_lines(&mut opts, lines.iter().map(String::as_str), &mut out)
                .map_err(|e| format!("inferno flamegraph error: {e}"))?;
        }
    }

    Ok(())
}

// ---- info --------------------------------------------------------------

/// Print the embedded metadata + a short summary of a culpert profile.
/// Doesn't render the per-span / per-callsite breakdown — that's what
/// `culpert report` is for. Useful for CI logs and quick file inspection.
fn run_info(path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let profile = load(path)?;

    // Total counts across all samples in the file (regular + synthetic).
    // Synthetic samples (parent-span markers) have count=0/bytes=0 so
    // they don't skew the totals.
    let sample_count: i64 = profile.sample.iter().filter_map(|s| s.value.first()).sum();
    let total_bytes: i64 = profile.sample.iter().filter_map(|s| s.value.get(1)).sum();

    // Unique span names referenced by any sample.
    let span_name_key = string_index(&profile, "span_name");
    let mut unique_spans: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for sample in &profile.sample {
        if let Some(name) = span_name_key.and_then(|k| label_str(sample, k, &profile)) {
            unique_spans.insert(name);
        }
    }

    let rate_bytes = profile.period.max(1) as u64;
    let metadata = pprof::metadata(&profile);

    println!("File:            {}", path.display());
    println!("Sample rate:     {}/alloc", format_bytes(rate_bytes));
    println!("Total samples:   {sample_count}");
    println!(
        "Total bytes:     {}",
        format_bytes(total_bytes.max(0) as u64)
    );
    println!("Unique spans:    {}", unique_spans.len());

    if metadata.is_empty() {
        println!();
        println!("Metadata:        (none — install with Config {{ metadata: ... }} to embed)");
    } else {
        println!();
        println!("Metadata:");
        // Stable, sorted output.
        let mut pairs: Vec<(&String, &String)> = metadata.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        let key_w = pairs.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
        for (k, v) in pairs {
            println!("  {k:<key_w$}  {v}");
        }
    }

    Ok(())
}

// ---- archive integration -----------------------------------------------
//
// `culpert upload` / `culpert pull` — thin wrappers over `archive::*`
// that translate CLI args into the right call shape, print the response,
// and translate the result into the right exit code.

/// Compose an optional [`archive::CfAccess`] from the two CLI flags.
/// When the `cloudflare-access` feature isn't built in, both arguments
/// are absent and the function just returns `None`.
///
/// Clap enforces "both or neither" via the `requires =` attribute on
/// each flag, so by the time we get here we either have a complete
/// pair or no values at all.
fn build_cf_access(
    #[cfg(feature = "cloudflare-access")] client_id: Option<String>,
    #[cfg(feature = "cloudflare-access")] client_secret: Option<String>,
) -> Option<archive::CfAccess> {
    #[cfg(feature = "cloudflare-access")]
    {
        match (client_id, client_secret) {
            (Some(client_id), Some(client_secret)) => Some(archive::CfAccess {
                client_id,
                client_secret,
            }),
            _ => None,
        }
    }
    #[cfg(not(feature = "cloudflare-access"))]
    {
        None
    }
}

fn run_upload(
    file: &PathBuf,
    endpoint: &str,
    token: &str,
    project: &str,
    commit_sha: &str,
    branch: Option<&str>,
    cf_access: Option<archive::CfAccess>,
) -> Result<(), Box<dyn std::error::Error>> {
    let ep = archive::Endpoint {
        url: endpoint.to_string(),
        token: token.to_string(),
        project: project.to_string(),
        cf_access,
    };
    let response = archive::upload(&ep, file, commit_sha, branch)?;
    println!("{response}");
    eprintln!(
        "uploaded {} as commit {commit_sha}{}",
        file.display(),
        match branch {
            Some(b) => format!(" on branch {b}"),
            None => String::new(),
        }
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_pull(
    endpoint: &str,
    token: &str,
    project: &str,
    sha: Option<&str>,
    latest_of: Option<&str>,
    output: Option<&PathBuf>,
    allow_missing: bool,
    cf_access: Option<archive::CfAccess>,
) -> Result<i32, Box<dyn std::error::Error>> {
    let target = match (sha, latest_of) {
        (Some(s), None) => archive::PullTarget::BySha(s.to_string()),
        (None, Some(b)) => archive::PullTarget::LatestOf(b.to_string()),
        (Some(_), Some(_)) => {
            return Err("pass only one of --sha or --latest-of".into());
        }
        (None, None) => {
            return Err("pass one of --sha or --latest-of".into());
        }
    };

    let ep = archive::Endpoint {
        url: endpoint.to_string(),
        token: token.to_string(),
        project: project.to_string(),
        cf_access,
    };
    let found = archive::pull(&ep, &target, output, allow_missing)?;
    if !found {
        // --allow-missing path: exit 0 but make the absence visible.
        // The CI step that runs `culpert diff` after this can branch on
        // `[ -f baseline.pb.gz ]` and skip cleanly.
        return Ok(0);
    }
    if let Some(path) = output {
        eprintln!("pulled to {}", path.display());
    }
    Ok(0)
}
