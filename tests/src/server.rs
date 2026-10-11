//! The axum mock server.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::faults::{Faults, basic_auth_value};

const CHUNK: usize = 16 * 1024;

/// What the mock server serves and how.
#[derive(Debug, Clone)]
pub struct MockConfig {
    pub size: u64,
    /// Selects the content; changing it changes every byte.
    pub seed: u64,
    /// Honor `Range` requests. When false, always answer 200 with the body.
    pub ranges: bool,
    /// Bytes per second per response (i.e. per connection), if limited.
    pub per_connection_rate: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_disposition: Option<String>,
    /// Send `Content-Length` (otherwise the body is chunked).
    pub content_length: bool,
    pub faults: Faults,
}

impl MockConfig {
    pub fn new(size: u64) -> Self {
        Self {
            size,
            seed: 0,
            ranges: true,
            per_connection_rate: None,
            etag: Some("\"v0\"".into()),
            last_modified: Some("Wed, 01 Jan 2025 00:00:00 GMT".into()),
            content_disposition: None,
            content_length: true,
            faults: Faults::default(),
        }
    }
}

#[derive(Debug, Default)]
struct Stats {
    requests: AtomicU64,
    proxied: AtomicU64,
    bytes_served: AtomicU64,
    active: AtomicUsize,
    peak: AtomicUsize,
    body_faults: AtomicU64,
    /// Byte ranges served, as inclusive `(first, last)`, in request order.
    served_ranges: Mutex<Vec<(u64, u64)>>,
}

#[derive(Debug)]
struct Shared {
    config: RwLock<MockConfig>,
    stats: Stats,
}

