//! The `<file>.rdm` resume metadata.
//!
//! Durability rule: the output file's data is `fsync`'d *before* a sidecar
//! listing those ranges is written, and the sidecar is replaced atomically
//! (write temp file, fsync, rename). A crash or power loss can therefore
//! lose recent progress but never mark unwritten bytes as complete.
//!
//! Cookies, headers and credentials are deliberately not stored here.

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::probe::ProbeInfo;
use crate::ranges::RangeSet;

const FORMAT_VERSION: u32 = 1;

/// Extension appended to the destination path.
pub const SIDECAR_EXTENSION: &str = "rdm";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Sidecar {
    pub version: u32,
    pub url: String,
    pub size: Option<u64>,
    pub resumable: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub completed: RangeSet,
}

impl Sidecar {
    pub fn new(url: &str, info: &ProbeInfo) -> Self {
        Self {
            version: FORMAT_VERSION,
            url: url.to_owned(),
            size: info.size,
            resumable: info.resumable,
            etag: info.etag.clone(),
            last_modified: info.last_modified.clone(),
            completed: RangeSet::new(),
        }
    }

    /// Checks whether a partial download described by `self` can continue
    /// against the resource described by `info`. `Err` carries the reason.
    pub fn validate(&self, info: &ProbeInfo) -> std::result::Result<(), String> {
        if self.version != FORMAT_VERSION {
            return Err(format!("unsupported metadata version {}", self.version));
        }
        if !self.resumable || !info.resumable {
            return Err("the server does not support resuming".into());
        }
        if self.size != info.size {
            return Err(format!(
                "size changed from {} to {}",
                fmt_size(self.size),
                fmt_size(info.size)
            ));
        }
        if let (Some(old), Some(new)) = (&self.etag, &info.etag) {
            return if old == new {
                Ok(())
            } else {
                Err(format!("ETag changed from {old} to {new}"))
            };
        }
        if let (Some(old), Some(new)) = (&self.last_modified, &info.last_modified)
            && old != new
        {
            return Err(format!("Last-Modified changed from {old} to {new}"));
        }
        Ok(())
    }
}

fn fmt_size(size: Option<u64>) -> String {
    size.map_or_else(|| "unknown".into(), |s| s.to_string())
}

/// `<dest>.rdm`
pub fn sidecar_path(dest: &Path) -> PathBuf {
    with_suffix(dest, SIDECAR_EXTENSION)
}

fn with_suffix(path: &Path, ext: &str) -> PathBuf {
    let mut s = OsString::from(path.as_os_str());
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

/// Reads a sidecar. `Ok(None)` if it does not exist.
pub(crate) fn load(path: &Path) -> Result<Option<Sidecar>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(path)(e)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| Error::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
        })
}

/// Atomically replaces the sidecar at `path`.
pub(crate) fn store(path: &Path, sidecar: &Sidecar) -> Result<()> {
    let tmp = with_suffix(path, "tmp");
    let json = serde_json::to_vec(sidecar).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other(e),
    })?;
    let mut f = fs::File::create(&tmp).map_err(Error::io(&tmp))?;
    f.write_all(&json).map_err(Error::io(&tmp))?;
    f.sync_all().map_err(Error::io(&tmp))?;
    drop(f);
    fs::rename(&tmp, path).map_err(Error::io(path))?;
    sync_parent_dir(path);
    Ok(())
}

/// Deletes the sidecar (and any leftover temp file). Missing files are fine.
pub(crate) fn remove(path: &Path) -> Result<()> {
    for p in [path.to_path_buf(), with_suffix(path, "tmp")] {
        match fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io(&p)(e)),
        }
    }
    Ok(())
}

/// Makes the rename durable. Best effort: directories cannot be opened for
/// syncing on Windows, where NTFS journals the rename itself.
fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Public view of a partial download, for front ends that want to list or
/// resume downloads from their `.rdm` files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialDownload {
    /// The URL the download was started with.
    pub url: String,
    /// Total size, if known.
    pub size: Option<u64>,
    /// Bytes already safely on disk.
    pub downloaded: u64,
    /// Whether the download can be resumed rather than restarted.
    pub resumable: bool,
}

/// Reads the resume state for `dest` (the downloaded file's path, or the
/// `.rdm` path itself). `Ok(None)` if there is no partial download.
pub fn inspect_partial(dest: &Path) -> Result<Option<PartialDownload>> {
    let path = if dest.extension().is_some_and(|e| e == SIDECAR_EXTENSION) {
        dest.to_path_buf()
    } else {
        sidecar_path(dest)
    };
    Ok(load(&path)?.map(|s| PartialDownload {
        url: s.url,
        size: s.size,
        downloaded: s.completed.covered(),
        resumable: s.resumable,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(size: Option<u64>, etag: Option<&str>, lm: Option<&str>) -> ProbeInfo {
        let url = reqwest::Url::parse("http://example.com/f").unwrap();
        ProbeInfo {
            url: url.clone(),
            final_url: url,
            size,
            resumable: size.is_some(),
            accept_ranges: true,
            content_type: None,
            filename: "f".into(),
            etag: etag.map(Into::into),
            last_modified: lm.map(Into::into),
        }
    }

    #[test]
    fn roundtrip_and_inspect() {
        let dir = std::env::temp_dir().join(format!("rdm-sidecar-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("file.bin");
        let p = sidecar_path(&dest);
        assert_eq!(p, dir.join("file.bin.rdm"));
        assert_eq!(load(&p).unwrap(), None);

        let mut sc = Sidecar::new(
            "http://example.com/f",
            &info(Some(100), Some("\"a\""), None),
        );
        sc.completed.insert(0, 40);
        store(&p, &sc).unwrap();
        assert_eq!(load(&p).unwrap(), Some(sc.clone()));
        let partial = inspect_partial(&dest).unwrap().unwrap();
        assert_eq!(partial.downloaded, 40);
        assert_eq!(inspect_partial(&p).unwrap(), Some(partial));

        remove(&p).unwrap();
        assert_eq!(load(&p).unwrap(), None);
        remove(&p).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn validation() {
        let base = Sidecar::new("u", &info(Some(100), Some("\"a\""), Some("Mon")));
        assert!(
            base.validate(&info(Some(100), Some("\"a\""), Some("Mon")))
                .is_ok()
        );
        // ETag wins over Last-Modified when both sides have one.
        assert!(
            base.validate(&info(Some(100), Some("\"a\""), Some("Tue")))
                .is_ok()
        );
        assert!(
            base.validate(&info(Some(100), Some("\"b\""), Some("Mon")))
                .is_err()
        );
        assert!(
            base.validate(&info(Some(101), Some("\"a\""), Some("Mon")))
                .is_err()
        );
        assert!(base.validate(&info(Some(100), None, Some("Tue"))).is_err());
        assert!(base.validate(&info(Some(100), None, None)).is_ok());
        assert!(base.validate(&info(None, Some("\"a\""), None)).is_err());
    }
}
