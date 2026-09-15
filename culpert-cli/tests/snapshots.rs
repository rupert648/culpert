/// Snapshot tests for `culpert` CLI output.
///
/// Each test builds a synthetic profile with known, round byte counts so the
/// rendered output is fully deterministic, runs the binary against it, and
/// compares against the stored `.snap` file. Run `cargo insta review` after
/// adding new tests or intentionally changing output to accept new snapshots.
///
/// Profile shape:
///   handle_request  (root span, no direct allocations)
///     ├─ parse_input       before: 512.00 KB   after: 768.00 KB  (+50%)
///     └─ build_response    before:   1.00 MB   after:   1.00 MB  (flat)
///
/// Rate: 64.00 KB/alloc. All values are exact powers-of-two so `format_bytes`
/// emits clean strings with no trailing digits.
use culpert::span::{SpanId, SpanMetadata};
use culpert::{Config, Frame, Profile, ProfileEntry};
use std::collections::HashMap;
use std::num::NonZeroU64;
use std::path::Path;
use std::process::Command;

const RATE: u64 = 65_536; // 64.00 KB/alloc

fn sid(n: u64) -> SpanId {
    NonZeroU64::new(n).unwrap()
}

fn make_profile(parse_bytes: u64, response_bytes: u64) -> Vec<u8> {
    let mut spans = HashMap::new();
    spans.insert(
        sid(1),
        SpanMetadata {
            name: "handle_request".into(),
            parent: None,
        },
    );
    spans.insert(
        sid(2),
        SpanMetadata {
            name: "parse_input".into(),
            parent: Some(sid(1)),
        },
    );
    spans.insert(
        sid(3),
        SpanMetadata {
            name: "build_response".into(),
            parent: Some(sid(1)),
        },
    );

    let profile = Profile {
        entries: vec![
            ProfileEntry {
                span: Some(sid(2)),
                frames: vec![Frame {
                    ip: 0x1000,
                    name: Some("my_app::parse::run".into()),
                    filename: Some("src/parse.rs".into()),
                    lineno: Some(42),
                }],
                bytes_total: parse_bytes,
                samples: 8,
            },
            ProfileEntry {
                span: Some(sid(3)),
                frames: vec![Frame {
                    ip: 0x2000,
                    name: Some("my_app::response::build".into()),
                    filename: Some("src/response.rs".into()),
                    lineno: Some(18),
                }],
                bytes_total: response_bytes,
                samples: 16,
            },
        ],
        spans,
        dropped_samples: 0,
        config: Config {
            rate_bytes: RATE,
            ..Default::default()
        },
    };
    culpert::pprof::encode_gzipped(&profile).unwrap()
}

fn culpert(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_culpert"))
        .args(args)
        .output()
        .expect("failed to run culpert binary");
    // Allow exit code 1 (regressions found) as well as 0.
    assert!(
        matches!(out.status.code(), Some(0) | Some(1)),
        "culpert exited with unexpected code {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn scrub(s: String, path: &Path, label: &str) -> String {
    s.replace(path.to_str().unwrap(), label)
}

// ---- report -------------------------------------------------------------

#[test]
fn report_tree() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("profile.pb.gz");
    std::fs::write(&p, make_profile(524_288, 1_048_576)).unwrap();

    let out = culpert(&["report", p.to_str().unwrap()]);
    insta::assert_snapshot!(out);
}

#[test]
fn report_flat() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("profile.pb.gz");
    std::fs::write(&p, make_profile(524_288, 1_048_576)).unwrap();

    let out = culpert(&["report", "--flat", p.to_str().unwrap()]);
    insta::assert_snapshot!(out);
}

// ---- diff ---------------------------------------------------------------

#[test]
fn diff_text_flat() {
    let dir = tempfile::tempdir().unwrap();
    let before_p = dir.path().join("before.pb.gz");
    let after_p = dir.path().join("after.pb.gz");
    std::fs::write(&before_p, make_profile(524_288, 1_048_576)).unwrap();
    std::fs::write(&after_p, make_profile(786_432, 1_048_576)).unwrap();

    let out = culpert(&[
        "diff",
        "--no-fail",
        before_p.to_str().unwrap(),
        after_p.to_str().unwrap(),
    ]);
    let out = scrub(out, &before_p, "[BEFORE]");
    let out = scrub(out, &after_p, "[AFTER]");
    insta::assert_snapshot!(out);
}