impl Shared {
    fn config(&self) -> MockConfig {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// A running mock server. Stops when dropped.
#[derive(Debug)]
pub struct MockServer {
    addr: SocketAddr,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl MockServer {
    /// Starts on an ephemeral localhost port.
    pub async fn start(config: MockConfig) -> io::Result<Self> {
        Self::bind(SocketAddr::from(([127, 0, 0, 1], 0)), config).await
    }

    pub async fn bind(addr: SocketAddr, config: MockConfig) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            config: RwLock::new(config),
            stats: Stats::default(),
        });
        let app = Router::new().fallback(serve).with_state(shared.clone());
        let task = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                eprintln!("mock server stopped: {e}");
            }
        });
        Ok(Self { addr, shared, task })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// URL of the served file.
    pub fn url(&self) -> String {
        format!("http://{}/file.bin", self.addr)
    }

    /// A URL that redirects `hops` times (302) before reaching the file.
    pub fn redirect_url(&self, hops: u32) -> String {
        format!("http://{}/hop/{hops}/file.bin", self.addr)
    }

    /// Changes the configuration for subsequent requests.
    pub fn update(&self, f: impl FnOnce(&mut MockConfig)) {
        f(&mut self
            .shared
            .config
            .write()
            .unwrap_or_else(|e| e.into_inner()));
    }

    pub fn config(&self) -> MockConfig {
        self.shared.config()
    }

    /// Requests received so far.
    pub fn requests(&self) -> u64 {
        self.shared.stats.requests.load(Ordering::Relaxed)
    }

    /// Requests that arrived in proxy (absolute-URI) form.
    pub fn proxied_requests(&self) -> u64 {
        self.shared.stats.proxied.load(Ordering::Relaxed)
    }

    /// Body bytes sent so far.
    pub fn bytes_served(&self) -> u64 {
        self.shared.stats.bytes_served.load(Ordering::Relaxed)
    }

    /// Every body range served so far, as inclusive `(first, last)`, in
    /// request order (full responses appear as `(0, size - 1)`).
    pub fn served_ranges(&self) -> Vec<(u64, u64)> {
        self.shared
            .stats
            .served_ranges
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Highest number of simultaneously streaming responses.
    pub fn peak_connections(&self) -> usize {
        self.shared.stats.peak.load(Ordering::Relaxed)
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn status(code: StatusCode) -> Response {
    code.into_response()
}

async fn serve(
    State(shared): State<Arc<Shared>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let index = shared.stats.requests.fetch_add(1, Ordering::Relaxed);
    if method != Method::GET && method != Method::HEAD {
        return status(StatusCode::METHOD_NOT_ALLOWED);
    }
    let cfg = shared.config();
    let faults = &cfg.faults;

    if uri.authority().is_some() {
        shared.stats.proxied.fetch_add(1, Ordering::Relaxed);
        if let Some((user, pass)) = &faults.require_proxy_auth
            && !header_is(
                &headers,
                header::PROXY_AUTHORIZATION,
                &basic_auth_value(user, pass),
            )
        {
            return Response::builder()
                .status(StatusCode::PROXY_AUTHENTICATION_REQUIRED)
                .header(header::PROXY_AUTHENTICATE, "Basic realm=\"mock\"")
                .body(Body::empty())
                .unwrap_or_default();
        }
    }

    if let Some(latency) = faults.latency {
        tokio::time::sleep(latency).await;
    }

    // `/hop/N/name` redirects to `/hop/N-1/name`; `/hop/0/name` is the file.
    if let Some(rest) = uri.path().strip_prefix("/hop/")
        && let Some((n, name)) = rest.split_once('/')
        && let Ok(n) = n.parse::<u32>()
        && n > 0
    {
        let mut resp = Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, format!("/hop/{}/{name}", n - 1));
        if let Some(cookie) = &faults.redirect_cookie {
            resp = resp.header(header::SET_COOKIE, format!("{cookie}; Path=/"));
        }
        return resp.body(Body::empty()).unwrap_or_default();
    }

    if let Some(err) = faults.error.filter(|e| e.applies_to(index)) {
        let mut resp =
            Response::builder().status(StatusCode::from_u16(err.status).unwrap_or_default());
        if let Some(secs) = err.retry_after {
            resp = resp.header(header::RETRY_AFTER, secs);
        }
        return resp.body(Body::empty()).unwrap_or_default();
    }

    if let Some((user, pass)) = &faults.require_basic_auth
        && !header_is(
            &headers,
            header::AUTHORIZATION,
            &basic_auth_value(user, pass),
        )
    {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::WWW_AUTHENTICATE, "Basic realm=\"mock\"")
            .body(Body::empty())
            .unwrap_or_default();
    }
    if let Some(cookie) = &faults.require_cookie {
        let sent = headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .any(|pair| pair.trim() == cookie);
        if !sent {
            return status(StatusCode::FORBIDDEN);
        }
    }
    for (name, value) in &faults.require_headers {
        let ok = headers
            .get(name.as_str())
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == value);
        if !ok {
            return status(StatusCode::FORBIDDEN);
        }
    }

    let size = cfg.size;
    let ranges_on = cfg.ranges
        && faults
            .ranges_until_request
            .is_none_or(|limit| index < limit);
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .filter(|_| ranges_on && if_range_matches(&headers, &cfg))
        .map(|r| parse_range(r, size));

    let mut resp = Response::builder();
    let (code, first, end) = match range {
        None => (StatusCode::OK, 0, size),
        Some(None) => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                .body(Body::empty())
                .unwrap_or_default();
        }
        Some(Some((a, b))) => {
            let a = a.saturating_sub(faults.range_shift);
            resp = resp.header(header::CONTENT_RANGE, format!("bytes {a}-{b}/{size}"));
            (StatusCode::PARTIAL_CONTENT, a, b + 1)
        }
    };

    resp = resp
        .status(code)
        .header(header::CONTENT_TYPE, "application/octet-stream");
    if cfg.ranges {
        resp = resp.header(header::ACCEPT_RANGES, "bytes");
    }
    if cfg.content_length {
        let claimed = match (code, faults.content_length_override) {
            (StatusCode::OK, Some(len)) => len,
            _ => end - first,
        };
        resp = resp.header(header::CONTENT_LENGTH, claimed);
    }
    for (name, value) in [
        (header::ETAG, &cfg.etag),
        (header::LAST_MODIFIED, &cfg.last_modified),
        (header::CONTENT_DISPOSITION, &cfg.content_disposition),
    ] {
        if let Some(v) = value.as_deref().and_then(|v| HeaderValue::from_str(v).ok()) {
            resp = resp.header(name, v);
        }
    }

    if method == Method::HEAD {
        return resp.body(Body::empty()).unwrap_or_default();
    }
    if end > first {
        shared
            .stats
            .served_ranges
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((first, end - 1));
    }
    let body_fault = (faults.drop_after.is_some() || faults.stall_after.is_some())
        && faults
            .body_fault_limit
            .is_none_or(|limit| shared.stats.body_faults.fetch_add(1, Ordering::Relaxed) < limit);
    let plan = BodyPlan {
        seed: cfg.seed,
        start: first,
        end,
        rate: cfg.per_connection_rate,
        drop_after: faults.drop_after.filter(|_| body_fault),
        stall_after: faults.stall_after.filter(|_| body_fault),
    };
    resp.body(Body::from_stream(body_stream(shared, plan)))
        .unwrap_or_default()
}

