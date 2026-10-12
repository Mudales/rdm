//! Optional helper programs rdm downloads on request: yt-dlp (finds the
//! videos on web pages) and Deno (the JavaScript runtime yt-dlp needs for
//! YouTube). Each comes from its project's GitHub releases, is checked
//! against the SHA-256 the release publishes, and is unpacked into its own
//! folder under rdm's tools directory.

use std::io::Read;
use std::path::{Path, PathBuf};

use rdm_core::{Destination, DownloadSpec, Engine, Outcome, Request};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// A tool rdm can install, as shown in the consent dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSource {
    pub name: &'static str,
    /// GitHub `owner/repo`.
    pub repo: &'static str,
    /// Release asset (a zip).
    pub asset: &'static str,
    #[serde(skip)]
    checksums: Checksums,
    pub publisher: &'static str,
    pub license: &'static str,
    pub size_mb: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Checksums {
    /// A `SHA2-256SUMS`-style file listing every asset.
    SumsFile(&'static str),
    /// `<asset>.sha256sum` next to the asset.
    Sidecar,
}

/// The yt-dlp build for this system: the unpacked ("onedir") variant,
/// which starts much faster than the single-file one.
pub fn ytdlp_source() -> Option<ToolSource> {
    let (asset, size_mb) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", _) => ("yt-dlp_macos.zip", 54),
        ("windows", "x86_64") => ("yt-dlp_win.zip", 18),
        ("windows", "aarch64") => ("yt-dlp_win_arm64.zip", 21),
        ("linux", "x86_64") => ("yt-dlp_linux.zip", 41),
        ("linux", "aarch64") => ("yt-dlp_linux_aarch64.zip", 40),
        _ => return None,
    };
    Some(ToolSource {
        name: "yt-dlp",
        repo: "yt-dlp/yt-dlp",
        asset,
        checksums: Checksums::SumsFile("SHA2-256SUMS"),
        publisher: "the yt-dlp project (github.com/yt-dlp/yt-dlp)",
        license: "Unlicense (public domain)",
        size_mb,
    })
}

/// The Deno build for this system.
pub fn deno_source() -> Option<ToolSource> {
    let (asset, size_mb) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => ("deno-aarch64-apple-darwin.zip", 39),
        ("macos", "x86_64") => ("deno-x86_64-apple-darwin.zip", 43),
        ("windows", "x86_64") => ("deno-x86_64-pc-windows-msvc.zip", 43),
        ("windows", "aarch64") => ("deno-aarch64-pc-windows-msvc.zip", 41),
        ("linux", "x86_64") => ("deno-x86_64-unknown-linux-gnu.zip", 42),
        ("linux", "aarch64") => ("deno-aarch64-unknown-linux-gnu.zip", 40),
        _ => return None,
    };
    Some(ToolSource {
        name: "Deno",
        repo: "denoland/deno",
        asset,
        checksums: Checksums::Sidecar,
        publisher: "the Deno project (github.com/denoland/deno)",
        license: "MIT",
        size_mb,
    })
}

/// The newest release tag of a GitHub repository, read from the redirect
/// of `/releases/latest` (no API rate limits).
pub async fn latest_release(engine: &Engine, repo: &str) -> Result<String> {
    let info = engine
        .probe(&Request::new(&format!(
            "https://github.com/{repo}/releases/latest"
        ))?)
        .await?;
    let tag = info
        .final_url
        .path_segments()
        .and_then(|mut s| s.next_back().map(str::to_owned))
        .unwrap_or_default();
    if tag.is_empty() || tag == "latest" || tag == "releases" {
        return Err(Error::ToolInstall(format!("no release found for {repo}")));
    }
    Ok(tag)
}

