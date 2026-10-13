//! The browser extension, shipped inside the app.
//!
//! Until rdm is listed in the browsers' stores, the extension is loaded
//! "unpacked" from a folder. The app writes that folder itself (one copy for
//! Chrome, Edge, Brave and Chromium, one for Firefox), opens the browser's
//! extensions page and shows the user the two clicks left to do. Because the
//! browser runs the extension from that folder, updating rdm updates the
//! extension too: the folder is rewritten when the app starts, and the
//! extension reloads itself when it learns (from [`Request::Hello`]) that
//! the app ships a newer version.
//!
//! [`Request::Hello`]: rdm_ipc::Request::Hello

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rdm_host::install::Browser;
use rdm_ipc::ExtensionInfo;
use rdm_manager::Manager;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager as _};

/// Tells the UI an extension said hello; the payload is a [`SeenExtension`].
pub const EXTENSION_SEEN: &str = "rdm://extension-seen";

/// Where the extension sits on the user's machine.
pub const FOLDER_NAME: &str = "rdm-browser-extension";

/// Manager key-value entry with the extensions seen, per browser.
const SEEN_KEY: &str = "extension_seen";

macro_rules! files {
    ($($path:literal),* $(,)?) => {
        &[$((
            $path,
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../extension/", $path)) as &[u8],
        )),*]
    };
}

/// Every file of `extension/` except its README and tests.
const FILES: &[(&str, &[u8])] = files![
    "manifest.json",
    "background.js",
    "content.js",
    "popup.html",
    "popup.css",
    "popup.js",
    "lib/capture.js",
    "lib/detect.js",
    "icons/16.png",
    "icons/32.png",
    "icons/48.png",
    "icons/128.png",
];

/// The two builds of the extension, as `scripts/package-extension.py`
/// makes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// Chrome, Edge, Brave, Chromium: keeps the `key` that fixes the
    /// extension ID `rdm-host` allows.
    Chromium,
    /// Firefox: background scripts instead of a service worker.
    Firefox,
}

impl Flavor {
    pub fn of(browser: Browser) -> Self {
        if browser == Browser::Firefox {
            Flavor::Firefox
        } else {
            Flavor::Chromium
        }
    }

    fn dir_name(self) -> &'static str {
        match self {
            Flavor::Chromium => "chromium",
            Flavor::Firefox => "firefox",
        }
    }
}

fn source_manifest() -> serde_json::Value {
    // Valid JSON: the tests write and read it back.
    let (_, bytes) = FILES[0];
    serde_json::from_slice(bytes).unwrap_or_default()
}

/// The version of the extension this app ships.
pub fn bundled_version() -> String {
    source_manifest()["version"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// `manifest.json` for one flavor.
fn manifest(flavor: Flavor) -> serde_json::Value {
    let mut m = source_manifest();
    if let Some(obj) = m.as_object_mut() {
        let (drop_key, drop_background) = match flavor {
            Flavor::Chromium => ("browser_specific_settings", "scripts"),
            Flavor::Firefox => ("key", "service_worker"),
        };
        obj.remove(drop_key);
        if let Some(bg) = obj.get_mut("background").and_then(|b| b.as_object_mut()) {
            bg.remove(drop_background);
        }
    }
    m
}

/// The folder holding both flavors.
pub fn root(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join(FOLDER_NAME))
}

/// The folder the user loads in `browser`.
pub fn folder(root: &Path, flavor: Flavor) -> PathBuf {
    root.join(flavor.dir_name())
}

/// Writes one flavor into `dir`, leaving files that are already current.
pub fn write(dir: &Path, flavor: Flavor) -> std::io::Result<()> {
    for (rel, bytes) in FILES {
        let path = dir.join(rel);
        let manifest_bytes;
        let data: &[u8] = if *rel == "manifest.json" {
            let mut text =
                serde_json::to_vec_pretty(&manifest(flavor)).map_err(std::io::Error::other)?;
            text.push(b'\n');
            manifest_bytes = text;
            &manifest_bytes
        } else {
            bytes
        };
        if std::fs::read(&path).is_ok_and(|current| current == data) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, data)?;
    }
    Ok(())
}

