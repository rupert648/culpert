//! pprof protobuf export.
//!
//! Encodes a culpert [`Profile`] into a pprof-format protobuf
//! message (`perftools.profiles.Profile`). Two emit functions:
//!
//! - [`encode`] — raw protobuf bytes (no compression). Suitable for
//!   downstream consumers that handle their own framing.
//! - [`encode_gzipped`] — gzip-compressed protobuf bytes. This is the
//!   on-disk convention pprof and its ecosystem expect (`*.pb.gz`).
//!
//! ## Sample value model
//!
//! pprof's heap-profile convention uses two values per sample:
//!
//! - `allocations / count` — number of sampled allocations
//! - `space / bytes`       — sum of `Layout::size()` across those allocations
//!
//! Multiply by `Profile::config.rate_bytes` to recover an unbiased estimate
//! of total bytes / total allocations.
//!
//! ## Span attribution
//!
//! Each sample carries up to three pprof labels:
//!
//! - `span_id`        — numeric label, the SpanId minted by the SpanContext
//! - `span_name`      — string label, the human-readable span name
//! - `span_parent_id` — numeric label, the parent SpanId (only emitted if
//!   the SpanContext reports a non-`None` parent in the SpanMetadata)
//!
//! Stock `pprof` exposes labels as filterable / groupable axes, which gets
//! us "show only handle_request samples" out-of-the-box. Hierarchical
//! roll-up ("which sub-span dominates within `handle_request`?") is the
//! job of the `culpert report` CLI; `span_parent_id` is what powers the
//! tree view.

use crate::span::SpanId;
use crate::Profile;
use prost::Message;
use std::collections::HashMap;

