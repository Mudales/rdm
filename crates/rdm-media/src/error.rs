//! Error type for streaming downloads.

use std::path::{Path, PathBuf};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error(transparent)]
    Core(#[from] rdm_core::Error),

    /// The playlist or MPD could not be understood.
    #[error("invalid {what}: {reason}")]
    Parse { what: &'static str, reason: String },

    /// DRM or encryption rdm deliberately does not handle.
    #[error("protected content ({0}); rdm does not download DRM-protected media")]
    Protected(String),

    #[error("live streams are not supported yet")]
    Live,

    #[error("the stream has no variant `{0}`")]
    NoSuchVariant(String),

    #[error("the stream has no playable variants")]
    Empty,

    #[error("not a streaming playlist (expected HLS or DASH)")]
    NotAStream,

    #[error("{0}")]
    Unsupported(String),

    #[error("could not decrypt segment: {0}")]
    Decrypt(String),

    /// The requested output needs ffmpeg, which was not found. Downloaded
    /// segments are kept; finishing resumes once ffmpeg is available.
    #[error("ffmpeg is required to {0}, but it was not found")]
    FfmpegMissing(&'static str),

    #[error("yt-dlp is needed to find the video on this page; install it from Settings → Media")]
    YtDlpMissing,

    #[error("no video was found on this page")]
    NoVideoFound,

    #[error("yt-dlp: {0}")]
    YtDlp(String),

    #[error("could not install the tool: {0}")]
    ToolInstall(String),

    #[error("ffmpeg failed: {0}")]
    Ffmpeg(String),

    #[error("I/O error on {}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("background task failed: {0}")]
    Task(String),
}

impl Error {
    pub(crate) fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
        move |source| Error::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    pub(crate) fn parse(what: &'static str, reason: impl Into<String>) -> Self {
        Error::Parse {
            what,
            reason: reason.into(),
        }
    }
}
