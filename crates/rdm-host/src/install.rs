//! Registers `rdm-host` with browsers.
//!
//! Each browser looks for a JSON manifest naming the host executable and the
//! extensions allowed to start it:
//!
//! | Browser | macOS | Linux | Windows |
//! |---|---|---|---|
//! | Chrome | `~/Library/Application Support/Google/Chrome/NativeMessagingHosts` | `~/.config/google-chrome/NativeMessagingHosts` | `HKCU\Software\Google\Chrome\NativeMessagingHosts` |
//! | Chromium | `…/Chromium/NativeMessagingHosts` | `~/.config/chromium/NativeMessagingHosts` | `HKCU\Software\Chromium\NativeMessagingHosts` |
//! | Edge | `…/Microsoft Edge/NativeMessagingHosts` | `~/.config/microsoft-edge/NativeMessagingHosts` | `HKCU\Software\Microsoft\Edge\NativeMessagingHosts` |
//! | Brave | `…/BraveSoftware/Brave-Browser/NativeMessagingHosts` | `~/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts` | `HKCU\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts` |
//! | Firefox | `~/Library/Application Support/Mozilla/NativeMessagingHosts` | `~/.mozilla/native-messaging-hosts` | `HKCU\Software\Mozilla\NativeMessagingHosts` |
//!
//! On Windows the manifest file lives under `%LOCALAPPDATA%\rdm` and a
//! registry value points at it. Everything is per user; nothing needs admin
//! rights.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{CHROME_EXTENSION_IDS, FIREFOX_EXTENSION_ID, HOST_NAME};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("cannot determine the home directory")]
    NoHome,
    #[error("unknown browser `{0}` (use chrome, chromium, edge, brave or firefox)")]
    UnknownBrowser(String),
    #[error("I/O error on {}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Browser {
    Chrome,
    Chromium,
    Edge,
    Brave,
    Firefox,
}

impl Browser {
    pub const ALL: [Browser; 5] = [
        Browser::Chrome,
        Browser::Chromium,
        Browser::Edge,
        Browser::Brave,
        Browser::Firefox,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Browser::Chrome => "Chrome",
            Browser::Chromium => "Chromium",
            Browser::Edge => "Edge",
            Browser::Brave => "Brave",
            Browser::Firefox => "Firefox",
        }
    }

    pub fn parse(s: &str) -> Result<Self, Error> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "chrome" | "google-chrome" => Browser::Chrome,
            "chromium" => Browser::Chromium,
            "edge" | "msedge" | "microsoft-edge" => Browser::Edge,
            "brave" => Browser::Brave,
            "firefox" => Browser::Firefox,
            _ => return Err(Error::UnknownBrowser(s.into())),
        })
    }

    fn is_firefox(self) -> bool {
        self == Browser::Firefox
    }
}

/// Install state of one browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserStatus {
    pub browser: Browser,
    pub name: &'static str,
    /// The browser seems to be installed (its profile folder exists).
    pub detected: bool,
    /// A manifest for this host exists.
    pub installed: bool,
    /// The installed manifest points at this `rdm-host` executable.
    pub current: bool,
    pub manifest_path: PathBuf,
}

/// Writes and removes host manifests.
#[derive(Debug, Clone)]
pub struct Installer {
    home: PathBuf,
    host_path: PathBuf,
    chrome_ids: Vec<String>,
    firefox_id: String,
    /// Registry path prefix under HKCU (tests use a private one).
    #[cfg(windows)]
    registry_base: String,
}

impl Installer {
    /// For the current user, registering `host_path`.
    pub fn new(host_path: PathBuf) -> Result<Self, Error> {
        let home = dirs::home_dir().ok_or(Error::NoHome)?;
        Ok(Self::with_home(home, host_path))
    }

    /// Uses `home` as the user's home directory (and, on Windows, a private
    /// registry prefix derived from it) — for tests.
    pub fn with_home(home: PathBuf, host_path: PathBuf) -> Self {
        Self {
            home,
            host_path,
            chrome_ids: CHROME_EXTENSION_IDS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            firefox_id: FIREFOX_EXTENSION_ID.to_owned(),
            #[cfg(windows)]
            registry_base: "Software".to_owned(),
        }
    }

    /// Adds Chrome-family extension IDs (e.g. a store listing's).
    pub fn allow_chrome_extension(&mut self, id: impl Into<String>) {
        let id = id.into();
        if !self.chrome_ids.contains(&id) {
            self.chrome_ids.push(id);
        }
    }

    pub fn set_firefox_extension(&mut self, id: impl Into<String>) {
        self.firefox_id = id.into();
    }

    /// Windows only: put registry keys under `HKCU\<base>\…` instead of
    /// `HKCU\Software\…`.
    #[cfg(windows)]
    pub fn set_registry_base(&mut self, base: impl Into<String>) {
        self.registry_base = base.into();
    }

    pub fn host_path(&self) -> &Path {
        &self.host_path
    }

    /// The manifest contents for `browser`.
    pub fn manifest(&self, browser: Browser) -> serde_json::Value {
        let mut m = serde_json::json!({
            "name": HOST_NAME,
            "description": "rdm download manager",
            "path": self.host_path,
            "type": "stdio",
        });
        if browser.is_firefox() {
            m["allowed_extensions"] = serde_json::json!([self.firefox_id]);
        } else {
            let origins: Vec<String> = self
                .chrome_ids
                .iter()
                .map(|id| format!("chrome-extension://{id}/"))
                .collect();
            m["allowed_origins"] = serde_json::json!(origins);
        }
        m
    }

