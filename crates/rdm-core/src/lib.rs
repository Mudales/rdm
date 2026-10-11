//! `rdm-core` — the rdm download engine.
//!
//! A standalone library with no GUI, browser-extension or ffmpeg
//! dependencies. Every fallible operation returns a [`Result`].
//!
//! # Overview
//!
//! * [`Engine`] holds engine-wide state: the global speed limit and a
//!   per-host connection cap shared by all downloads.
//! * [`Engine::start`] takes a [`DownloadSpec`] and returns a
//!   [`DownloadHandle`] to pause, resume, stop, cancel or throttle the
//!   download and to [`subscribe`](DownloadHandle::subscribe) to [`Event`]s.
//! * Downloads are split dynamically: a connection that finishes its range
//!   takes over half of the largest range still in progress, down to
//!   [`DownloadSpec::min_split`].
//! * All connections write into one file at the right offsets. Progress is
//!   kept in a `<file>.rdm` sidecar so a killed process resumes where it
//!   left off, after checking that the server's file has not changed.
//! * Servers that ignore ranges or omit the size get a single,
//!   non-resumable stream.
//!
//! ```no_run
//! # async fn demo() -> rdm_core::Result<()> {
//! use rdm_core::{Destination, DownloadSpec, Engine, EngineConfig, Request};
//!
//! let engine = Engine::new(EngineConfig::default());
//! let request = Request::new("https://example.com/big.iso")?;
//! let spec = DownloadSpec::new(request, Destination::Directory("downloads".into()));
//! let handle = engine.start(spec)?;
//! let outcome = handle.wait().await?;
//! println!("{outcome:?}");
//! # Ok(()) }
//! ```

mod config;
mod download;
mod engine;
mod error;
mod events;
mod http;
mod probe;
mod ranges;
mod retry;
mod segments;
mod sidecar;
mod throttle;
mod writer;

pub use config::{
    Credentials, Destination, DownloadSpec, EngineConfig, MAX_CONNECTIONS, ProxyConfig, Request,
};
pub use engine::{DownloadControl, DownloadHandle, Engine};
pub use error::{Error, Result};
pub use events::{DownloadId, Event, Outcome, Progress, State};
pub use probe::{ProbeInfo, sanitize_filename};
pub use ranges::RangeSet;
pub use reqwest::Url;
pub use retry::RetryPolicy;
pub use segments::SegmentInfo;
pub use sidecar::{PartialDownload, SIDECAR_EXTENSION, inspect_partial, sidecar_path};

/// Version of the engine crate, for diagnostics and the default `User-Agent`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