/// Downloads, verifies and unpacks a tool into `tools_dir/<dir_name>`,
/// replacing an older copy only once the new one is complete. Returns the
/// release tag installed.
pub async fn install_tool(
    engine: &Engine,
    tools_dir: &Path,
    source: &ToolSource,
    dir_name: &str,
) -> Result<String> {
    std::fs::create_dir_all(tools_dir).map_err(Error::io(tools_dir))?;
    let tag = latest_release(engine, source.repo).await?;
    let base = format!("https://github.com/{}/releases/download/{tag}", source.repo);
    let asset_url = format!("{base}/{}", source.asset);

    let (sums_url, entry) = match source.checksums {
        Checksums::SumsFile(file) => (format!("{base}/{file}"), Some(source.asset)),
        Checksums::Sidecar => (format!("{asset_url}.sha256sum"), None),
    };
    let sums_request = Request::new(&sums_url)?;
    let sums = engine
        .fetcher(&sums_request)?
        .get(&sums_request.url, None)
        .await?;
    let expected =
        expected_sha256(&String::from_utf8_lossy(&sums.body), entry).ok_or_else(|| {
            Error::ToolInstall(format!("no SHA-256 for {} in {sums_url}", source.asset))
        })?;

    let zip_path = tools_dir.join(format!("{dir_name}-download.zip"));
    let mut spec = DownloadSpec::new(
        Request::new(&asset_url)?,
        Destination::File(zip_path.clone()),
    );
    spec.overwrite = true;
    match engine.start(spec)?.wait().await? {
        Outcome::Completed { .. } => {}
        _ => {
            return Err(Error::ToolInstall(format!(
                "the {} download was interrupted",
                source.name
            )));
        }
    }

    let target = tools_dir.join(dir_name);
    let zip = zip_path.clone();
    let result = tokio::task::spawn_blocking(move || {
        if sha256_file(&zip)? != expected {
            return Err(Error::ToolInstall(
                "checksum mismatch: the download is damaged or was altered, and was discarded"
                    .into(),
            ));
        }
        unpack_replacing(&zip, &target)
    })
    .await
    .map_err(|e| Error::Task(e.to_string()))?;
    let _ = std::fs::remove_file(&zip_path);
    result?;
    Ok(tag)
}

/// The SHA-256 for `entry` in a sums file (`<hex>  <name>` lines), or the
/// first SHA-256 in the text when there is no entry name (a sidecar file,
/// in any of the formats projects use, including PowerShell's).
pub(crate) fn expected_sha256(text: &str, entry: Option<&str>) -> Option<String> {
    let is_hex = |t: &str| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit());
    match entry {
        Some(name) => text.lines().find_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let file = parts.next()?.trim_start_matches('*');
            (file == name && is_hex(hash)).then(|| hash.to_ascii_lowercase())
        }),
        None => text
            .split(|c: char| !c.is_ascii_alphanumeric())
            .find(|t| is_hex(t))
            .map(str::to_ascii_lowercase),
    }
}

pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).map_err(Error::io(path))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf).map_err(Error::io(path))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Unpacks a zip into `target`, keeping each entry's Unix permissions, then
/// swaps it in for any older copy.
fn unpack_replacing(zip_path: &Path, target: &Path) -> Result<()> {
    let staging = target.with_extension("new");
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(Error::io(&staging))?;
    }
    std::fs::create_dir_all(&staging).map_err(Error::io(&staging))?;
    let file = std::fs::File::open(zip_path).map_err(Error::io(zip_path))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| Error::ToolInstall(format!("bad archive: {e}")))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| Error::ToolInstall(format!("bad archive: {e}")))?;
        // `enclosed_name` rejects absolute paths and `..` (zip-slip).
        let Some(rel) = entry.enclosed_name() else {
            return Err(Error::ToolInstall(
                "the archive contains an unsafe path".into(),
            ));
        };
        let out = staging.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(Error::io(&out))?;
            continue;
        }
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir).map_err(Error::io(dir))?;
        }
        let mut dest = std::fs::File::create(&out).map_err(Error::io(&out))?;
        std::io::copy(&mut entry, &mut dest).map_err(Error::io(&out))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = entry.unix_mode().unwrap_or(0o644) & 0o777;
            // Top-level files are the programs: always executable.
            let mode = if rel.components().count() == 1 {
                mode | 0o755
            } else {
                mode | 0o444
            };
            std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode))
                .map_err(Error::io(&out))?;
        }
    }
    let old = target.with_extension("old");
    if old.exists() {
        std::fs::remove_dir_all(&old).map_err(Error::io(&old))?;
    }
    if target.exists() {
        std::fs::rename(target, &old).map_err(Error::io(target))?;
    }
    std::fs::rename(&staging, target).map_err(Error::io(target))?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// The program inside an unpacked tool folder: a top-level file whose name
