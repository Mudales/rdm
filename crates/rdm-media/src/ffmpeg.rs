//! Finding and running ffmpeg, and (with the user's consent) installing a
//! static build.
//!
//! ffmpeg is only needed to merge separate video and audio tracks, to turn
//! MPEG-TS into MP4, and to extract MP3/M4A audio. Single-track streams are
//! joined without it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use rdm_core::{Destination, DownloadSpec, Engine, Outcome, Request};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::error::{Error, Result};

/// Overrides ffmpeg discovery with an explicit path.
pub const FFMPEG_ENV: &str = "RDM_FFMPEG";

const EXE: &str = if cfg!(windows) {
    "ffmpeg.exe"
} else {
    "ffmpeg"
};

/// How to find ffmpeg.
#[derive(Debug, Clone, Default)]
pub enum FfmpegConfig {
    /// `$RDM_FFMPEG`, then rdm's tools folder, then `PATH` and the usual
    /// install locations (GUI apps on macOS do not inherit the shell PATH).
    #[default]
    Auto,
    Path(PathBuf),
    /// Pretend ffmpeg is absent (tests, or users who want no remuxing).
    Disabled,
}

#[derive(Debug, Clone, Default)]
pub struct Ffmpeg {
    config: FfmpegConfig,
    tools_dir: Option<PathBuf>,
}

impl Ffmpeg {
    pub fn new(config: FfmpegConfig, tools_dir: Option<PathBuf>) -> Self {
        Self { config, tools_dir }
    }

    pub fn disabled() -> Self {
        Self::new(FfmpegConfig::Disabled, None)
    }

    pub fn tools_dir(&self) -> Option<&Path> {
        self.tools_dir.as_deref()
    }

    /// The ffmpeg executable to use, if any.
    pub fn locate(&self) -> Option<PathBuf> {
        match &self.config {
            FfmpegConfig::Disabled => None,
            FfmpegConfig::Path(p) => p.is_file().then(|| p.clone()),
            FfmpegConfig::Auto => {
                if let Some(p) = std::env::var_os(FFMPEG_ENV).filter(|p| !p.is_empty()) {
                    return Some(PathBuf::from(p)).filter(|p| p.is_file());
                }
                let mut dirs: Vec<PathBuf> = self.tools_dir.iter().cloned().collect();
                if let Some(path) = std::env::var_os("PATH") {
                    dirs.extend(std::env::split_paths(&path));
                }
                if cfg!(unix) {
                    dirs.extend(
                        ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from),
                    );
                }
                dirs.into_iter().map(|d| d.join(EXE)).find(|p| p.is_file())
            }
        }
    }

    /// `ffmpeg -version`'s first line.
    pub async fn version(path: &Path) -> Result<String> {
        let out = tokio::process::Command::new(path)
            .arg("-version")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(Error::io(path))?;
        let text = String::from_utf8_lossy(&out.stdout);
        text.lines()
            .next()
            .map(str::to_owned)
            .filter(|_| out.status.success())
            .ok_or_else(|| Error::Ffmpeg(format!("{} does not run", path.display())))
    }

    /// Runs ffmpeg quietly; on failure the error carries its last messages.
    pub(crate) async fn run(path: &Path, args: &[OsString]) -> Result<()> {
        let mut child = tokio::process::Command::new(path)
            .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(Error::io(path))?;
        let mut stderr = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr).await;
        }
        let status = child.wait().await.map_err(Error::io(path))?;
        if status.success() {
            return Ok(());
        }
        let tail: Vec<&str> = stderr.lines().rev().take(6).collect();
        Err(Error::Ffmpeg(
            tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
        ))
    }
}

// ---------------------------------------------------------------------------
// Installing a static build
// ---------------------------------------------------------------------------

/// Where a static ffmpeg build for this platform comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FfmpegSource {
    /// Stable "latest release" URL; redirects to a versioned file.
    pub url: &'static str,
    pub publisher: &'static str,
    pub license: &'static str,
    /// Approximate download size in MB, for the consent prompt.
    pub size_mb: u32,
}

/// Static builds offered for download, or `None` where the user should use
/// their package manager instead. Linux is in that group: the only
/// zip-plus-checksum Linux builds are compiled with `--enable-nonfree`.
pub fn ffmpeg_source() -> Option<FfmpegSource> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some(FfmpegSource {
            url: "https://ffmpeg.martin-riedl.de/redirect/latest/macos/arm64/release/ffmpeg.zip",
            publisher: "Martin Riedl (ffmpeg.martin-riedl.de)",
            license: "GPLv3",
            size_mb: 29,
        }),
        ("macos", "x86_64") => Some(FfmpegSource {
            url: "https://ffmpeg.martin-riedl.de/redirect/latest/macos/amd64/release/ffmpeg.zip",
            publisher: "Martin Riedl (ffmpeg.martin-riedl.de)",
            license: "GPLv3",
            size_mb: 34,
        }),
        ("windows", "x86_64") => Some(FfmpegSource {
            url: "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip",
            publisher: "Gyan Doshi (gyan.dev)",
            license: "GPLv3",
            size_mb: 115,
        }),
        _ => None,
    }
}

