//! HTTP client construction and response helpers.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use reqwest::cookie::Jar;
use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, RequestBuilder, Response, Url};

use crate::config::{EngineConfig, ProxyConfig, Request};
use crate::error::{Error, Result};

/// Headers the engine sets itself; user-supplied values are dropped.
/// (`Cookie` is not dropped: it is moved into the cookie jar.)
const CONTROLLED_HEADERS: [&str; 7] = [
    "range",
    "if-range",
    "accept-encoding",
    "host",
    "content-length",
    "connection",
    "transfer-encoding",
];

/// A configured client bound to one request (URL, auth, cookies, proxy).
#[derive(Debug, Clone)]
pub(crate) struct HttpClient {
    client: Client,
    request: Arc<Request>,
}

impl HttpClient {
    pub fn new(config: &EngineConfig, request: &Request, connections: usize) -> Result<Self> {
        let mut headers = HeaderMap::new();
        let mut has_user_agent = false;
        let mut cookie_strings: Vec<&str> = request.cookies.iter().map(String::as_str).collect();
        for (name, value) in &request.headers {
            let lower = name.to_ascii_lowercase();
            if lower == "cookie" {
                cookie_strings.push(value);
                continue;
            }
            if CONTROLLED_HEADERS.contains(&lower.as_str()) {
                continue;
            }
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| Error::InvalidHeader(name.clone()))?;
            let value =
                HeaderValue::from_str(value).map_err(|_| Error::InvalidHeader(name.to_string()))?;
            has_user_agent |= name == header::USER_AGENT;
            headers.insert(name, value);
        }
        // Byte offsets must refer to the raw entity, never a compressed one.
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );

        // The jar both carries browser-supplied cookies and keeps any cookies
        // set along redirect chains. A bare `name=value` would default to the
        // URL's directory as its path, so a redirect elsewhere on the same
        // host (common for attachments) would lose the session: scope them
        // to the whole host, as a `Cookie` header from the browser implies.
        let jar = Arc::new(Jar::default());
        for cookies in cookie_strings {
            for pair in cookies
                .split(';')
                .map(str::trim)
                .filter(|p| p.contains('='))
            {
                jar.add_cookie_str(&format!("{pair}; Path=/"), &request.url);
            }
        }

        let mut builder = Client::builder()
            .http1_only()
            .default_headers(headers)
            .cookie_provider(jar)
            .redirect(reqwest::redirect::Policy::limited(10))
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.read_timeout)
            .pool_max_idle_per_host(connections)
            .tcp_nodelay(true);
        if !has_user_agent {
            builder = builder.user_agent(config.user_agent.clone());
        }
        builder = match &request.proxy {
            ProxyConfig::System => builder,
            ProxyConfig::Direct => builder.no_proxy(),
            ProxyConfig::Custom { url, auth } => {
                let mut proxy =
                    reqwest::Proxy::all(url).map_err(|_| Error::InvalidProxy(url.clone()))?;
                if let Some(auth) = auth {
                    proxy = proxy
                        .basic_auth(&auth.username, auth.password.as_deref().unwrap_or_default());
                }
                builder.proxy(proxy)
            }
        };
        let client = builder.build().map_err(|e| Error::Client(Box::new(e)))?;
        Ok(Self {
            client,
            request: Arc::new(request.clone()),
        })
    }

    /// A GET for the request URL with auth applied.
    pub fn get(&self) -> RequestBuilder {
        let mut rb = self.client.get(self.request.url.clone());
        if let Some(auth) = &self.request.auth {
            rb = rb.basic_auth(&auth.username, auth.password.as_deref());
        }
        rb
    }

    /// A GET for another URL with this request's headers, cookies and
    /// proxy. Basic auth is only sent to the request's own origin, so
    /// credentials never leak to a CDN or another host.
    pub fn get_url(&self, url: &Url) -> RequestBuilder {
        let mut rb = self.client.get(url.clone());
        if let Some(auth) = &self.request.auth
            && url.origin() == self.request.url.origin()
        {
            rb = rb.basic_auth(&auth.username, auth.password.as_deref());
        }
        rb
    }

    pub fn url(&self) -> &Url {
        &self.request.url
    }

    /// Key for the per-host connection cap.
    pub fn host_key(&self) -> String {
        host_key(&self.request.url)
    }
}

