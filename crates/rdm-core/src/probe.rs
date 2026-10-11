//! Discovering what is behind a URL before downloading it.
//!
//! The probe is a `GET` with `Range: bytes=0-0` rather than a `HEAD`: many
//! servers answer `HEAD` incorrectly (or not at all), while the ranged GET
//! shows directly whether ranges work. A `206` reply carries the total size
//! in `Content-Range`. A `200` reply means ranges are not supported, and its
//! body is handed to the single-stream path instead of being thrown away,
//! which also keeps one-time download links working.

use percent_encoding::percent_decode_str;
use reqwest::header::{self, HeaderMap};
use reqwest::{Response, StatusCode, Url};

use crate::error::{Error, Result};
use crate::http::{self, HttpClient};

/// What the server told us about a resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeInfo {
    /// The URL as requested.
    pub url: Url,
    /// The URL after following redirects.
    pub final_url: Url,
    /// Total size in bytes, if the server disclosed it.
    pub size: Option<u64>,
    /// Byte ranges work, so the download can be segmented and resumed.
    pub resumable: bool,
    /// The server advertised `Accept-Ranges: bytes` (informational; the
    /// range probe itself decides `resumable`).
    pub accept_ranges: bool,
    pub content_type: Option<String>,
    /// Sanitized file name from `Content-Disposition` or the URL path.
    pub filename: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// A checksum of the whole file published by the server, if any.
    pub digest: Option<ContentDigest>,
}

/// A server-published checksum of the complete file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentDigest {
    /// `md5`, `sha-1`, `sha-256` or `sha-512`.
    pub algorithm: String,
    /// Lower-case hex.
    pub hex: String,
}

impl ProbeInfo {
    /// Value for `If-Range`, which makes the server send the full new entity
    /// (status 200) instead of mixing old and new bytes if the file changed.
    /// Weak ETags are not allowed there, so `Last-Modified` is used instead.
    pub(crate) fn if_range(&self) -> Option<&str> {
        match &self.etag {
            Some(e) if !e.starts_with("W/") => Some(e),
            _ => self.last_modified.as_deref(),
        }
    }
}

/// Probes the resource. The `Response` is returned only when it is a full
/// `200` body that the caller may stream from.
pub(crate) async fn probe(client: &HttpClient) -> Result<(ProbeInfo, Option<Response>)> {
    let resp = client
        .get()
        .header(header::RANGE, "bytes=0-0")
        .send()
        .await
        .map_err(Error::network)?;

    match resp.status() {
        StatusCode::PARTIAL_CONTENT => {
            let total = http::content_range(resp.headers())
                .filter(|cr| cr.first == 0)
                .ok_or_else(|| Error::Protocol("206 without a valid Content-Range".into()))?
                .total;
            let mut info = build_info(client.url(), &resp, total, total.is_some());
            // Content-MD5 on a 206 covers only the range; ignore it here.
            info.digest = repr_digest(resp.headers());
            Ok((info, None))
        }
        StatusCode::OK => {
            let size = resp.content_length();
            let info = build_info(client.url(), &resp, size, false);
            Ok((info, Some(resp)))
        }
        StatusCode::RANGE_NOT_SATISFIABLE => {
            // Typically an empty file ("bytes */0"). Ask again without a range.
            let resp = client.get().send().await.map_err(Error::network)?;
            if resp.status() != StatusCode::OK {
                return Err(http::status_error(&resp));
            }
            let size = resp.content_length();
            let info = build_info(client.url(), &resp, size, false);
            Ok((info, Some(resp)))
        }
        _ => Err(http::status_error(&resp)),
    }
}

fn build_info(url: &Url, resp: &Response, size: Option<u64>, resumable: bool) -> ProbeInfo {
    let headers = resp.headers();
    let final_url = resp.url().clone();
    ProbeInfo {
        url: url.clone(),
        filename: filename(headers, &final_url),
        final_url,
        size,
        resumable,
        accept_ranges: headers
            .get(header::ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().contains("bytes")),
        content_type: http::header_str(headers, header::CONTENT_TYPE),
        etag: http::header_str(headers, header::ETAG),
        last_modified: http::header_str(headers, header::LAST_MODIFIED),
        digest: repr_digest(headers).or_else(|| {
            headers
                .get("content-md5")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| base64_decode(v.trim()))
                .filter(|b| b.len() == 16)
                .map(|b| ContentDigest {
                    algorithm: "md5".into(),
                    hex: to_hex(&b),
                })
        }),
    }
}

