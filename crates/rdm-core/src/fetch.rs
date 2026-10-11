//! In-memory fetching of small resources: playlists, decryption keys and
//! media segments.
//!
//! A [`Fetcher`] reuses one HTTP client (so connections and cookies are
//! shared across hundreds of segment requests) and goes through the same
//! global speed limit, per-host connection cap and retry policy as regular
//! downloads.

use std::ops::Range;
use std::sync::Arc;

use reqwest::{StatusCode, Url, header};

use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::http::{self, HttpClient};
use crate::retry::RetryPolicy;
use crate::throttle::TokenBucket;

/// Default cap on a single fetched body.
pub const DEFAULT_MAX_FETCH: usize = 256 << 20;

/// A fetched resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub body: Vec<u8>,
    /// The URL after redirects (relative URIs in playlists resolve
    /// against this).
    pub final_url: Url,
    pub content_type: Option<String>,
}

/// Fetches resources with one request's headers, cookies, auth and proxy.
/// Cheap to clone; clones share connections and cookies.
#[derive(Debug, Clone)]
pub struct Fetcher {
    client: HttpClient,
    engine: Engine,
    retry: RetryPolicy,
    max_bytes: usize,
    limit: Option<Arc<TokenBucket>>,
}

impl Engine {
    /// A fetcher using `request`'s headers, cookies, auth and proxy.
    pub fn fetcher(&self, request: &crate::Request) -> Result<Fetcher> {
        Ok(Fetcher {
            client: HttpClient::new(self.config(), request, 16)?,
            engine: self.clone(),
            retry: RetryPolicy::default(),
            max_bytes: DEFAULT_MAX_FETCH,
            limit: None,
        })
    }
}

impl Fetcher {
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    pub fn with_max_bytes(mut self, max: usize) -> Self {
        self.max_bytes = max;
        self
    }

    /// Adds a per-job speed limit on top of the global one.
    pub fn with_speed_limit(mut self, limit: Option<std::num::NonZeroU64>) -> Self {
        self.limit = limit.map(|l| Arc::new(TokenBucket::new(Some(l))));
        self
    }

    /// The request's URL.
    pub fn url(&self) -> &Url {
        self.client.url()
    }

    /// GETs `url` (or a byte `range` of it), retrying transient failures.
    pub async fn get(&self, url: &Url, range: Option<Range<u64>>) -> Result<Fetched> {
        let mut attempt = 0;
        loop {
            match self.get_once(url, range.clone()).await {
                Ok(f) => return Ok(f),
                Err(e) if e.is_retryable() && attempt < self.retry.max_retries => {
                    attempt += 1;
                    let delay = self.retry.delay(attempt, e.retry_after());
                    tracing::debug!(%url, attempt, ?delay, "fetch failed, retrying: {e}");
                    tokio::time::sleep(delay).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn get_once(&self, url: &Url, range: Option<Range<u64>>) -> Result<Fetched> {
        let host = self.engine.host_limiter(&http::host_key(url));
        let _permit = host
            .acquire()
            .await
            .map_err(|_| Error::Task("connection limiter closed".into()))?;

        let mut req = self.client.get_url(url);
        if let Some(r) = &range {
            if r.start >= r.end {
                return Ok(Fetched {
                    body: Vec::new(),
                    final_url: url.clone(),
                    content_type: None,
                });
            }
            req = req.header(header::RANGE, format!("bytes={}-{}", r.start, r.end - 1));
        }
        let mut resp = req.send().await.map_err(Error::network)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(http::status_error(&resp));
        }
        // A server that ignores the range sends everything; slice it below.
        let skip = match (&range, status) {
            (Some(r), StatusCode::PARTIAL_CONTENT) => {
                let cr = http::content_range(resp.headers()).ok_or_else(|| {
                    Error::Protocol("206 response without a valid Content-Range".into())
                })?;
                if cr.first != r.start {
                    return Err(Error::Protocol(format!(
                        "requested bytes from {} but the server sent from {}",
                        r.start, cr.first
                    )));
                }
                None
            }
            (Some(r), _) => Some(r.clone()),
            (None, _) => None,
        };

        let final_url = resp.url().clone();
        let content_type = http::header_str(resp.headers(), header::CONTENT_TYPE);
        let mut body = Vec::with_capacity(
            resp.content_length()
                .map_or(64 * 1024, |l| l.min(self.max_bytes as u64) as usize),
        );
        while let Some(chunk) = resp.chunk().await.map_err(Error::network)? {
            if let Some(limit) = &self.limit {
                limit.acquire(chunk.len() as u64).await;
            }
            self.engine
                .global_bucket()
                .acquire(chunk.len() as u64)
                .await;
            if body.len() + chunk.len() > self.max_bytes {
                return Err(Error::Protocol(format!(
                    "{url} is larger than the {} MiB limit for in-memory fetches",
                    self.max_bytes >> 20
                )));
            }
            body.extend_from_slice(&chunk);
        }
        if let Some(r) = skip {
            let start = usize::try_from(r.start).unwrap_or(usize::MAX);
            let end = usize::try_from(r.end).unwrap_or(usize::MAX).min(body.len());
            if start > body.len() {
                return Err(Error::Protocol(format!(
                    "byte range {}-{} is beyond the {}-byte resource",
                    r.start,
                    r.end,
                    body.len()
                )));
            }
            body = body.get(start..end).map(<[u8]>::to_vec).unwrap_or_default();
        } else if let Some(r) = &range {
            let expected = usize::try_from(r.end - r.start).unwrap_or(usize::MAX);
            if body.len() < expected {
                return Err(Error::Truncated {
                    received: body.len() as u64,
                    expected: expected as u64,
                });
            }
        }
        Ok(Fetched {
            body,
            final_url,
            content_type,
        })
    }
}