fn header_is(headers: &HeaderMap, name: header::HeaderName, want: &str) -> bool {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == want)
}

/// `If-Range` absent, or equal to the current ETag / Last-Modified.
fn if_range_matches(headers: &HeaderMap, cfg: &MockConfig) -> bool {
    match headers.get(header::IF_RANGE).and_then(|v| v.to_str().ok()) {
        None => true,
        Some(v) => cfg.etag.as_deref() == Some(v) || cfg.last_modified.as_deref() == Some(v),
    }
}

/// Parses a single `bytes=` range into an inclusive `(first, last)`.
/// `None` means unsatisfiable. Multi-range requests are treated as the first.
pub(crate) fn parse_range(header: &str, size: u64) -> Option<(u64, u64)> {
    let spec = header
        .trim()
        .strip_prefix("bytes=")?
        .split(',')
        .next()?
        .trim();
    let (a, b) = spec.split_once('-')?;
    let (first, last) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (size.checked_sub(n.min(size))?, size.checked_sub(1)?)
        }
        (a, "") => (a.parse().ok()?, size.checked_sub(1)?),
        (a, b) => (
            a.parse().ok()?,
            b.parse::<u64>().ok()?.min(size.checked_sub(1)?),
        ),
    };
    (first <= last && first < size).then_some((first, last))
}

struct BodyPlan {
    seed: u64,
    start: u64,
    end: u64,
    rate: Option<u64>,
    drop_after: Option<u64>,
    stall_after: Option<(u64, Duration)>,
}

struct StreamState {
    shared: Arc<Shared>,
    plan: BodyPlan,
    pos: u64,
    sent: u64,
    started: Instant,
    stalled: bool,
    finished: bool,
}

impl Drop for StreamState {
    fn drop(&mut self) {
        self.shared.stats.active.fetch_sub(1, Ordering::Relaxed);
    }
}

fn body_stream(
    shared: Arc<Shared>,
    plan: BodyPlan,
) -> impl futures_util::Stream<Item = Result<Bytes, io::Error>> {
    let now = shared.stats.active.fetch_add(1, Ordering::Relaxed) + 1;
    shared.stats.peak.fetch_max(now, Ordering::Relaxed);
    let state = StreamState {
        shared,
        pos: plan.start,
        plan,
        sent: 0,
        started: Instant::now(),
        stalled: false,
        finished: false,
    };
    futures_util::stream::unfold(state, |mut st| async move {
        if st.finished || st.pos >= st.plan.end {
            return None;
        }
        if st.plan.drop_after.is_some_and(|n| st.sent >= n) {
            // An error aborts the connection mid-body.
            st.finished = true;
            let err = io::Error::new(io::ErrorKind::ConnectionAborted, "injected drop");
            return Some((Err(err), st));
        }
        if let Some((at, pause)) = st.plan.stall_after
            && !st.stalled
            && st.sent >= at
        {
            st.stalled = true;
            tokio::time::sleep(pause).await;
            st.started += pause; // the stall does not count against the rate
        }
        if let Some(rate) = st.plan.rate.filter(|r| *r > 0) {
            let due = st.started + Duration::from_secs_f64(st.sent as f64 / rate as f64);
            tokio::time::sleep_until(due).await;
        }

        // Cut chunks so faults trigger at exactly the configured byte.
        let mut len = (st.plan.end - st.pos).min(CHUNK as u64);
        for limit in [
            st.plan.drop_after,
            st.plan
                .stall_after
                .filter(|_| !st.stalled)
                .map(|(at, _)| at),
        ]
        .into_iter()
        .flatten()
        {
            if limit > st.sent {
                len = len.min(limit - st.sent);
            }
        }
        let mut buf = vec![0u8; len as usize];
        crate::fill(st.plan.seed, st.pos, &mut buf);
        st.pos += len;
        st.sent += len;
        st.shared
            .stats
            .bytes_served
            .fetch_add(len, Ordering::Relaxed);
        Some((Ok(Bytes::from(buf)), st))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-0", 10), Some((0, 0)));
        assert_eq!(parse_range("bytes=2-", 10), Some((2, 9)));
        assert_eq!(parse_range("bytes=-3", 10), Some((7, 9)));
        assert_eq!(parse_range("bytes=5-100", 10), Some((5, 9)));
        assert_eq!(parse_range("bytes=10-", 10), None);
        assert_eq!(parse_range("bytes=0-0", 0), None);
        assert_eq!(parse_range("items=0-1", 10), None);
    }
}
