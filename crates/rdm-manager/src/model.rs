//! Records, requests and events exchanged with front ends.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use rdm_media::{Manifest, OutputFormat};

use crate::settings::Settings;

/// Download identifier (the database row id).
pub type Id = i64;

/// Where a download is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    /// Waiting for a free slot in its queue.
    Queued,
    Downloading,
    /// Not running; progress is kept and starting again resumes.
    Stopped,
    Completed,
    Failed,
}

impl Status {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Downloading => "downloading",
            Status::Stopped => "stopped",
            Status::Completed => "completed",
            Status::Failed => "failed",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => Status::Queued,
            "downloading" => Status::Downloading,
            "stopped" => Status::Stopped,
            "completed" => Status::Completed,
            "failed" => Status::Failed,
            _ => return None,
        })
    }
}

/// A download as stored and shown in the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadRecord {
    pub id: Id,
    pub url: String,
    pub file_name: String,
    pub directory: PathBuf,
    pub category_id: Option<u32>,
    pub queue_id: u32,
    pub status: Status,
    /// Why it failed, for `Failed`.
    pub error: Option<String>,
    pub size: Option<u64>,
    pub downloaded: u64,
    /// Whether the server supports resuming (known once started).
    pub resumable: Option<bool>,
    pub referer: Option<String>,
    pub user_agent: Option<String>,
    /// Overrides the default connection count.
    pub connections: Option<u8>,
    /// Bytes per second.
    pub speed_limit: Option<u64>,
    /// Unix seconds.
    pub added_at: i64,
    pub completed_at: Option<i64>,
    /// Order within the list and its queue.
    pub position: i64,
    /// Set for HLS/DASH streams: which rendition and output format.
    pub stream: Option<StreamChoice>,
    /// Expected checksum (`sha-256:hex`), from the user or the server.
    pub checksum: Option<String>,
    /// Whether the finished file matched `checksum` (None: not checked yet).
    pub checksum_ok: Option<bool>,
    /// Antivirus scan result, when a scanner is configured.
    pub scan: Option<String>,
}

/// How to download a streaming playlist.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StreamChoice {
    /// Variant id from the manifest; best quality if `None`.
    pub variant: Option<String>,
    /// Audio track id; the default track if `None`.
    pub audio: Option<String>,
    pub format: OutputFormat,
    /// What was chosen, for display ("1080p · 5.0 Mbps").
    pub label: Option<String>,
}

impl DownloadRecord {
    pub fn path(&self) -> PathBuf {
        self.directory.join(&self.file_name)
    }

    pub fn is_finished(&self) -> bool {
        self.status == Status::Completed
    }
}

/// What to do right after adding a download.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StartMode {
    /// Start immediately, regardless of queue limits.
    #[default]
    Now,
    /// Wait for a slot in the queue (runs when the queue is started).
    Queue,
    /// Add stopped.
    Paused,
}

/// A request to add a download. Only `url` is required.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NewDownload {
    pub url: String,
    /// Defaults to the server-suggested name.
    pub file_name: Option<String>,
    /// Defaults to the category's folder.
    pub directory: Option<PathBuf>,
    /// Defaults to the category matching the file extension.
    pub category_id: Option<u32>,
    /// Defaults to the main queue.
    pub queue_id: Option<u32>,
    pub referer: Option<String>,
    pub user_agent: Option<String>,
    /// `Cookie` header value. Kept in memory only, never written to disk.
    pub cookies: Option<String>,
    pub connections: Option<u8>,
    pub speed_limit: Option<u64>,
    pub start: StartMode,
    /// Download as a stream (HLS/DASH). Playlist URLs are detected
    /// automatically; this picks a rendition and format.
    pub stream: Option<StreamChoice>,
    /// Page title, used to name streams (whose URLs say "master.m3u8").
    pub title: Option<String>,
    /// Expected checksum: `sha-256:…`, `md5:…` or bare hex.
    pub checksum: Option<String>,
    /// Add even if the URL (or an identical finished file) is already in
    /// the list.
    pub allow_duplicate: bool,
}

/// Result of probing a URL for the Add dialog.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub url: String,
    pub final_url: String,
    /// Suggested, de-duplicated file name.
    pub file_name: String,
    pub size: Option<u64>,
    pub resumable: bool,
    pub content_type: Option<String>,
    pub category_id: Option<u32>,
    pub directory: PathBuf,
    /// For streaming playlists: the renditions to choose from.
    pub stream: Option<Manifest>,
    /// An existing download of the same URL or file, if any.
    pub duplicate: Option<DownloadRecord>,
    /// A checksum published by the server, verified automatically.
    pub server_checksum: Option<String>,
}

/// Live progress of a running download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveProgress {
    pub id: Id,
    pub downloaded: u64,
    pub total: Option<u64>,
    /// Bytes per second.
    pub speed: u64,
    pub eta_secs: Option<u64>,
    pub connections: usize,
    /// Unfinished segments as `[start, position, end]`; everything else in
    /// `[0, total)` is done.
    pub segments: Vec<[u64; 3]>,
    /// Streams: `[segments done, segments total]`.
    pub stream: Option<[usize; 2]>,
}

/// Runtime view of a queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueState {
    pub id: u32,
    pub name: String,
    pub max_concurrent: u32,
    pub running: bool,
    /// Downloads of this queue currently running.
    pub active: usize,
    /// Downloads waiting in this queue.
    pub waiting: usize,
}

/// Change notifications for front ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "camelCase")]
pub enum ManagerEvent {
    Added(DownloadRecord),
    Updated(DownloadRecord),
    Removed(Id),
    /// Batched every half second while anything is downloading.
    Progress(Vec<LiveProgress>),
    Settings(Box<Settings>),
    Queues(Vec<QueueState>),
    /// Something worth telling the user (e.g. a restart from zero).
    Notice {
        id: Option<Id>,
        message: String,
    },
    /// A running queue ran out of work; the app performs `when_done`.
    #[serde(rename_all = "camelCase")]
    QueueFinished {
        queue_id: u32,
        name: String,
        when_done: crate::settings::WhenDone,
    },
}