/// starts with `prefix`.
pub(crate) fn find_program(dir: &Path, prefix: &str) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(prefix) && !n.ends_with(".txt"))
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn checksum_files_in_every_format() {
        let sums = "07e5  short\n\
                    32e72032766bef9199d99d15beb69fd52e46df8f8b06f0d8745db59e04d339e9  yt-dlp_linux.zip\n\
                    07E54B0865303C864006925913BCE2604F8EE8CC6F18699BAC9C309F9328A6D8 *yt-dlp_macos.zip\n";
        assert_eq!(
            expected_sha256(sums, Some("yt-dlp_macos.zip")).as_deref(),
            Some("07e54b0865303c864006925913bce2604f8ee8cc6f18699bac9c309f9328a6d8")
        );
        assert_eq!(expected_sha256(sums, Some("yt-dlp_win.zip")), None);
        let powershell = "\nAlgorithm : SHA256\nHash      : A0C3101B4158D1DFB7D6A78A7BF0F3DE80C96BB423C152BEEC8BEB22786F2238\nPath      : C:\\a\\deno.zip\n";
        assert_eq!(
            expected_sha256(powershell, None).as_deref(),
            Some("a0c3101b4158d1dfb7d6a78a7bf0f3de80c96bb423c152beec8beb22786f2238")
        );
        let plain = "5cd46d6268f6f78f5d88bdc7159d20bd44cdaa4b3303474839f87ec6fe7ae25c  deno.zip\n";
        assert!(expected_sha256(plain, None).is_some());
    }

    #[test]
    fn unpacking_replaces_the_old_copy_and_rejects_unsafe_paths() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("t.zip");
        let write = |entries: &[(&str, &[u8])]| {
            let mut z = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
            for (name, data) in entries {
                z.start_file(*name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                z.write_all(data).unwrap();
            }
            z.finish().unwrap();
        };
        let target = dir.path().join("yt-dlp");
        write(&[("yt-dlp_macos", b"v1"), ("_internal/lib.py", b"x")]);
        unpack_replacing(&zip_path, &target).unwrap();
        write(&[("yt-dlp_macos", b"v2")]);
        unpack_replacing(&zip_path, &target).unwrap();
        assert_eq!(std::fs::read(target.join("yt-dlp_macos")).unwrap(), b"v2");
        assert!(!target.join("_internal").exists(), "old files are gone");
        assert_eq!(
            find_program(&target, "yt-dlp"),
            Some(target.join("yt-dlp_macos"))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(target.join("yt-dlp_macos"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "the program is executable");
        }
        write(&[("../escape", b"no")]);
        assert!(unpack_replacing(&zip_path, &target).is_err());
        assert!(!dir.path().join("escape").exists());
        assert_eq!(std::fs::read(target.join("yt-dlp_macos")).unwrap(), b"v2");
    }

    #[test]
    fn sources_exist_for_desktop_systems() {
        if cfg!(any(
            target_os = "macos",
            target_os = "windows",
            target_os = "linux"
        )) && matches!(std::env::consts::ARCH, "x86_64" | "aarch64")
        {
            assert!(ytdlp_source().is_some());
            assert!(deno_source().is_some());
        }
    }
}