/// `host:port`, used to key the per-host connection cap.
pub(crate) fn host_key(url: &Url) -> String {
    format!(
        "{}:{}",
        url.host_str().unwrap_or_default(),
        url.port_or_known_default().unwrap_or_default()
    )
}

/// Turns a non-success response into an [`Error::HttpStatus`].
pub(crate) fn status_error(resp: &Response) -> Error {
    Error::HttpStatus {
        status: resp.status().as_u16(),
        retry_after: retry_after(resp.headers()),
    }
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let v = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(secs) = v.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let when = httpdate::parse_http_date(v).ok()?;
    Some(when.duration_since(SystemTime::now()).unwrap_or_default())
}

pub(crate) fn header_str(headers: &HeaderMap, name: HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Parsed `Content-Range: bytes first-last/total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContentRange {
    pub first: u64,
    pub last: u64,
    pub total: Option<u64>,
}

pub(crate) fn content_range(headers: &HeaderMap) -> Option<ContentRange> {
    let v = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
    parse_content_range(v)
}

fn parse_content_range(v: &str) -> Option<ContentRange> {
    let rest = v.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (first, last) = range.trim().split_once('-')?;
    let first = first.trim().parse().ok()?;
    let last = last.trim().parse().ok()?;
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    (first <= last).then_some(ContentRange { first, last, total })
}

/// The encoding an entity is stored or sent under, when that is not the
/// file's own bytes.
///
/// Byte ranges then count encoded bytes, so neither the offsets nor the total
/// in `Content-Range` describe the file being downloaded. `Content-Encoding`
/// is the plain case. Google Cloud Storage adds `x-goog-stored-content-encoding`
/// for "decompressive transcoding": it gunzips the body on the way out, yet
/// still applies `Range` to the stored (compressed) object and reports the
/// compressed length as the total. A segmented download of such a URL mixes
/// decompressed and compressed bytes into a file that looks complete.
pub(crate) fn stored_encoding(headers: &HeaderMap) -> Option<String> {
    ["content-encoding", "x-goog-stored-content-encoding"]
        .into_iter()
        .filter_map(|name| headers.get(name)?.to_str().ok())
        .map(str::trim)
        .find(|v| !v.is_empty() && !v.eq_ignore_ascii_case("identity"))
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_encoding_spots_an_encoded_entity() {
        let encoded = |name: &str, value: &str| {
            let mut h = HeaderMap::new();
            h.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
            stored_encoding(&h)
        };
        // Google Cloud Storage decompresses the body on the way out but still
        // counts `Range` against the stored, compressed object.
        assert_eq!(
            encoded("x-goog-stored-content-encoding", "gzip"),
            Some("gzip".into())
        );
        assert_eq!(encoded("content-encoding", "GZIP"), Some("gzip".into()));
        assert_eq!(encoded("content-encoding", "br"), Some("br".into()));
        // Neither of these means the bytes are anything but the file's own.
        assert_eq!(encoded("content-encoding", "identity"), None);
        assert_eq!(encoded("x-goog-stored-content-encoding", "identity"), None);
        assert_eq!(encoded("content-encoding", "  "), None);
        assert_eq!(stored_encoding(&HeaderMap::new()), None);
    }

    #[test]
    fn content_range_parsing() {
        assert_eq!(
            parse_content_range("bytes 0-0/1234"),
            Some(ContentRange {
                first: 0,
                last: 0,
                total: Some(1234)
            })
        );
        assert_eq!(
            parse_content_range("bytes 100-199/*"),
            Some(ContentRange {
                first: 100,
                last: 199,
                total: None
            })
        );
        assert_eq!(parse_content_range("bytes */1234"), None);
        assert_eq!(parse_content_range("bytes 5-4/10"), None);
        assert_eq!(parse_content_range("items 0-1/2"), None);
    }

    #[test]
    fn retry_after_seconds_and_dates() {
        let mut h = HeaderMap::new();
        h.insert(header::RETRY_AFTER, HeaderValue::from_static("7"));
        assert_eq!(retry_after(&h), Some(Duration::from_secs(7)));
        h.insert(
            header::RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retry_after(&h), Some(Duration::ZERO)); // in the past
        h.insert(header::RETRY_AFTER, HeaderValue::from_static("soon"));
        assert_eq!(retry_after(&h), None);
    }
}
