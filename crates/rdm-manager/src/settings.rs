//! User settings, categories and queues.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The queue every download belongs to unless another is chosen. It cannot
/// be deleted.
pub const MAIN_QUEUE: u32 = 1;

/// Application settings, persisted as one JSON document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Where files go when no category applies.
    pub default_directory: PathBuf,
    /// Connections per download.
    pub connections: u8,
    /// Ranges are never split into pieces smaller than this.
    pub min_split: u64,
    /// Total bandwidth cap in bytes per second.
    pub global_speed_limit: Option<u64>,
    pub proxy: ProxySetting,
    /// Closing the window hides it to the system tray instead of quitting.
    pub close_to_tray: bool,
    /// Offer to download http(s) links to known file types that are copied
    /// to the clipboard.
    pub clipboard_monitoring: bool,
    pub categories: Vec<Category>,
    pub queues: Vec<Queue>,
    /// Command run on every finished download, e.g.
    /// `clamscan --no-summary {file}`. `{file}` is replaced by the path
    /// (appended if absent). Run directly, never through a shell.
    pub antivirus: Option<String>,
}

/// Proxy choice. Credentials may be embedded in a custom URL
/// (`socks5://user:pass@host:1080`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum ProxySetting {
    #[default]
    System,
    Direct,
    Custom {
        url: String,
    },
}

/// A file category with its own destination folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Category {
    pub id: u32,
    pub name: String,
    /// Lower-case extensions without the dot. A category with none is the
    /// catch-all for files no other category claims.
    pub extensions: Vec<String>,
    pub directory: PathBuf,
}

/// A download queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Queue {
    pub id: u32,
    pub name: String,
    /// Downloads the queue runs at once.
    pub max_concurrent: u32,
    #[serde(default)]
    pub schedule: Schedule,
    /// What happens when the running queue has nothing left to do.
    #[serde(default)]
    pub when_done: WhenDone,
}

/// When a queue starts and stops by itself (local time).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Schedule {
    pub enabled: bool,
    /// `HH:MM`, 24-hour.
    pub start: Option<String>,
    /// `HH:MM`; stops the queue (downloads go back in line).
    pub stop: Option<String>,
    /// ISO weekdays, 1 = Monday … 7 = Sunday. Empty means every day.
    pub days: Vec<u8>,
    /// Run once, then disable the schedule.
    pub once: bool,
}

/// Action when a queue finishes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WhenDone {
    #[default]
    Nothing,
    /// Quit rdm.
    Quit,
    /// Shut the computer down (after a cancellable countdown in the app).
    Shutdown,
}

const DEFAULT_CATEGORIES: [(&str, &str); 6] = [
    (
        "Video",
        "mp4 mkv webm avi mov wmv flv m4v mpg mpeg 3gp ts m2ts vob",
    ),
    ("Music", "mp3 m4a aac flac wav ogg oga opus wma aiff alac"),
    (
        "Documents",
        "pdf doc docx xls xlsx ppt pptx odt ods odp txt rtf csv epub md",
    ),
    ("Archives", "zip rar 7z tar gz tgz bz2 xz zst iso dmg img"),
    (
        "Programs",
        "exe msi msix apk deb rpm appimage pkg jar bat sh",
    ),
    ("Other", ""),
];

impl Default for Settings {
    fn default() -> Self {
        let downloads = dirs::download_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
            .unwrap_or_else(|| PathBuf::from("Downloads"));
        Self::with_directory(&downloads)
    }
}

impl Settings {
    /// Defaults with every folder under `downloads`.
    pub fn with_directory(downloads: &Path) -> Self {
        let categories = DEFAULT_CATEGORIES
            .iter()
            .zip(1..)
            .map(|(&(name, exts), id)| Category {
                id,
                name: name.into(),
                extensions: exts.split_whitespace().map(Into::into).collect(),
                directory: downloads.join(name),
            })
            .collect();
        Self {
            default_directory: downloads.to_path_buf(),
            connections: 8,
            min_split: 1 << 20,
            global_speed_limit: None,
            proxy: ProxySetting::System,
            close_to_tray: true,
            clipboard_monitoring: true,
            categories,
            queues: vec![Queue {
                id: MAIN_QUEUE,
                name: "Main".into(),
                max_concurrent: 3,
                schedule: Schedule::default(),
                when_done: WhenDone::Nothing,
            }],
            antivirus: None,
        }
    }

