//! Error type for the download engine.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Convenience alias used throughout the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Everything that can go wrong while probing or downloading.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The URL could not be parsed or uses an unsupported scheme.
    #[error("invalid URL `{url}`: {reason}")]
    InvalidUrl { url: String, reason: String },

    /// A user-supplied header name or value is not valid HTTP.
    #[error("invalid header `{0}`")]
    InvalidHeader(String),

    /// The proxy URL could not be parsed or uses an unsupported scheme.
    #[error("invalid proxy `{0}`")]
    InvalidProxy(String),

    /// The HTTP client could not be constructed.
    #[error("failed to build HTTP client")]
    Client(#[source] BoxError),

    /// Connecting, sending or receiving failed at the transport level.
    #[error("network error")]
    Network(#[source] BoxError),

    /// The server answered with an unexpected status code.
    #[error("server returned HTTP {status}")]
    HttpStatus {
        status: u16,
        /// Parsed `Retry-After`, if the server sent one.
        retry_after: Option<Duration>,
    },

    /// The connection closed before the expected number of bytes arrived.
    #[error("connection closed early: received {received} of {expected} bytes")]
    Truncated { received: u64, expected: u64 },

    /// A ranged request was answered with `200 OK` and the full body.
    #[error("server ignored the byte-range request")]
    RangeNotHonored,

    /// The remote resource is no longer the one being downloaded.
    #[error("remote file changed: {0}")]
    ResourceChanged(String),

    /// The server's response violates the protocol in a way we cannot handle.
    #[error("unexpected server response: {0}")]
    Protocol(String),

    /// The destination file exists and is not an rdm partial download.
    #[error("destination already exists: {}", .0.display())]
    DestinationExists(PathBuf),

    /// A filesystem operation failed.
    #[error("I/O error on {}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// `Engine::start` was called outside a Tokio runtime.
    #[error("rdm-core must be used inside a Tokio runtime")]
    NoRuntime,

    /// An internal task failed unexpectedly.
    #[error("download task failed: {0}")]
    Task(String),
}

impl Error {
    /// Whether retrying the same request later may succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Network(_) | Error::Truncated { .. } => true,
            Error::HttpStatus { status, .. } => {
                matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504)
            }
            _ => false,
        }
    }

    /// Server-requested delay before retrying, if any.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Error::HttpStatus { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    pub(crate) fn network(e: reqwest::Error) -> Self {
        Error::Network(Box::new(e))
    }

    pub(crate) fn io(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
        move |source| Error::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    pub(crate) fn task(e: tokio::task::JoinError) -> Self {
        Error::Task(e.to_string())
    }
}