    /// Where the manifest file for `browser` goes.
    pub fn manifest_path(&self, browser: Browser) -> PathBuf {
        let file = format!("{HOST_NAME}.json");
        #[cfg(target_os = "macos")]
        {
            let base = self.home.join("Library/Application Support");
            let dir = match browser {
                Browser::Chrome => "Google/Chrome",
                Browser::Chromium => "Chromium",
                Browser::Edge => "Microsoft Edge",
                Browser::Brave => "BraveSoftware/Brave-Browser",
                Browser::Firefox => "Mozilla",
            };
            base.join(dir).join("NativeMessagingHosts").join(file)
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let dir = match browser {
                Browser::Chrome => ".config/google-chrome/NativeMessagingHosts",
                Browser::Chromium => ".config/chromium/NativeMessagingHosts",
                Browser::Edge => ".config/microsoft-edge/NativeMessagingHosts",
                Browser::Brave => ".config/BraveSoftware/Brave-Browser/NativeMessagingHosts",
                Browser::Firefox => ".mozilla/native-messaging-hosts",
            };
            self.home.join(dir).join(file)
        }
        #[cfg(windows)]
        {
            self.home
                .join("AppData")
                .join("Local")
                .join("rdm")
                .join("NativeMessagingHosts")
                .join(browser.name().to_ascii_lowercase())
                .join(file)
        }
    }

    /// Whether the browser appears to be installed for this user.
    pub fn detected(&self, browser: Browser) -> bool {
        #[cfg(unix)]
        {
            // The folder containing NativeMessagingHosts is the profile root.
            self.manifest_path(browser)
                .parent()
                .and_then(Path::parent)
                .is_some_and(Path::is_dir)
        }
        #[cfg(windows)]
        {
            let local = self.home.join("AppData").join("Local");
            let roaming = self.home.join("AppData").join("Roaming");
            match browser {
                Browser::Chrome => local.join(r"Google\Chrome\User Data"),
                Browser::Chromium => local.join(r"Chromium\User Data"),
                Browser::Edge => local.join(r"Microsoft\Edge\User Data"),
                Browser::Brave => local.join(r"BraveSoftware\Brave-Browser\User Data"),
                Browser::Firefox => roaming.join(r"Mozilla\Firefox"),
            }
            .is_dir()
        }
    }

    /// Registers the host for `browser`. Returns the manifest path.
    pub fn install(&self, browser: Browser) -> Result<PathBuf, Error> {
        let path = self.manifest_path(browser);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io(dir))?;
        }
        let json = serde_json::to_vec_pretty(&self.manifest(browser))
            .map_err(|e| io(&path)(std::io::Error::other(e)))?;
        std::fs::write(&path, json).map_err(io(&path))?;
        #[cfg(windows)]
        for key in self.registry_keys(browser) {
            use winreg::RegKey;
            use winreg::enums::HKEY_CURRENT_USER;
            let reg_path = PathBuf::from(format!(r"HKCU\{key}"));
            let (k, _) = RegKey::predef(HKEY_CURRENT_USER)
                .create_subkey(&key)
                .map_err(io(&reg_path))?;
            k.set_value("", &path.to_string_lossy().into_owned())
                .map_err(io(&reg_path))?;
        }
        Ok(path)
    }

    /// Unregisters the host for `browser`. `Ok(false)` if it was not there.
    pub fn uninstall(&self, browser: Browser) -> Result<bool, Error> {
        let path = self.manifest_path(browser);
        let existed = match std::fs::remove_file(&path) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(io(&path)(e)),
        };
        #[cfg(windows)]
        for key in self.registry_keys(browser) {
            use winreg::RegKey;
            use winreg::enums::HKEY_CURRENT_USER;
            match RegKey::predef(HKEY_CURRENT_USER).delete_subkey(&key) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io(&PathBuf::from(format!(r"HKCU\{key}")))(e)),
            }
        }
        Ok(existed)
    }

    pub fn status(&self) -> Vec<BrowserStatus> {
        Browser::ALL
            .into_iter()
            .map(|browser| {
                let manifest_path = self.manifest_path(browser);
                let installed_path = std::fs::read(&manifest_path)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                    .and_then(|v| v["path"].as_str().map(PathBuf::from));
                BrowserStatus {
                    browser,
                    name: browser.name(),
                    detected: self.detected(browser),
                    installed: installed_path.is_some(),
                    current: installed_path.as_deref() == Some(self.host_path.as_path()),
                    manifest_path,
                }
            })
            .collect()
    }

    #[cfg(windows)]
    fn registry_keys(&self, browser: Browser) -> Vec<String> {
        let base = &self.registry_base;
        let sub = |vendor: &str| format!(r"{base}\{vendor}\NativeMessagingHosts\{HOST_NAME}");
        match browser {
            Browser::Chrome => vec![sub(r"Google\Chrome")],
            Browser::Chromium => vec![sub("Chromium")],
            Browser::Edge => vec![sub(r"Microsoft\Edge")],
            Browser::Brave => vec![sub(r"BraveSoftware\Brave-Browser")],
            Browser::Firefox => vec![sub("Mozilla")],
        }
    }
}

/// The `rdm-host` executable next to the current executable (the app and
/// the host ship in the same folder).
pub fn sibling_host_path() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let name = if cfg!(windows) {
        "rdm-host.exe"
    } else {
        "rdm-host"
    };
    Ok(exe.with_file_name(name))
}
