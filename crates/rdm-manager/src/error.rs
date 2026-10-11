//! Error type for the manager.

use std::path::{Path, PathBuf};

use crate::model::Id;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error(transparent)]
    Engine(#[from] rdm_core::Error),

    #[error(transparent)]
    Media(#[from] rdm_media::Error),

    #[error("database error")]
    Database(#[from] rusqlite::Error),

    #[error("stored data is invalid: {0}")]
    Corrupt(String),

    #[error("download {0} not found")]
    NotFound(Id),

    #[error("queue {0} not found")]
    QueueNotFound(u32),

    #[error("invalid settings: {0}")]
    InvalidSettings(String),

    #[error("`{0}` is not a checksum (use sha-256:…, sha-1:…, md5:… or sha-512:…)")]
    InvalidChecksum(String),

    /// The URL (or an identical finished file) is already in the list.
    #[error("already in the list as “{}”", .0.file_name)]
    Duplicate(Box<crate::model::DownloadRecord>),

    #[error("I/O error on {}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl Error {
    pub(crate) fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
        move |source| Error::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// `outer: inner: innermost`, for messages shown to users.
pub fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut s = e.to_string();
    let mut source = e.source();
    while let Some(inner) = source {
        let msg = inner.to_string();
        if !s.ends_with(&msg) {
            s.push_str(": ");
            s.push_str(&msg);
        }
        source = inner.source();
    }
    s
}