/// What to tell users on platforms without a download.
pub fn install_hint() -> &'static str {
    if cfg!(target_os = "linux") {
        "Install ffmpeg with your package manager, e.g. `sudo apt install ffmpeg` or `sudo dnf install ffmpeg`."
    } else if cfg!(target_os = "macos") {
        "Install ffmpeg with Homebrew (`brew install ffmpeg`) or let rdm download it."
    } else {
        "Install ffmpeg (e.g. `winget install ffmpeg`) or let rdm download it."
    }
}

/// Downloads the platform's static build into `tools_dir`, verifies its
/// published SHA-256, and extracts the ffmpeg executable. The caller must
/// have obtained the user's consent first.
pub async fn install_ffmpeg(engine: &Engine, tools_dir: &Path) -> Result<PathBuf> {
    let source = ffmpeg_source().ok_or_else(|| Error::Unsupported(install_hint().into()))?;
    std::fs::create_dir_all(tools_dir).map_err(Error::io(tools_dir))?;

    // Resolve "latest" once, then use that exact versioned file for both
    // the archive and its checksum, so they cannot come from two builds.
    let info = engine.probe(&Request::new(source.url)?).await?;
    let archive_url = info.final_url;
    let checksum_url = rdm_core::Url::parse(&format!("{archive_url}.sha256"))
        .map_err(|e| Error::Unsupported(format!("bad checksum URL: {e}")))?;
    let fetcher = engine.fetcher(&Request::new(archive_url.as_str())?)?;
    let checksum = fetcher.get(&checksum_url, None).await?;
    let expected = String::from_utf8_lossy(&checksum.body)
        .split_whitespace()
        .next()
        .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| Error::Ffmpeg(format!("no SHA-256 found at {checksum_url}")))?;

    let zip_path = tools_dir.join("ffmpeg-download.zip");
    let mut spec = DownloadSpec::new(
        Request::new(archive_url.as_str())?,
        Destination::File(zip_path.clone()),
    );
    spec.overwrite = true;
    match engine.start(spec)?.wait().await? {
        Outcome::Completed { .. } => {}
        _ => return Err(Error::Ffmpeg("the ffmpeg download was interrupted".into())),
    }

    let target = tools_dir.join(EXE);
    let zip_for_task = zip_path.clone();
    let target_for_task = target.clone();
    let result = tokio::task::spawn_blocking(move || {
        verify_and_extract(&zip_for_task, &expected, &target_for_task)
    })
    .await
    .map_err(|e| Error::Task(e.to_string()))?;
    let _ = std::fs::remove_file(&zip_path);
    result?;
    Ffmpeg::version(&target).await?;
    Ok(target)
}

fn verify_and_extract(zip_path: &Path, expected: &str, target: &Path) -> Result<()> {
    let mut file = std::fs::File::open(zip_path).map_err(Error::io(zip_path))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(Error::io(zip_path))?;
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != expected {
        return Err(Error::Ffmpeg(format!(
            "checksum mismatch: expected {expected}, got {actual}; the download was discarded"
        )));
    }

    let file = std::fs::File::open(zip_path).map_err(Error::io(zip_path))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| Error::Ffmpeg(format!("bad archive: {e}")))?;
    let index = (0..archive.len())
        .find(|&i| {
            archive.by_index(i).is_ok_and(|e| {
                e.name().is_ok_and(|name| {
                    let name = name.replace('\\', "/");
                    name == EXE
                        || name.ends_with(&format!("/bin/{EXE}"))
                        || name.ends_with(&format!("/{EXE}"))
                })
            })
        })
        .ok_or_else(|| Error::Ffmpeg(format!("no {EXE} in the archive")))?;
    let mut entry = archive
        .by_index(index)
        .map_err(|e| Error::Ffmpeg(format!("bad archive: {e}")))?;
    let tmp = target.with_extension("partial");
    let mut out = std::fs::File::create(&tmp).map_err(Error::io(&tmp))?;
    std::io::copy(&mut entry, &mut out).map_err(Error::io(&tmp))?;
    drop(out);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(Error::io(&tmp))?;
    }
    std::fs::rename(&tmp, target).map_err(Error::io(target))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_and_explicit_paths() {
        assert_eq!(Ffmpeg::disabled().locate(), None);
        let missing = Ffmpeg::new(FfmpegConfig::Path("/nonexistent/ffmpeg".into()), None);
        assert_eq!(missing.locate(), None);
    }

    #[test]
    fn tools_dir_is_searched_first() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join(EXE);
        std::fs::write(&exe, b"").unwrap();
        // Only meaningful when RDM_FFMPEG is not set in the environment.
        if std::env::var_os(FFMPEG_ENV).is_none() {
            let f = Ffmpeg::new(FfmpegConfig::Auto, Some(dir.path().into()));
            assert_eq!(f.locate(), Some(exe));
        }
    }

    #[test]
    fn extraction_verifies_the_checksum() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("a.zip");
        {
            let mut z = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
            let opts = zip::write::SimpleFileOptions::default();
            z.start_file(format!("ffmpeg-9.0-build/bin/{EXE}"), opts)
                .unwrap();
            z.write_all(b"#!/bin/sh\necho fake\n").unwrap();
            z.finish().unwrap();
        }
        let bytes = std::fs::read(&zip_path).unwrap();
        let good: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let target = dir.path().join(EXE);
        assert!(verify_and_extract(&zip_path, &"0".repeat(64), &target).is_err());
        assert!(!target.exists());
        verify_and_extract(&zip_path, &good, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"#!/bin/sh\necho fake\n");
    }
}