/// The version of the extension in `dir`, if one was written there.
pub fn installed_version(dir: &Path) -> Option<String> {
    let bytes = std::fs::read(dir.join("manifest.json")).ok()?;
    let m: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    m["version"].as_str().map(str::to_owned)
}

/// At startup: brings folders the user set up to this app's version. The
/// extensions loaded from them reload themselves when they next say hello.
pub fn refresh_installed(app: &AppHandle) {
    let Ok(root) = root(app) else { return };
    let bundled = bundled_version();
    for flavor in [Flavor::Chromium, Flavor::Firefox] {
        let dir = folder(&root, flavor);
        match installed_version(&dir) {
            Some(v) if v != bundled => {
                if let Err(e) = write(&dir, flavor) {
                    tracing::warn!("cannot update the extension in {}: {e}", dir.display());
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Extensions that said hello
// ---------------------------------------------------------------------------

/// An extension that reached this app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeenExtension {
    /// "chrome", "edge", "brave", "chromium", "firefox" or "browser".
    pub browser: String,
    pub version: String,
    /// Unix seconds.
    pub last_seen: i64,
}

pub fn seen(manager: &Manager) -> Vec<SeenExtension> {
    let map: BTreeMap<String, SeenExtension> = manager
        .ui_value(SEEN_KEY)
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    map.into_values().collect()
}

/// Records an extension's hello and tells the UI.
pub fn note_hello(app: &AppHandle, manager: &Arc<Manager>, info: &ExtensionInfo) {
    let browser = info
        .browser
        .as_deref()
        .map(|b| b.trim().to_ascii_lowercase())
        .filter(|b| !b.is_empty() && b.len() <= 20 && b.bytes().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "browser".into());
    let entry = SeenExtension {
        browser: browser.clone(),
        version: info.version.chars().take(20).collect(),
        last_seen: unix_now(),
    };
    let mut map: BTreeMap<String, SeenExtension> = seen(manager)
        .into_iter()
        .map(|s| (s.browser.clone(), s))
        .collect();
    map.insert(browser, entry.clone());
    match serde_json::to_string(&map) {
        Ok(json) => {
            if let Err(e) = manager.set_ui_value(SEEN_KEY, &json) {
                tracing::warn!("cannot save the extension's hello: {e}");
            }
        }
        Err(e) => tracing::warn!("cannot save the extension's hello: {e}"),
    }
    let _ = app.emit(EXTENSION_SEEN, entry);
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

// ---------------------------------------------------------------------------
// Opening the browser's extensions page
// ---------------------------------------------------------------------------

/// The page where an unpacked extension is loaded.
pub fn extensions_page(browser: Browser) -> &'static str {
    match browser {
        Browser::Chrome | Browser::Chromium => "chrome://extensions",
        Browser::Edge => "edge://extensions",
        Browser::Brave => "brave://extensions",
        Browser::Firefox => "about:debugging#/runtime/this-firefox",
    }
}

/// Opens `url` in `browser`. Pages like `chrome://extensions` cannot be
/// opened through the system's default handler, so the browser's own
/// executable is started with the address; a browser that is already
/// running opens it in its window.
pub fn open_in_browser(browser: Browser, url: &str) -> std::io::Result<()> {
    let exe = executables(browser)
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{} was not found", browser.name()),
            )
        })?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn()?;
    // The browser hands the address to its running window and exits, or
    // keeps running; either way, reap it.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Where each browser's executable usually is.
fn executables(browser: Browser) -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let app = match browser {
            Browser::Chrome => "Google Chrome",
            Browser::Chromium => "Chromium",
            Browser::Edge => "Microsoft Edge",
            Browser::Brave => "Brave Browser",
            Browser::Firefox => "Firefox",
        };
        let exe = match browser {
            Browser::Firefox => "firefox",
            _ => app,
        };
        let mut roots = vec![PathBuf::from("/Applications")];
        if let Some(home) = dirs_home() {
            roots.push(home.join("Applications"));
        }
        roots
            .into_iter()
            .map(|r| r.join(format!("{app}.app/Contents/MacOS/{exe}")))
            .collect()
    }
    #[cfg(windows)]
    {
        let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
        let bases = [
            env("ProgramFiles"),
            env("ProgramFiles(x86)"),
            env("LOCALAPPDATA"),
        ];
        let rel = match browser {
            Browser::Chrome => r"Google\Chrome\Application\chrome.exe",
            Browser::Chromium => r"Chromium\Application\chrome.exe",
            Browser::Edge => r"Microsoft\Edge\Application\msedge.exe",
            Browser::Brave => r"BraveSoftware\Brave-Browser\Application\brave.exe",
            Browser::Firefox => r"Mozilla Firefox\firefox.exe",
        };
        bases.into_iter().flatten().map(|b| b.join(rel)).collect()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let names: &[&str] = match browser {
            Browser::Chrome => &["google-chrome", "google-chrome-stable"],
            Browser::Chromium => &["chromium", "chromium-browser"],
            Browser::Edge => &["microsoft-edge", "microsoft-edge-stable"],
            Browser::Brave => &["brave-browser", "brave"],
            Browser::Firefox => &["firefox"],
        };
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path)
            .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
            .collect()
    }
}