/// The strongest digest from `Repr-Digest` (RFC 9530, `sha-256=:b64:`) or
/// the older `Digest` header (RFC 3230, `SHA-256=b64`).
fn repr_digest(headers: &HeaderMap) -> Option<ContentDigest> {
    const PREFERENCE: [(&str, usize); 4] =
        [("sha-512", 64), ("sha-256", 32), ("sha-1", 20), ("md5", 16)];
    let mut found: Vec<(String, Vec<u8>)> = Vec::new();
    for name in ["repr-digest", "digest"] {
        for value in headers.get_all(name).iter().filter_map(|v| v.to_str().ok()) {
            for item in value.split(',') {
                let Some((alg, val)) = item.split_once('=') else {
                    continue;
                };
                let alg = match alg.trim().to_ascii_lowercase().as_str() {
                    "sha" | "sha-1" => "sha-1".to_owned(),
                    other => other.to_owned(),
                };
                let val = val.trim().trim_matches(':');
                if let Some(bytes) = base64_decode(val) {
                    found.push((alg, bytes));
                }
            }
        }
    }
    PREFERENCE.iter().find_map(|(alg, len)| {
        found
            .iter()
            .find(|(a, b)| a == alg && b.len() == *len)
            .map(|(a, b)| ContentDigest {
                algorithm: a.clone(),
                hex: to_hex(b),
            })
    })
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Standard base64 with optional padding; `None` on anything else.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn filename(headers: &HeaderMap, final_url: &Url) -> String {
    let from_header = headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| std::str::from_utf8(v.as_bytes()).ok())
        .and_then(filename_from_disposition);
    let from_url = || {
        final_url
            .path_segments()
            .and_then(|mut s| s.next_back())
            .filter(|s| !s.is_empty())
            .map(|s| percent_decode_str(s).decode_utf8_lossy().into_owned())
    };
    sanitize_filename(&from_header.or_else(from_url).unwrap_or_default())
}

/// Extracts the file name from a `Content-Disposition` value, preferring the
/// RFC 6266 / RFC 5987 `filename*` form.
pub(crate) fn filename_from_disposition(value: &str) -> Option<String> {
    let mut plain = None;
    let mut extended = None;
    for param in split_params(value) {
        let Some((key, val)) = param.split_once('=') else {
            continue;
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "filename*" => extended = decode_ext_value(val.trim()),
            "filename" => plain = Some(unquote(val.trim())),
            _ => {}
        }
    }
    extended.or(plain).filter(|s| !s.trim().is_empty())
}