/// pprof protobuf types, hand-written from `profile.proto`.
///
/// We carry the messages directly (with `#[derive(prost::Message)]`) rather
/// than running `prost-build` from a `build.rs`, so culpert's build does not
/// need `protoc` on the user's machine. Tag numbers and field types are
/// taken from <https://github.com/google/pprof/blob/main/proto/profile.proto>.
///
/// These types are exposed publicly so external tools (notably `culpert-cli`)
/// can decode profiles without having to vendor their own copy of the proto.
pub mod proto {
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Profile {
        #[prost(message, repeated, tag = "1")]
        pub sample_type: Vec<ValueType>,
        #[prost(message, repeated, tag = "2")]
        pub sample: Vec<Sample>,
        #[prost(message, repeated, tag = "3")]
        pub mapping: Vec<Mapping>,
        #[prost(message, repeated, tag = "4")]
        pub location: Vec<Location>,
        #[prost(message, repeated, tag = "5")]
        pub function: Vec<Function>,
        #[prost(string, repeated, tag = "6")]
        pub string_table: Vec<String>,
        #[prost(int64, tag = "7")]
        pub drop_frames: i64,
        #[prost(int64, tag = "8")]
        pub keep_frames: i64,
        #[prost(int64, tag = "9")]
        pub time_nanos: i64,
        #[prost(int64, tag = "10")]
        pub duration_nanos: i64,
        #[prost(message, optional, tag = "11")]
        pub period_type: Option<ValueType>,
        #[prost(int64, tag = "12")]
        pub period: i64,
        #[prost(int64, repeated, tag = "13")]
        pub comment: Vec<i64>,
        #[prost(int64, tag = "14")]
        pub default_sample_type: i64,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ValueType {
        #[prost(int64, tag = "1")]
        pub r#type: i64,
        #[prost(int64, tag = "2")]
        pub unit: i64,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Sample {
        #[prost(uint64, repeated, tag = "1")]
        pub location_id: Vec<u64>,
        #[prost(int64, repeated, tag = "2")]
        pub value: Vec<i64>,
        #[prost(message, repeated, tag = "3")]
        pub label: Vec<Label>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Label {
        #[prost(int64, tag = "1")]
        pub key: i64,
        #[prost(int64, tag = "2")]
        pub str: i64,
        #[prost(int64, tag = "3")]
        pub num: i64,
        #[prost(int64, tag = "4")]
        pub num_unit: i64,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Mapping {
        #[prost(uint64, tag = "1")]
        pub id: u64,
        #[prost(uint64, tag = "2")]
        pub memory_start: u64,
        #[prost(uint64, tag = "3")]
        pub memory_limit: u64,
        #[prost(uint64, tag = "4")]
        pub file_offset: u64,
        #[prost(int64, tag = "5")]
        pub filename: i64,
        #[prost(int64, tag = "6")]
        pub build_id: i64,
        #[prost(bool, tag = "7")]
        pub has_functions: bool,
        #[prost(bool, tag = "8")]
        pub has_filenames: bool,
        #[prost(bool, tag = "9")]
        pub has_line_numbers: bool,
        #[prost(bool, tag = "10")]
        pub has_inline_frames: bool,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Location {
        #[prost(uint64, tag = "1")]
        pub id: u64,
        #[prost(uint64, tag = "2")]
        pub mapping_id: u64,
        #[prost(uint64, tag = "3")]
        pub address: u64,
        #[prost(message, repeated, tag = "4")]
        pub line: Vec<Line>,
        #[prost(bool, tag = "5")]
        pub is_folded: bool,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Line {
        #[prost(uint64, tag = "1")]
        pub function_id: u64,
        #[prost(int64, tag = "2")]
        pub line: i64,
        #[prost(int64, tag = "3")]
        pub column: i64,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Function {
        #[prost(uint64, tag = "1")]
        pub id: u64,
        #[prost(int64, tag = "2")]
        pub name: i64,
        #[prost(int64, tag = "3")]
        pub system_name: i64,
        #[prost(int64, tag = "4")]
        pub filename: i64,
        #[prost(int64, tag = "5")]
        pub start_line: i64,
    }
}

/// String-interning helper. pprof requires `string_table[0]` to be `""`.
struct StringTable {
    strings: Vec<String>,
    index: HashMap<String, i64>,
}

impl StringTable {
    fn new() -> Self {
        let mut s = Self {
            strings: Vec::new(),
            index: HashMap::new(),
        };
        s.intern(""); // index 0 must be empty per pprof spec
        s
    }

    fn intern(&mut self, s: &str) -> i64 {
        if let Some(&idx) = self.index.get(s) {
            return idx;
        }
        let idx = self.strings.len() as i64;
        self.strings.push(s.to_owned());
        self.index.insert(s.to_owned(), idx);
        idx
    }

    fn into_vec(self) -> Vec<String> {
        self.strings
    }
}

/// Encode a [`Profile`] as raw pprof protobuf bytes (uncompressed).
pub fn encode(profile: &Profile) -> Vec<u8> {
    build_proto(profile).encode_to_vec()
}

/// Encode a [`Profile`] as gzip-compressed pprof bytes — the on-disk
/// convention pprof tooling expects (`*.pb.gz`).
pub fn encode_gzipped(profile: &Profile) -> std::io::Result<Vec<u8>> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let bytes = encode(profile);
    let mut gz = GzEncoder::new(Vec::with_capacity(bytes.len() / 4), Compression::default());
    gz.write_all(&bytes)?;
    gz.finish()
}

/// Errors that can arise from [`decode_gzipped`].
#[derive(Debug)]
pub enum DecodeError {
    /// Gzip decompression failed.
    Gzip(std::io::Error),
    /// Protobuf decode failed.
    Proto(prost::DecodeError),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Gzip(e) => write!(f, "gzip decompression failed: {e}"),
            DecodeError::Proto(e) => write!(f, "protobuf decode failed: {e}"),
        }
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DecodeError::Gzip(e) => Some(e),
            DecodeError::Proto(e) => Some(e),
        }
    }
}

/// Decode gzip-compressed pprof bytes into a [`proto::Profile`].
///
/// Companion to [`encode_gzipped`]. Used by `culpert-cli` to read profiles
/// off disk for reporting; exposed publicly so third-party Rust code can
/// also analyse culpert profiles programmatically.
pub fn decode_gzipped(bytes: &[u8]) -> Result<proto::Profile, DecodeError> {
    use flate2::read::GzDecoder;
    use std::io::Read;

    let mut decoder = GzDecoder::new(bytes);
    let mut decompressed = Vec::with_capacity(bytes.len() * 4);
    decoder
        .read_to_end(&mut decompressed)
        .map_err(DecodeError::Gzip)?;
    proto::Profile::decode(&decompressed[..]).map_err(DecodeError::Proto)
}

fn build_proto(profile: &Profile) -> proto::Profile {
    let mut strs = StringTable::new();

    // Heap-profile sample-type convention from profile.proto comment.
    let sample_type = vec![
        proto::ValueType {
            r#type: strs.intern("allocations"),
            unit: strs.intern("count"),
        },
        proto::ValueType {
            r#type: strs.intern("space"),
            unit: strs.intern("bytes"),
        },
    ];

    // Single fake mapping. pprof requires Locations to have a non-zero
    // mapping_id pointing to a known Mapping, but we don't know real
    // module addresses; use [0, u64::MAX].
    let mapping_id = 1u64;
    let empty = strs.intern("");
    let mapping = proto::Mapping {
        id: mapping_id,
        memory_start: 0,
        memory_limit: u64::MAX,
        file_offset: 0,
        filename: empty,
        build_id: empty,
        has_functions: true,
        has_filenames: true,
        has_line_numbers: true,
        has_inline_frames: false,
    };

    // Pre-intern the label keys so they're stable.
    let span_id_key = strs.intern("span_id");
    let span_name_key = strs.intern("span_name");
    let span_parent_id_key = strs.intern("span_parent_id");

    // Function dedup keyed by (name, filename); Location dedup keyed by ip.
    let mut funcs: HashMap<(String, String), u64> = HashMap::new();
    let mut functions: Vec<proto::Function> = Vec::new();
    let mut next_func_id: u64 = 1;

    let mut locs: HashMap<usize, u64> = HashMap::new();
    let mut locations: Vec<proto::Location> = Vec::new();
    let mut next_loc_id: u64 = 1;

    let mut samples: Vec<proto::Sample> = Vec::with_capacity(profile.entries.len());

    for entry in &profile.entries {
        let mut location_ids = Vec::with_capacity(entry.frames.len());
        for frame in &entry.frames {
            let loc_id = match locs.get(&frame.ip) {
                Some(&id) => id,
                None => {
                    // Function dedup. Empty strings for missing fields.
                    let fname = frame.name.clone().unwrap_or_default();
                    let ffile = frame.filename.clone().unwrap_or_default();
                    let func_id = match funcs.get(&(fname.clone(), ffile.clone())) {
                        Some(&fid) => fid,
                        None => {
                            let fid = next_func_id;
                            next_func_id += 1;
                            let name_idx = strs.intern(&fname);
                            let file_idx = strs.intern(&ffile);
                            functions.push(proto::Function {
                                id: fid,
                                name: name_idx,
                                system_name: name_idx,
                                filename: file_idx,
                                start_line: 0,
                            });
                            funcs.insert((fname, ffile), fid);
                            fid
                        }
                    };

                    let id = next_loc_id;
                    next_loc_id += 1;
                    locations.push(proto::Location {
                        id,
                        mapping_id,
                        address: frame.ip as u64,
                        line: vec![proto::Line {
                            function_id: func_id,
                            line: frame.lineno.unwrap_or(0) as i64,
                            column: 0,
                        }],
                        is_folded: false,
                    });
                    locs.insert(frame.ip, id);
                    id
                }
            };
            location_ids.push(loc_id);
        }

        // Span attribution as labels.
        let mut labels: Vec<proto::Label> = Vec::new();
        if let Some(span_id) = entry.span {
            labels.push(proto::Label {
                key: span_id_key,
                str: 0,
                num: span_id.get() as i64,
                num_unit: 0,
            });
            if let Some(meta) = profile.spans.get(&span_id) {
                let name_idx = strs.intern(&meta.name);
                labels.push(proto::Label {
                    key: span_name_key,
                    str: name_idx,
                    num: 0,
                    num_unit: 0,
                });
                if let Some(parent_id) = meta.parent {
                    labels.push(proto::Label {
                        key: span_parent_id_key,
                        str: 0,
                        num: parent_id.get() as i64,
                        num_unit: 0,
                    });
                }
            }
        }

        samples.push(proto::Sample {
            location_id: location_ids,
            value: vec![entry.samples as i64, entry.bytes_total as i64],
            label: labels,
        });
    }

    // Synthetic samples for parent spans that have no direct samples of
    // their own. Without these, the CLI's tree builder can't resolve the
    // parent's name (its id-to-name map is built by scanning sample labels),
    // so children appear as roots and the hierarchy is lost.
    //
    // Common case: a `handle_request` body that only orchestrates sub-spans
    // (`parse_input`, `build_response`) — there are no direct allocations to
    // tag `span_name = "handle_request"` on, but `parse_input`'s samples
    // carry `span_parent_id = <handle_request id>`. The synthetic sample
    // below carries `span_id` + `span_name` for the parent so the CLI can
    // close the loop, with `value = [0, 0]` so it doesn't add to any
    // aggregate.
    let entry_span_ids: std::collections::HashSet<SpanId> = profile
        .entries
        .iter()
        .filter_map(|e| e.span)
        .collect();
    for (&span_id, meta) in &profile.spans {
        if entry_span_ids.contains(&span_id) {
            continue;
        }
        let mut labels: Vec<proto::Label> = Vec::with_capacity(3);
        labels.push(proto::Label {
            key: span_id_key,
            str: 0,
            num: span_id.get() as i64,
            num_unit: 0,
        });
        let name_idx = strs.intern(&meta.name);
        labels.push(proto::Label {
            key: span_name_key,
            str: name_idx,
            num: 0,
            num_unit: 0,
        });
        if let Some(parent_id) = meta.parent {
            labels.push(proto::Label {
                key: span_parent_id_key,
                str: 0,
                num: parent_id.get() as i64,
                num_unit: 0,
            });
        }
        samples.push(proto::Sample {
            location_id: vec![],
            value: vec![0, 0],
            label: labels,
        });
    }

    let period_type = proto::ValueType {
        r#type: strs.intern("space"),
        unit: strs.intern("bytes"),
    };

    let time_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);

    proto::Profile {
        sample_type,
        sample: samples,
        mapping: vec![mapping],
        location: locations,
        function: functions,
        string_table: strs.into_vec(),
        drop_frames: 0,
        keep_frames: 0,
        time_nanos,
        duration_nanos: 0,
        period_type: Some(period_type),
        period: profile.config.rate_bytes as i64,
        comment: vec![],
        default_sample_type: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::span::{SpanId, SpanMetadata};
    use crate::{Frame, ProfileEntry};
    use std::num::NonZeroU64;

    fn span(n: u64) -> SpanId {
        NonZeroU64::new(n).unwrap()
    }

    fn fake_profile() -> Profile {
        let mut spans = std::collections::HashMap::new();
        spans.insert(
            span(7),
            SpanMetadata {
                name: "handle_request".into(),
                parent: None,
            },
        );
        spans.insert(
            span(8),
            SpanMetadata {
                name: "render_template".into(),
                parent: Some(span(7)),
            },
        );

        let frames_a = vec![
            Frame {
                ip: 0x1000,
                name: Some("render_template".into()),
                filename: Some("render/src/template.rs".into()),
                lineno: Some(142),
            },
            Frame {
                ip: 0x2000,
                name: Some("build_response".into()),
                filename: Some("response/src/build.rs".into()),
                lineno: Some(40),
            },
        ];
        let frames_b = vec![Frame {
            ip: 0x1000,
            name: Some("render_template".into()),
            filename: Some("render/src/template.rs".into()),
            lineno: Some(142),
        }];

        Profile {
            entries: vec![
                ProfileEntry {
                    span: Some(span(8)),
                    frames: frames_a,
                    bytes_total: 12_400_000,
                    samples: 24,
                },
                ProfileEntry {
                    span: Some(span(7)),
                    frames: frames_b,
                    bytes_total: 5_000_000,
                    samples: 10,
                },
                ProfileEntry {
                    span: None,
                    frames: vec![],
                    bytes_total: 1_024,
                    samples: 1,
                },
            ],
            spans,
            dropped_samples: 0,
            config: Config::default(),
        }
    }

    #[test]
    fn roundtrip_via_prost() {
        let profile = fake_profile();
        let bytes = encode(&profile);
        assert!(!bytes.is_empty(), "encoded bytes should not be empty");

        let decoded = proto::Profile::decode(&bytes[..]).expect("re-decode");

        // Sample-type schema: allocations/count + space/bytes.
        assert_eq!(decoded.sample_type.len(), 2);
        let st0 = &decoded.sample_type[0];
        let st1 = &decoded.sample_type[1];
        assert_eq!(decoded.string_table[st0.r#type as usize], "allocations");
        assert_eq!(decoded.string_table[st0.unit as usize], "count");
        assert_eq!(decoded.string_table[st1.r#type as usize], "space");
        assert_eq!(decoded.string_table[st1.unit as usize], "bytes");

        // 3 samples in, 3 out.
        assert_eq!(decoded.sample.len(), 3);

        // Each sample has 2 values (count, bytes).
        for s in &decoded.sample {
            assert_eq!(s.value.len(), 2);
        }

        // Spec: string_table[0] == "".
        assert_eq!(decoded.string_table[0], "");

        // Span labels present on the spanned samples; absent on the unspanned one.
        let spanned: Vec<&proto::Sample> =
            decoded.sample.iter().filter(|s| !s.label.is_empty()).collect();
        assert_eq!(spanned.len(), 2);

        for s in spanned {
            let by_key: std::collections::HashMap<&str, &proto::Label> = s
                .label
                .iter()
                .map(|l| (decoded.string_table[l.key as usize].as_str(), l))
                .collect();
            assert!(by_key.contains_key("span_id"));
            assert!(by_key.contains_key("span_name"));
        }

        // Dedup: location 0x1000 appears in two entries; should be one Location.
        let unique_locs: std::collections::HashSet<u64> =
            decoded.location.iter().map(|l| l.id).collect();
        assert_eq!(unique_locs.len(), decoded.location.len());

        // Mapping is the single fake covering everything.
        assert_eq!(decoded.mapping.len(), 1);
        assert_eq!(decoded.mapping[0].memory_limit, u64::MAX);
    }

    #[test]
    fn gzipped_round_trip() {
        let profile = fake_profile();
        let bytes = encode_gzipped(&profile).expect("gzip");

        // gzip magic.
        assert_eq!(&bytes[..2], &[0x1f, 0x8b]);

        // Decompress and decode.
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut decoder = GzDecoder::new(&bytes[..]);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).expect("gunzip");

        let decoded = proto::Profile::decode(&decompressed[..]).expect("decode");
        assert_eq!(decoded.sample.len(), 3);
    }

    #[test]
    fn empty_profile_encodes() {
        let profile = Profile {
            entries: vec![],
            spans: std::collections::HashMap::new(),
            dropped_samples: 0,
            config: Config::default(),
        };
        let bytes = encode(&profile);
        let decoded = proto::Profile::decode(&bytes[..]).expect("decode");
        assert_eq!(decoded.sample.len(), 0);
        assert_eq!(decoded.location.len(), 0);
        assert_eq!(decoded.function.len(), 0);
        // String table must still have the empty string at [0].
        assert_eq!(decoded.string_table[0], "");
    }
}