#[test]
fn diff_text_tree() {
    let dir = tempfile::tempdir().unwrap();
    let before_p = dir.path().join("before.pb.gz");
    let after_p = dir.path().join("after.pb.gz");
    std::fs::write(&before_p, make_profile(524_288, 1_048_576)).unwrap();
    std::fs::write(&after_p, make_profile(786_432, 1_048_576)).unwrap();

    let out = culpert(&[
        "diff",
        "--tree",
        "--no-fail",
        before_p.to_str().unwrap(),
        after_p.to_str().unwrap(),
    ]);
    let out = scrub(out, &before_p, "[BEFORE]");
    let out = scrub(out, &after_p, "[AFTER]");
    insta::assert_snapshot!(out);
}

#[test]
fn diff_markdown_flat() {
    let dir = tempfile::tempdir().unwrap();
    let before_p = dir.path().join("before.pb.gz");
    let after_p = dir.path().join("after.pb.gz");
    std::fs::write(&before_p, make_profile(524_288, 1_048_576)).unwrap();
    std::fs::write(&after_p, make_profile(786_432, 1_048_576)).unwrap();

    let out = culpert(&[
        "diff",
        "--format",
        "markdown",
        "--no-fail",
        before_p.to_str().unwrap(),
        after_p.to_str().unwrap(),
    ]);
    let out = scrub(out, &before_p, "[BEFORE]");
    let out = scrub(out, &after_p, "[AFTER]");
    insta::assert_snapshot!(out);
}

#[test]
fn diff_markdown_tree() {
    let dir = tempfile::tempdir().unwrap();
    let before_p = dir.path().join("before.pb.gz");
    let after_p = dir.path().join("after.pb.gz");
    std::fs::write(&before_p, make_profile(524_288, 1_048_576)).unwrap();
    std::fs::write(&after_p, make_profile(786_432, 1_048_576)).unwrap();

    let out = culpert(&[
        "diff",
        "--format",
        "markdown",
        "--tree",
        "--no-fail",
        before_p.to_str().unwrap(),
        after_p.to_str().unwrap(),
    ]);
    let out = scrub(out, &before_p, "[BEFORE]");
    let out = scrub(out, &after_p, "[AFTER]");
    insta::assert_snapshot!(out);
}

#[test]
fn diff_json() {
    let dir = tempfile::tempdir().unwrap();
    let before_p = dir.path().join("before.pb.gz");
    let after_p = dir.path().join("after.pb.gz");
    std::fs::write(&before_p, make_profile(524_288, 1_048_576)).unwrap();
    std::fs::write(&after_p, make_profile(786_432, 1_048_576)).unwrap();

    let out = culpert(&[
        "diff",
        "--format",
        "json",
        "--no-fail",
        before_p.to_str().unwrap(),
        after_p.to_str().unwrap(),
    ]);
    let out = scrub(out, &before_p, "[BEFORE]");
    let out = scrub(out, &after_p, "[AFTER]");
    insta::assert_snapshot!(out);
}

// ---- info ---------------------------------------------------------------

#[test]
fn info() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("profile.pb.gz");
    std::fs::write(&p, make_profile(524_288, 1_048_576)).unwrap();

    let out = culpert(&["info", p.to_str().unwrap()]);
    let out = scrub(out, &p, "[PROFILE]");
    insta::assert_snapshot!(out);
}

#[test]
fn report_groups_unknown_parents_by_all_child_names() {
    let mut spans = HashMap::new();
    let mut entries = Vec::new();
    for (parent, children) in [
        (1, vec!["parse", "render"]),
        (2, vec!["render", "parse"]),
        (3, vec!["parse"]),
    ] {
        spans.insert(
            sid(parent),
            SpanMetadata {
                name: format!("<unknown:{parent}>"),
                parent: None,
            },
        );
        for (index, name) in children.into_iter().enumerate() {
            let id = sid(parent * 10 + index as u64);
            spans.insert(
                id,
                SpanMetadata {
                    name: name.into(),
                    parent: Some(sid(parent)),
                },
            );
            entries.push(ProfileEntry {
                span: Some(id),
                frames: vec![],
                bytes_total: 1_048_576,
                samples: 16,
            });
        }
    }
    let profile = Profile {
        entries,
        spans,
        dropped_samples: 0,
        config: Config {
            rate_bytes: RATE,
            ..Default::default()
        },
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unknown-parents.pb.gz");
    std::fs::write(&path, culpert::pprof::encode_gzipped(&profile).unwrap()).unwrap();
    let output = culpert(&["report", path.to_str().unwrap()]);
    let merged = output
        .lines()
        .find(|line| line.contains("2 unknown parents"))
        .unwrap();
    assert!(merged.contains("4.00 MB"), "{output}");
    assert!(output.contains("<unknown:3>"), "{output}");
    assert!(!output.contains("<unknown:1>"), "{output}");
    assert!(!output.contains("<unknown:2>"), "{output}");
}