/// Splits on `;` outside quoted strings.
fn split_params(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut escaped) = (false, false);
    for c in value.chars() {
        match c {
            _ if escaped => {
                escaped = false;
                cur.push(c);
            }
            '\\' if quoted => {
                escaped = true;
                cur.push(c);
            }
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ';' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn unquote(v: &str) -> String {
    let Some(inner) = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
        return v.to_owned();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `charset'lang'percent-encoded` (RFC 5987).
fn decode_ext_value(v: &str) -> Option<String> {
    let mut parts = v.splitn(3, '\'');
    let charset = parts.next()?.trim().to_ascii_lowercase();
    let _lang = parts.next()?;
    let encoded = parts.next()?;
    let bytes: Vec<u8> = percent_decode_str(encoded).collect();
    match charset.as_str() {
        "utf-8" => String::from_utf8(bytes).ok(),
        // ISO-8859-1 maps bytes 1:1 onto the first 256 code points.
        "iso-8859-1" => Some(bytes.into_iter().map(char::from).collect()),
        _ => None,
    }
}

/// Makes a server-supplied name safe to use as a file name on every OS:
/// strips directories, replaces reserved and control characters, avoids
/// Windows device names, and bounds the length.
pub fn sanitize_filename(name: &str) -> String {
    const MAX_BYTES: usize = 200;
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let replaced: String = base
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = replaced.trim_matches(|c: char| c == '.' || c.is_whitespace());

    let mut out = String::new();
    for c in trimmed.chars() {
        if out.len() + c.len_utf8() > MAX_BYTES {
            break;
        }
        out.push(c);
    }
    let out = out.trim_end_matches(['.', ' ']).to_owned();
    if out.is_empty() {
        return "download".into();
    }

    let stem = out
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.ends_with(|c: char| c.is_ascii_digit()));
    if reserved { format!("_{out}") } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_variants() {
        let cases = [
            ("attachment; filename=\"report.pdf\"", Some("report.pdf")),
            ("attachment; filename=plain.txt", Some("plain.txt")),
            ("inline; filename=\"a;b.txt\"", Some("a;b.txt")),
            (
                "attachment; filename=\"q\\\"uote.txt\"",
                Some("q\"uote.txt"),
            ),
            (
                "attachment; filename=\"fallback.txt\"; filename*=UTF-8''na%C3%AFve%20file.txt",
                Some("naïve file.txt"),
            ),
            (
                "attachment; filename*=iso-8859-1'en'%E9t%E9.txt",
                Some("été.txt"),
            ),
            ("attachment; FILENAME=\"Upper.TXT\"", Some("Upper.TXT")),
            ("attachment", None),
            ("attachment; filename=\"\"", None),
        ];
        for (input, want) in cases {
            assert_eq!(filename_from_disposition(input).as_deref(), want, "{input}");
        }
    }

    #[test]
    fn sanitizing() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("C:\\Windows\\evil.exe"), "evil.exe");
        assert_eq!(
            sanitize_filename("a<b>c:d|e?f*g\"h.txt"),
            "a_b_c_d_e_f_g_h.txt"
        );
        assert_eq!(sanitize_filename("tab\there.txt"), "tab_here.txt");
        assert_eq!(sanitize_filename("  .hidden. "), "hidden");
        assert_eq!(sanitize_filename("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_filename("com1"), "_com1");
        assert_eq!(sanitize_filename("console.txt"), "console.txt");
        assert_eq!(sanitize_filename(""), "download");
        assert_eq!(sanitize_filename(".."), "download");
        let long = "é".repeat(300);
        assert!(sanitize_filename(&long).len() <= 200);
    }

    #[test]
    fn filename_falls_back_to_url() {
        let url = Url::parse("https://x.test/dir/My%20File.zip?token=1").unwrap();
        assert_eq!(filename(&HeaderMap::new(), &url), "My File.zip");
        let root = Url::parse("https://x.test/").unwrap();
        assert_eq!(filename(&HeaderMap::new(), &root), "download");
    }

    #[test]
    fn server_digests() {
        let mut h = HeaderMap::new();
        // sha-256 of "hello world"
        h.insert(
            "repr-digest",
            "md5=:XrY7u+Ae7tCTyyK7j1rNww==:, sha-256=:uU0nuZNNPgilLlLX2n2r+sSE7+N6U4DukIj3rOLvzek=:"
                .parse()
                .unwrap(),
        );
        let d = repr_digest(&h).unwrap();
        assert_eq!(d.algorithm, "sha-256");
        assert_eq!(
            d.hex,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );

        let mut old = HeaderMap::new();
        old.insert("digest", "MD5=XrY7u+Ae7tCTyyK7j1rNww==".parse().unwrap());
        assert_eq!(
            repr_digest(&old).unwrap().hex,
            "5eb63bbbe01eeed093cb22bb8f5acdc3"
        );

        let mut bad = HeaderMap::new();
        bad.insert("digest", "sha-256=not*base64".parse().unwrap());
        assert_eq!(repr_digest(&bad), None);
        assert_eq!(base64_decode("aGk="), Some(b"hi".to_vec()));
    }

    #[test]
    fn if_range_prefers_strong_etag() {
        let url = Url::parse("https://x.test/f").unwrap();
        let mut info = ProbeInfo {
            url: url.clone(),
            final_url: url,
            size: Some(1),
            resumable: true,
            accept_ranges: true,
            content_type: None,
            filename: "f".into(),
            etag: Some("\"abc\"".into()),
            last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
            digest: None,
        };
        assert_eq!(info.if_range(), Some("\"abc\""));
        info.etag = Some("W/\"abc\"".into());
        assert_eq!(info.if_range(), Some("Mon, 01 Jan 2024 00:00:00 GMT"));
        info.last_modified = None;
        assert_eq!(info.if_range(), None);
    }
}