#[cfg(target_os = "macos")]
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file added to `extension/` must be added to [`FILES`] too.
    #[test]
    fn ships_every_extension_file() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extension");
        let mut on_disk = Vec::new();
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path
                    .strip_prefix(&src)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if path.is_dir() {
                    if rel != "test" {
                        stack.push(path);
                    }
                } else if rel != "README.md" && !rel.ends_with(".DS_Store") {
                    on_disk.push(rel);
                }
            }
        }
        on_disk.sort();
        let mut shipped: Vec<String> = FILES.iter().map(|(p, _)| (*p).to_owned()).collect();
        shipped.sort();
        assert_eq!(on_disk, shipped);
    }

    #[test]
    fn writes_each_flavor() {
        let dir = tempfile::tempdir().unwrap();
        let chromium = folder(dir.path(), Flavor::Chromium);
        let firefox = folder(dir.path(), Flavor::Firefox);
        assert_eq!(installed_version(&chromium), None);
        write(&chromium, Flavor::Chromium).unwrap();
        write(&firefox, Flavor::Firefox).unwrap();
        assert_eq!(installed_version(&chromium), Some(bundled_version()));
        assert!(chromium.join("lib/detect.js").is_file());
        assert!(firefox.join("icons/128.png").is_file());

        let read = |d: &Path| -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(d.join("manifest.json")).unwrap()).unwrap()
        };
        let c = read(&chromium);
        assert!(c["key"].is_string(), "the key pins the ID rdm-host allows");
        assert!(c.get("browser_specific_settings").is_none());
        assert!(c["background"].get("scripts").is_none());
        let f = read(&firefox);
        assert!(f.get("key").is_none());
        assert_eq!(
            f["browser_specific_settings"]["gecko"]["id"],
            rdm_host::FIREFOX_EXTENSION_ID
        );
        assert!(f["background"].get("service_worker").is_none());

        // An old copy is replaced.
        std::fs::write(chromium.join("content.js"), "old").unwrap();
        write(&chromium, Flavor::Chromium).unwrap();
        assert_ne!(std::fs::read(chromium.join("content.js")).unwrap(), b"old");
    }

    #[test]
    fn the_bundled_extension_matches_the_app() {
        assert_eq!(bundled_version(), env!("CARGO_PKG_VERSION"));
    }
}
