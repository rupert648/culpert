//! HTTP client for [culpert-archive](https://github.com/rupert648/culpert-archive),
//! the companion Cloudflare Worker that stores `.pb.gz` profiles in R2,
//! indexed by commit SHA in D1.
//!
//! Two operations are wired:
//!
//! - [`upload`] — `POST /v1/profiles/{sha}?branch=<branch>` with the
//!   file body. Used by CI right after capturing a profile so subsequent
//!   runs have a baseline to diff against.
//! - [`pull`] — `GET /v1/profiles/{sha}` or `GET /v1/profiles/latest?branch=<branch>`,
//!   write the response body to a path (or stdout).
//!
//! Sync `ureq` rather than `reqwest` — no tokio, no async runtime, a few
//! hundred KB instead of a few MB, and the API is one line per call. The
//! CLI is otherwise sync; introducing tokio just for two HTTP requests
//! would be silly.

use std::io::{self, Write};
use std::path::PathBuf;

/// Connection parameters for a culpert-archive instance.
///
/// `endpoint` is the worker base URL (no trailing slash, no path);
/// `token` is the `AUTH_TOKEN` you set via `wrangler secret put`.
/// Both are typically read from environment variables in CI
/// (`CULPERT_ARCHIVE` / `CULPERT_TOKEN`).
///
/// `cf_access` carries optional Cloudflare Access service-token
/// credentials for archive instances that sit behind Cloudflare Access.
/// Only meaningful when the crate is built with the `cloudflare-access`
/// feature — without that feature the field exists (as `None`) but no
/// CLI surface wires it up, so the headers are never set.
pub struct Endpoint {
    pub url: String,
    pub token: String,
    pub cf_access: Option<CfAccess>,
}

/// Cloudflare Access service-token credentials. Sent as request headers:
///
/// - `CF-Access-Client-Id: <client_id>`
/// - `CF-Access-Client-Secret: <client_secret>`
///
/// Cloudflare strips these at the edge and forwards a signed JWT in
/// `Cf-Access-Jwt-Assertion` to the worker, which we don't currently
/// inspect — the existing bearer-token check inside the worker is the
/// "after-Access" gate.
pub struct CfAccess {
    pub client_id: String,
    pub client_secret: String,
}

impl Endpoint {
    /// Strip a trailing slash off the URL so concatenating `/v1/...`
    /// always produces a single-slash boundary.
    pub fn normalised_url(&self) -> &str {
        self.url.strip_suffix('/').unwrap_or(&self.url)
    }
}

/// What `pull` should fetch.
pub enum PullTarget {
    /// `GET /v1/profiles/{sha}` — the exact commit's profile.
    BySha(String),
    /// `GET /v1/profiles/latest?branch={name}` — the most recent profile
    /// on that branch. Used by CI to grab the baseline.
    LatestOf(String),
}

/// `POST /v1/profiles/{sha}?branch={branch}` with the file's bytes as
/// the body. Returns the parsed JSON response on 201, or an error
/// describing what the server said on any other status.
///
/// `Content-Type: application/octet-stream` — the worker doesn't parse
/// the body, but sending the right content type avoids edge clients
/// trying to interpret it.
pub fn upload(
    endpoint: &Endpoint,
    file: &PathBuf,
    commit_sha: &str,
    branch: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(file).map_err(|e| format!("read {}: {e}", file.display()))?;

    let mut url = format!("{}/v1/profiles/{}", endpoint.normalised_url(), commit_sha,);
    if let Some(b) = branch {
        // ureq doesn't url-encode query values automatically; manually
        // percent-encode anything unsafe. Branch names are usually
        // [a-zA-Z0-9._/-]+ so this is defensive.
        url.push_str("?branch=");
        url.push_str(&percent_encode(b));
    }

    let mut request = ureq::post(&url)
        .set("Authorization", &format!("Bearer {}", endpoint.token))
        .set("Content-Type", "application/octet-stream");
    if let Some(access) = &endpoint.cf_access {
        request = request
            .set("CF-Access-Client-Id", &access.client_id)
            .set("CF-Access-Client-Secret", &access.client_secret);
    }
    let response = request.send_bytes(&bytes);

    match response {
        Ok(r) => r
            .into_string()
            .map_err(|e| format!("read upload response: {e}").into()),
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_else(|_| String::new());
            Err(format!("upload failed: HTTP {code}: {body}").into())
        }
        Err(e) => Err(format!("upload request failed: {e}").into()),
    }
}

/// `GET` the requested profile and write the body to `output` (or
/// stdout if `output` is `None`). Returns `Ok(true)` on success,
/// `Ok(false)` on 404 (used by `--allow-missing` to exit cleanly when
/// the baseline doesn't exist yet), `Err` for anything else.
pub fn pull(
    endpoint: &Endpoint,
    target: &PullTarget,
    output: Option<&PathBuf>,
    allow_missing: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    let url = match target {
        PullTarget::BySha(sha) => format!("{}/v1/profiles/{}", endpoint.normalised_url(), sha,),
        PullTarget::LatestOf(branch) => format!(
            "{}/v1/profiles/latest?branch={}",
            endpoint.normalised_url(),
            percent_encode(branch),
        ),
    };

    let mut request = ureq::get(&url).set("Authorization", &format!("Bearer {}", endpoint.token));
    if let Some(access) = &endpoint.cf_access {
        request = request
            .set("CF-Access-Client-Id", &access.client_id)
            .set("CF-Access-Client-Secret", &access.client_secret);
    }
    let response = request.call();

    let response = match response {
        Ok(r) => r,
        Err(ureq::Error::Status(404, _)) if allow_missing => {
            eprintln!("culpert pull: 404 (no profile found); skipping per --allow-missing");
            return Ok(false);
        }
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_else(|_| String::new());
            return Err(format!("pull failed: HTTP {code}: {body}").into());
        }
        Err(e) => return Err(format!("pull request failed: {e}").into()),
    };

    // Read body straight into the chosen sink — no need to buffer in
    // memory first. Profiles are small (typically tens of KB) but it's
    // good hygiene.
    let mut reader = response.into_reader();
    match output {
        Some(path) => {
            let mut file = std::fs::File::create(path)
                .map_err(|e| format!("create {}: {e}", path.display()))?;
            io::copy(&mut reader, &mut file)
                .map_err(|e| format!("write {}: {e}", path.display()))?;
        }
        None => {
            io::copy(&mut reader, &mut io::stdout()).map_err(|e| format!("write stdout: {e}"))?;
        }
    }
    Ok(true)
}

/// Minimal RFC 3986 percent-encoder for query-string values. We only
/// pass branch names through; this handles `/`, `#`, `?`, spaces and
/// other awkwardness so the URL doesn't fail to parse server-side.
/// Not exhaustive; not for arbitrary input.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// Ensure `Write` is imported when used (the io::copy variants pull it
// in transitively, but explicit is clearer for the maintainer).
#[allow(dead_code)]
fn _ensure_write_in_scope() -> impl Write {
    io::sink()
}