    /// Checks the settings and returns them normalized (extensions lower-
    /// cased without dots, duplicates removed).
    pub fn validated(mut self) -> Result<Self, String> {
        if !(1..=rdm_core::MAX_CONNECTIONS as u8).contains(&self.connections) {
            return Err(format!(
                "connections must be between 1 and {}",
                rdm_core::MAX_CONNECTIONS
            ));
        }
        if self.min_split < 64 * 1024 {
            return Err("minimum split size must be at least 64 KiB".into());
        }
        if self.global_speed_limit == Some(0) {
            self.global_speed_limit = None;
        }
        if let ProxySetting::Custom { url } = &self.proxy {
            let ok = ["http://", "https://", "socks5://", "socks5h://"]
                .iter()
                .any(|s| url.starts_with(s));
            if !ok {
                return Err(
                    "proxy URL must start with http://, https://, socks5:// or socks5h://".into(),
                );
            }
        }

        let mut ids = HashSet::new();
        for c in &mut self.categories {
            c.name = c.name.trim().to_owned();
            if c.name.is_empty() {
                return Err("category names cannot be empty".into());
            }
            if !ids.insert(c.id) {
                return Err(format!("duplicate category id {}", c.id));
            }
            let mut seen = HashSet::new();
            c.extensions = c
                .extensions
                .iter()
                .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
                .filter(|e| !e.is_empty() && seen.insert(e.clone()))
                .collect();
        }

        let mut ids = HashSet::new();
        for q in &mut self.queues {
            q.name = q.name.trim().to_owned();
            if q.name.is_empty() {
                return Err("queue names cannot be empty".into());
            }
            if q.max_concurrent == 0 {
                return Err(format!("queue `{}` must run at least one download", q.name));
            }
            if !ids.insert(q.id) {
                return Err(format!("duplicate queue id {}", q.id));
            }
            let s = &mut q.schedule;
            for time in [&mut s.start, &mut s.stop] {
                *time = time
                    .take()
                    .map(|t| t.trim().to_owned())
                    .filter(|t| !t.is_empty());
                if let Some(t) = time
                    && crate::schedule::parse_clock(t).is_none()
                {
                    return Err(format!(
                        "queue `{}`: `{t}` is not a time like 07:30",
                        q.name
                    ));
                }
            }
            if s.days.iter().any(|d| !(1..=7).contains(d)) {
                return Err(format!(
                    "queue `{}`: days must be 1 (Monday) to 7 (Sunday)",
                    q.name
                ));
            }
            s.days.sort_unstable();
            s.days.dedup();
            if s.enabled && s.start.is_none() && s.stop.is_none() {
                return Err(format!(
                    "queue `{}`: a schedule needs a start or stop time",
                    q.name
                ));
            }
        }
        self.antivirus = self
            .antivirus
            .take()
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty());
        if let Some(cmd) = &self.antivirus
            && crate::antivirus::split_command(cmd).is_empty()
        {
            return Err("the antivirus command is empty".into());
        }
        if !ids.contains(&MAIN_QUEUE) {
            return Err("the main queue cannot be removed".into());
        }
        Ok(self)
    }

    /// The category a file name belongs to: the first whose extensions
    /// match, else the first catch-all category.
    pub fn category_for(&self, file_name: &str) -> Option<&Category> {
        let ext = Path::new(file_name)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        ext.and_then(|ext| self.categories.iter().find(|c| c.extensions.contains(&ext)))
            .or_else(|| self.categories.iter().find(|c| c.extensions.is_empty()))
    }

    pub fn category(&self, id: u32) -> Option<&Category> {
        self.categories.iter().find(|c| c.id == id)
    }

    pub fn queue(&self, id: u32) -> Option<&Queue> {
        self.queues.iter().find(|q| q.id == id)
    }

    /// Destination folder for a category (or the default folder).
    pub fn directory_for(&self, category: Option<u32>) -> PathBuf {
        category
            .and_then(|id| self.category(id))
            .map_or_else(|| self.default_directory.clone(), |c| c.directory.clone())
    }

    /// Whether `text` is a single http(s) URL whose file extension belongs
    /// to a category (catch-all categories do not count). Used to decide
    /// whether a copied link looks like a download.
    pub fn is_download_url(&self, text: &str) -> bool {
        let text = text.trim();
        if text.is_empty() || text.len() > 4096 || text.contains(char::is_whitespace) {
            return false;
        }
        let Ok(url) = rdm_core::Url::parse(text) else {
            return false;
        };
        if !matches!(url.scheme(), "http" | "https") {
            return false;
        }
        let Some(ext) = url
            .path_segments()
            .and_then(|mut s| s.next_back())
            .and_then(|last| Path::new(last).extension())
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
        else {
            return false;
        };
        self.categories.iter().any(|c| c.extensions.contains(&ext))
    }

    pub(crate) fn proxy_config(&self) -> rdm_core::ProxyConfig {
        match &self.proxy {
            ProxySetting::System => rdm_core::ProxyConfig::System,
            ProxySetting::Direct => rdm_core::ProxyConfig::Direct,
            ProxySetting::Custom { url } => rdm_core::ProxyConfig::Custom {
                url: url.clone(),
                auth: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_by_extension() {
        let s = Settings::with_directory(Path::new("/dl"));
        let name = |f: &str| s.category_for(f).map(|c| c.name.as_str());
        assert_eq!(name("movie.MKV"), Some("Video"));
        assert_eq!(name("song.flac"), Some("Music"));
        assert_eq!(name("backup.tar.gz"), Some("Archives"));
        assert_eq!(name("README"), Some("Other"));
        assert_eq!(name("thing.unknownext"), Some("Other"));
        let video = s.category_for("a.mp4").map(|c| c.id);
        assert_eq!(s.directory_for(video), Path::new("/dl/Video"));
        assert_eq!(s.directory_for(None), Path::new("/dl"));
    }

    #[test]
    fn validation_normalizes_and_rejects() {
        let mut s = Settings::with_directory(Path::new("/dl"));
        s.categories[0].extensions = vec![".MP4".into(), "mp4".into(), " ".into()];
        s.global_speed_limit = Some(0);
        let v = s.clone().validated().unwrap();
        assert_eq!(v.categories[0].extensions, ["mp4"]);
        assert_eq!(v.global_speed_limit, None);

        let mut bad = s.clone();
        bad.connections = 0;
        assert!(bad.validated().is_err());
        let mut bad = s.clone();
        bad.queues.clear();
        assert!(bad.validated().is_err());
        let mut bad = s.clone();
        bad.proxy = ProxySetting::Custom {
            url: "ftp://x".into(),
        };
        assert!(bad.validated().is_err());
        let mut bad = s;
        bad.queues[0].max_concurrent = 0;
        assert!(bad.validated().is_err());
    }

    #[test]
    fn download_url_detection() {
        let s = Settings::with_directory(Path::new("/dl"));
        for yes in [
            "https://example.com/files/setup.exe",
            "  http://x.org/a/b/movie.MKV?token=1#t  ",
            "https://cdn.example.com/archive.tar.gz",
        ] {
            assert!(s.is_download_url(yes), "{yes}");
        }
        for no in [
            "https://example.com/",
            "https://example.com/page.html",
            "https://example.com/readme",
            "ftp://example.com/file.zip",
            "file:///etc/passwd.zip",
            "see https://example.com/file.zip",
            "https://example.com/a.zip\nhttps://example.com/b.zip",
            "not a url.zip",
        ] {
            assert!(!s.is_download_url(no), "{no}");
        }
    }

    #[test]
    fn serde_fills_missing_fields_with_defaults() {
        let s: Settings = serde_json::from_str(r#"{"connections": 4}"#).unwrap();
        assert_eq!(s.connections, 4);
        assert_eq!(s.queues.len(), 1);
        let json = serde_json::to_string(&ProxySetting::Custom {
            url: "http://p:1".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"mode":"custom","url":"http://p:1"}"#);
    }
}
