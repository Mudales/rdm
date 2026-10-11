//! Public configuration and request types.

use std::fmt;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::time::Duration;

use reqwest::Url;

use crate::error::{Error, Result};
use crate::retry::RetryPolicy;

/// Hard upper bound on connections per download.
pub const MAX_CONNECTIONS: usize = 32;

/// Engine-wide settings shared by all downloads.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Sent when the request does not carry its own `User-Agent`.
    pub user_agent: String,
    /// Cap on simultaneous connections to one host across all downloads.
    /// This keeps rdm polite no matter how many downloads target a server.
    pub max_connections_per_host: NonZeroUsize,
    /// Total bandwidth cap across all downloads.
    pub global_speed_limit: Option<NonZeroU64>,
    pub connect_timeout: Duration,
    /// Maximum silence on an open connection before it is considered dead.
    pub read_timeout: Duration,
    /// How often resume metadata is made durable.
    pub checkpoint_interval: Duration,
    /// How often [`Event::Progress`](crate::Event::Progress) is emitted.
    pub progress_interval: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            user_agent: format!("rdm/{}", crate::VERSION),
            max_connections_per_host: NonZeroUsize::new(16).unwrap_or(NonZeroUsize::MIN),
            global_speed_limit: None,
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(30),
            checkpoint_interval: Duration::from_secs(2),
            progress_interval: Duration::from_millis(500),
        }
    }
}

/// Username and optional password for HTTP Basic authentication.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: Option<String>,
}

impl Credentials {
    pub fn new(username: impl Into<String>, password: Option<String>) -> Self {
        Self {
            username: username.into(),
            password,
        }
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Which proxy to use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProxyConfig {
    /// The operating system's proxy settings and `HTTP(S)_PROXY` variables.
    #[default]
    System,
    /// Connect directly, ignoring any system proxy.
    Direct,
    /// An explicit `http://`, `https://`, `socks5://` or `socks5h://` proxy.
    Custom {
        url: String,
        auth: Option<Credentials>,
    },
}

/// What to fetch and how to authenticate.
#[derive(Clone)]
pub struct Request {
    pub url: Url,
    /// Extra headers such as `Referer`. Headers the engine controls
    /// (`Range`, `If-Range`, `Accept-Encoding`, `Host`, ...) are ignored.
    pub headers: Vec<(String, String)>,
    /// A `Cookie` header value (`a=1; b=2`), e.g. forwarded from a browser.
    pub cookies: Option<String>,
    pub auth: Option<Credentials>,
    pub proxy: ProxyConfig,
}

impl Request {
    /// Parses `url`; only `http` and `https` are accepted.
    pub fn new(url: &str) -> Result<Self> {
        let parsed = Url::parse(url).map_err(|e| Error::InvalidUrl {
            url: url.to_owned(),
            reason: e.to_string(),
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(Error::InvalidUrl {
                url: url.to_owned(),
                reason: format!("unsupported scheme `{}`", parsed.scheme()),
            });
        }
        Ok(Self {
            url: parsed,
            headers: Vec::new(),
            cookies: None,
            auth: None,
            proxy: ProxyConfig::System,
        })
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn referer(self, referer: impl Into<String>) -> Self {
        self.header("Referer", referer)
    }

    pub fn user_agent(self, ua: impl Into<String>) -> Self {
        self.header("User-Agent", ua)
    }

    pub fn cookies(mut self, cookies: impl Into<String>) -> Self {
        self.cookies = Some(cookies.into());
        self
    }

    pub fn basic_auth(mut self, credentials: Credentials) -> Self {
        self.auth = Some(credentials);
        self
    }

    pub fn proxy(mut self, proxy: ProxyConfig) -> Self {
        self.proxy = proxy;
        self
    }
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(k, v)| {
                let sensitive = ["authorization", "cookie", "proxy-authorization"]
                    .iter()
                    .any(|s| k.eq_ignore_ascii_case(s));
                (
                    k.as_str(),
                    if sensitive { "<redacted>" } else { v.as_str() },
                )
            })
            .collect();
        f.debug_struct("Request")
            .field("url", &self.url.as_str())
            .field("headers", &headers)
            .field("cookies", &self.cookies.as_ref().map(|_| "<redacted>"))
            .field("auth", &self.auth)
            .field("proxy", &self.proxy)
            .finish()
    }
}

/// Where the downloaded file goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// Exactly this path.
    File(PathBuf),
    /// This directory, under the server-suggested file name. If that name is
    /// taken by an unrelated file, ` (1)`, ` (2)`, ... is appended.
    Directory(PathBuf),
}

/// A download job.
#[derive(Debug, Clone)]
pub struct DownloadSpec {
    pub request: Request,
    pub destination: Destination,
    /// Starting number of connections (clamped to [`MAX_CONNECTIONS`]).
    pub connections: NonZeroUsize,
    /// Ranges are never split into pieces smaller than this.
    pub min_split: u64,
    /// Bandwidth cap for this download alone.
    pub speed_limit: Option<NonZeroU64>,
    pub retry: RetryPolicy,
    /// With [`Destination::File`], replace an existing file that is not an
    /// rdm partial download instead of failing.
    pub overwrite: bool,
}

impl DownloadSpec {
    /// Defaults: 8 connections, 1 MiB minimum split, no speed limit.
    pub fn new(request: Request, destination: Destination) -> Self {
        Self {
            request,
            destination,
            connections: NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN),
            min_split: 1 << 20,
            speed_limit: None,
            retry: RetryPolicy::default(),
            overwrite: false,
        }
    }

    pub(crate) fn connection_count(&self) -> usize {
        self.connections.get().min(MAX_CONNECTIONS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_urls() {
        assert!(Request::new("ftp://example.com/x").is_err());
        assert!(Request::new("not a url").is_err());
        assert!(Request::new("https://example.com/x").is_ok());
    }

    #[test]
    fn debug_redacts_secrets() {
        let r = Request::new("https://example.com/")
            .unwrap()
            .cookies("session=s3cret")
            .header("Authorization", "Bearer t0ken")
            .basic_auth(Credentials::new("me", Some("hunter2".into())));
        let dbg = format!("{r:?}");
        for secret in ["s3cret", "t0ken", "hunter2"] {
            assert!(!dbg.contains(secret), "{dbg}");
        }
    }
}
