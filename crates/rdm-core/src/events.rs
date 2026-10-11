//! Progress and state events emitted by a running download.

use std::path::PathBuf;
use std::time::Duration;

use crate::probe::ProbeInfo;
use crate::segments::SegmentInfo;

/// Identifies a download within one [`Engine`](crate::Engine).
pub type DownloadId = u64;

/// Something that happened to a download.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// The resource was probed and the output file is ready.
    Started {
        path: PathBuf,
        info: Box<ProbeInfo>,
        /// Bytes already on disk from an earlier run.
        resumed_from: u64,
        /// Whether the download uses multiple connections and can be resumed.
        segmented: bool,
    },
    /// Periodic progress report.
    Progress(Progress),
    /// Lifecycle change.
    State(State),
    /// A request failed and will be retried after `delay`.
    Retrying {
        attempt: u32,
        delay: Duration,
        reason: String,
    },
    /// Something the user should know about that does not stop the download,
    /// e.g. a fallback to a single connection or a restart from zero.
    Warning(String),
}

/// Lifecycle states of a download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Probing,
    Downloading,
    Paused,
    Completed,
    /// Stopped with progress saved; starting it again resumes.
    Stopped,
    /// Stopped and the partial file deleted.
    Cancelled,
    Failed(String),
}

/// Point-in-time progress of a download.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// Bytes written to disk, including bytes from earlier runs.
    pub downloaded: u64,
    pub total: Option<u64>,
    /// Smoothed transfer rate in bytes per second.
    pub speed: u64,
    pub eta: Option<Duration>,
    /// Connections currently transferring data.
    pub connections: usize,
    /// Unfinished segments, ordered by offset.
    pub segments: Vec<SegmentInfo>,
}

/// How a download ended (when it did not fail).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Completed {
        path: PathBuf,
        size: u64,
        /// Bytes that were already on disk when this run started.
        resumed_from: u64,
        elapsed: Duration,
    },
    /// Stopped by [`DownloadHandle::stop`](crate::DownloadHandle::stop);
    /// progress is saved next to `path`.
    Stopped {
        path: Option<PathBuf>,
        downloaded: u64,
    },
    Cancelled,
}
