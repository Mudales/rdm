//! Test support for rdm: an axum mock HTTP server with deterministic content.
//!
//! The served bytes are a pure function of `(seed, offset)`, so files of any
//! size can be served and verified without keeping them in memory, and a
//! byte written at the wrong offset is detected.
//!
//! Phase 1 covers what the engine's acceptance tests need: byte ranges (on
//! or off), a per-connection rate limit, validators and request statistics.
//! Fault injection (dropped connections, latency, 429/503, wrong lengths)
//! is added in Phase 2.

use std::convert::Infallible;
use std::io::{self, Read};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::Instant;

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
        }
    }
}

#[derive(Debug)]
struct Shared {
    config: RwLock<MockConfig>,
    requests: AtomicU64,
    bytes_served: AtomicU64,
    active: AtomicUsize,
    peak: AtomicUsize,
    /// Byte ranges served, as inclusive `(first, last)`, in request order.
    served_ranges: Mutex<Vec<(u64, u64)>>,
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
            requests: AtomicU64::new(0),
            bytes_served: AtomicU64::new(0),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            served_ranges: Mutex::new(Vec::new()),
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
        self.shared.requests.load(Ordering::Relaxed)
    }

    /// Body bytes sent so far.
    pub fn bytes_served(&self) -> u64 {
        self.shared.bytes_served.load(Ordering::Relaxed)
    }

    /// Every body range served so far, as inclusive `(first, last)`, in
    /// request order (full responses appear as `(0, size - 1)`).
    pub fn served_ranges(&self) -> Vec<(u64, u64)> {
        self.shared
            .served_ranges
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Highest number of simultaneously streaming responses.
    pub fn peak_connections(&self) -> usize {
        self.shared.peak.load(Ordering::Relaxed)
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(State(shared): State<Arc<Shared>>, method: Method, headers: HeaderMap) -> Response {
    shared.requests.fetch_add(1, Ordering::Relaxed);
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let cfg = shared.config();
    let size = cfg.size;

    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .filter(|_| cfg.ranges && if_range_matches(&headers, &cfg))
        .map(|r| parse_range(r, size));

    let mut resp = Response::builder();
    let (status, first, last_excl) = match range {
        None => (StatusCode::OK, 0, size),
        Some(None) => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                .body(Body::empty())
                .unwrap_or_default();
        }
        Some(Some((a, b))) => {
            resp = resp.header(header::CONTENT_RANGE, format!("bytes {a}-{b}/{size}"));
            (StatusCode::PARTIAL_CONTENT, a, b + 1)
        }
    };

    resp = resp
        .status(status)
        .header(header::CONTENT_TYPE, "application/octet-stream");
    if cfg.ranges {
        resp = resp.header(header::ACCEPT_RANGES, "bytes");
    }
    if cfg.content_length {
        resp = resp.header(header::CONTENT_LENGTH, last_excl - first);
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

    if method == Method::GET && last_excl > first {
        shared
            .served_ranges
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((first, last_excl - 1));
    }
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from_stream(body_stream(
            shared,
            cfg.seed,
            first,
            last_excl,
            cfg.per_connection_rate,
        ))
    };
    resp.body(body).unwrap_or_default()
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
fn parse_range(header: &str, size: u64) -> Option<(u64, u64)> {
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

struct StreamState {
    shared: Arc<Shared>,
    seed: u64,
    pos: u64,
    end: u64,
    rate: Option<u64>,
    started: Instant,
    sent: u64,
}

impl StreamState {
    fn new(shared: Arc<Shared>, seed: u64, pos: u64, end: u64, rate: Option<u64>) -> Self {
        let now = shared.active.fetch_add(1, Ordering::Relaxed) + 1;
        shared.peak.fetch_max(now, Ordering::Relaxed);
        Self {
            shared,
            seed,
            pos,
            end,
            rate,
            started: Instant::now(),
            sent: 0,
        }
    }
}

impl Drop for StreamState {
    fn drop(&mut self) {
        self.shared.active.fetch_sub(1, Ordering::Relaxed);
    }
}

fn body_stream(
    shared: Arc<Shared>,
    seed: u64,
    start: u64,
    end: u64,
    rate: Option<u64>,
) -> impl futures_util::Stream<Item = Result<Bytes, Infallible>> {
    let state = StreamState::new(shared, seed, start, end, rate);
    futures_util::stream::unfold(state, |mut st| async move {
        if st.pos >= st.end {
            return None;
        }
        if let Some(rate) = st.rate.filter(|r| *r > 0) {
            let due = st.started + Duration::from_secs_f64(st.sent as f64 / rate as f64);
            tokio::time::sleep_until(due).await;
        }
        let len = (st.end - st.pos).min(CHUNK as u64) as usize;
        let mut buf = vec![0u8; len];
        fill(st.seed, st.pos, &mut buf);
        st.pos += len as u64;
        st.sent += len as u64;
        st.shared
            .bytes_served
            .fetch_add(len as u64, Ordering::Relaxed);
        Some((Ok(Bytes::from(buf)), st))
    })
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// The byte served at `offset` for `seed`.
pub fn content_byte(seed: u64, offset: u64) -> u8 {
    let word = splitmix64((offset >> 3) ^ seed.rotate_left(32));
    (word >> ((offset & 7) * 8)) as u8
}

/// Fills `buf` with the content starting at `offset`.
pub fn fill(seed: u64, offset: u64, buf: &mut [u8]) {
    for (i, b) in buf.iter_mut().enumerate() {
        *b = content_byte(seed, offset + i as u64);
    }
}

/// The full expected content (for small sizes).
pub fn expected_content(seed: u64, size: u64) -> Vec<u8> {
    let mut v = vec![0u8; size as usize];
    fill(seed, 0, &mut v);
    v
}

/// Streams `path` and compares it with the expected content.
/// `Ok(Err(msg))` describes the first difference.
pub fn verify_file(path: &Path, seed: u64, size: u64) -> io::Result<Result<(), String>> {
    let mut f = std::fs::File::open(path)?;
    let actual = f.metadata()?.len();
    if actual != size {
        return Ok(Err(format!("size is {actual}, expected {size}")));
    }
    let mut buf = vec![0u8; 1 << 20];
    let mut want = vec![0u8; 1 << 20];
    let mut offset = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(Ok(()));
        }
        let (got, exp) = (&buf[..n], &mut want[..n]);
        fill(seed, offset, exp);
        if let Some(i) = got.iter().zip(exp.iter()).position(|(a, b)| a != b) {
            return Ok(Err(format!("first mismatch at byte {}", offset + i as u64)));
        }
        offset += n as u64;
    }
}

/// Parses sizes like `512`, `64K`, `1.5M`, `1G`, `10MiB` (binary units).
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let num: f64 = num.parse().ok()?;
    let mult: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        _ => return None,
    };
    (num >= 0.0).then_some((num * mult as f64) as u64)
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

    #[test]
    fn content_is_deterministic_and_seeded() {
        let a = expected_content(0, 64);
        assert_eq!(a, expected_content(0, 64));
        assert_ne!(a, expected_content(1, 64));
        let mut mid = vec![0; 10];
        fill(0, 30, &mut mid);
        assert_eq!(&a[30..40], &mid[..]);
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("512"), Some(512));
        assert_eq!(parse_size("64K"), Some(65536));
        assert_eq!(parse_size("1.5M"), Some(1572864));
        assert_eq!(parse_size("1GiB"), Some(1 << 30));
        assert_eq!(parse_size("3x"), None);
    }
}
