//! Which browser downloads rdm takes over.
//!
//! The rules live in the app's settings; the browser extension fetches them
//! and applies the same logic (`extension/lib/capture.js`). Both are tested
//! against the shared cases in `extension/test/capture-cases.json`.

use serde::{Deserialize, Serialize};

/// A named group of file extensions, for the settings UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTypeGroup {
    pub id: &'static str,
    pub name: &'static str,
    pub extensions: &'static [&'static str],
}

/// Known file types, grouped. Users can add any other extension.
pub const FILE_TYPES: &[FileTypeGroup] = &[
    FileTypeGroup {
        id: "video",
        name: "Video",
        extensions: &[
            "mp4", "mkv", "webm", "avi", "mov", "wmv", "flv", "m4v", "mpg", "mpeg", "3gp", "ogv",
            "ts", "m2ts", "vob", "rm", "rmvb", "asf",
        ],
    },
    FileTypeGroup {
        id: "audio",
        name: "Audio",
        extensions: &[
            "mp3", "m4a", "aac", "flac", "wav", "ogg", "oga", "opus", "wma", "aiff", "alac", "mka",
        ],
    },
    FileTypeGroup {
        id: "archives",
        name: "Archives",
        extensions: &[
            "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "zst", "lz", "lzma", "cab", "arj",
        ],
    },
    FileTypeGroup {
        id: "programs",
        name: "Programs",
        extensions: &[
            "exe", "msi", "msix", "msu", "appx", "apk", "aab", "deb", "rpm", "appimage", "pkg",
            "jar", "flatpak", "snap",
        ],
    },
    FileTypeGroup {
        id: "diskImages",
        name: "Disk images",
        extensions: &["iso", "img", "dmg", "vhd", "vhdx", "vmdk", "qcow2"],
    },
    FileTypeGroup {
        id: "documents",
        name: "Documents",
        extensions: &[
            "pdf", "epub", "mobi", "djvu", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt",
            "ods", "odp", "rtf", "csv", "txt",
        ],
    },
    FileTypeGroup {
        id: "images",
        name: "Images",
        extensions: &[
            "jpg", "jpeg", "png", "gif", "webp", "avif", "svg", "bmp", "tif", "tiff", "heic", "psd",
        ],
    },
    FileTypeGroup {
        id: "other",
        name: "Other",
        extensions: &["torrent", "bin", "dat", "srt", "vtt", "ttf", "otf"],
    },
];

/// Groups captured in full by default.
const DEFAULT_GROUPS: &[&str] = &["video", "audio", "archives", "programs", "diskImages"];
/// Single extensions captured by default from other groups.
const DEFAULT_EXTRA: &[&str] = &["pdf", "epub", "mobi", "djvu"];

/// The extensions captured by default.
pub fn default_extensions() -> Vec<String> {
    FILE_TYPES
        .iter()
        .filter(|g| DEFAULT_GROUPS.contains(&g.id))
        .flat_map(|g| g.extensions.iter())
        .chain(DEFAULT_EXTRA)
        .map(|e| (*e).to_owned())
        .collect()
}

/// Rules for taking over browser downloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CaptureSettings {
    /// Lower-case extensions without the dot; `*` matches any characters
    /// (`r0*` for split archives).
    pub extensions: Vec<String>,
    /// Smaller files (when the size is known) stay in the browser.
    pub min_size: Option<u64>,
    /// Hosts whose downloads stay in the browser. `example.com` covers its
    /// subdomains too.
    pub excluded_sites: Vec<String>,
    /// URL patterns whose downloads stay in the browser. `*` matches any
    /// characters; a pattern without `*` matches URLs that start with it.
    pub excluded_addresses: Vec<String>,
    /// Also take over files of these types that the browser would display
    /// in a tab (PDFs, images, videos) instead of downloading.
    pub capture_inline: bool,
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self {
            extensions: default_extensions(),
            min_size: None,
            excluded_sites: Vec::new(),
            excluded_addresses: Vec::new(),
            capture_inline: false,
        }
    }
}

/// Why a download is or is not taken over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "camelCase")]
pub enum CaptureDecision {
    Capture,
    /// The file type is not in the list (`extension` is empty when the
    /// name has none).
    NotSelected {
        extension: String,
    },
    ExcludedSite,
    ExcludedAddress,
    TooSmall,
}

impl CaptureSettings {
    /// Normalizes the lists (lower case, no dots or blanks, no duplicates;
    /// sites reduced to host names). Fails on unusable entries.
    pub fn validated(mut self) -> Result<Self, String> {
        self.extensions = dedup(self.extensions.iter().map(|e| {
            e.trim()
                .trim_start_matches("*.")
                .trim_start_matches('.')
                .to_ascii_lowercase()
        }));
        if let Some(bad) = self
            .extensions
            .iter()
            .find(|e| e.contains(['/', '\\', ' ', '?', '#']))
        {
            return Err(format!("`{bad}` is not a file extension"));
        }
        self.excluded_sites = dedup(self.excluded_sites.iter().map(|s| site_host(s)));
        if let Some(bad) = self
            .excluded_sites
            .iter()
            .find(|s| s.contains(['/', ' ', '*']))
        {
            return Err(format!("`{bad}` is not a site name"));
        }
        self.excluded_addresses =
            dedup(self.excluded_addresses.iter().map(|a| a.trim().to_owned()));
        if self.min_size == Some(0) {
            self.min_size = None;
        }
        Ok(self)
    }

    /// Whether a download of `url` (named `file_name` by the server, when
    /// known) of `size` bytes should be taken over.
    pub fn decide(&self, url: &str, file_name: Option<&str>, size: Option<u64>) -> CaptureDecision {
        let host = rdm_core::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default();
        if self
            .excluded_sites
            .iter()
            .any(|site| host_matches(&host, site))
        {
            return CaptureDecision::ExcludedSite;
        }
        let lower_url = url.to_ascii_lowercase();
        if self.excluded_addresses.iter().any(|p| {
            let p = p.to_ascii_lowercase();
            if p.contains('*') {
                glob(&p, &lower_url)
            } else {
                lower_url.starts_with(&p)
            }
        }) {
            return CaptureDecision::ExcludedAddress;
        }
        let extension = file_name
            .filter(|n| !n.trim().is_empty())
            .map_or_else(|| url_extension(url), name_extension);
        if extension.is_empty() || !self.selects_extension(&extension) {
            return CaptureDecision::NotSelected { extension };
        }
        if let (Some(min), Some(size)) = (self.min_size, size)
            && size < min
        {
            return CaptureDecision::TooSmall;
        }
        CaptureDecision::Capture
    }

    /// Whether this (lower-case) extension is in the list.
    pub fn selects_extension(&self, ext: &str) -> bool {
        self.extensions.iter().any(|e| glob(e, ext))
    }

    /// Whether `host`'s downloads are taken over.
    pub fn captures_site(&self, host: &str) -> bool {
        let host = site_host(host);
        !self.excluded_sites.iter().any(|s| host_matches(&host, s))
    }
}

/// `example.com` matches itself and its subdomains.
fn host_matches(host: &str, site: &str) -> bool {
    !site.is_empty()
        && (host == site
            || host
                .strip_suffix(site)
                .is_some_and(|rest| rest.ends_with('.')))
}

/// A host name from what a user typed: `*.x.com`, `https://x.com/a`, `X.com:8080`.
pub(crate) fn site_host(input: &str) -> String {
    let s = input.trim().to_ascii_lowercase();
    let s = s.split_once("://").map_or(s.as_str(), |(_, rest)| rest);
    let s = s.split(['/', '?', '#']).next().unwrap_or_default();
    let s = s.rsplit_once('@').map_or(s, |(_, host)| host);
    let s = if s.starts_with('[') {
        s.split_inclusive(']').next().unwrap_or_default() // IPv6 literal
    } else {
        s.split(':').next().unwrap_or_default()
    };
    s.trim_start_matches("*.").trim_matches('.').to_owned()
}

/// Lower-case extension of a file name, or "".
pub(crate) fn name_extension(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default();
    match name.rfind('.') {
        Some(dot) if dot > 0 => name[dot + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Lower-case extension of the URL's last path segment, or "".
pub(crate) fn url_extension(url: &str) -> String {
    rdm_core::Url::parse(url)
        .ok()
        .and_then(|u| u.path_segments()?.next_back().map(str::to_owned))
        .map(|last| name_extension(&last))
        .unwrap_or_default()
}

/// `*` matches any run of characters (including none). Linear time.
pub(crate) fn glob(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

fn dedup(items: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items
        .filter(|s| !s.is_empty() && seen.insert(s.clone()))
        .collect()
}

/// A file type the browser downloaded without rdm, offered in the settings
/// so it can be added with one click.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedType {
    pub extension: String,
    pub count: u32,
    /// Unix seconds.
    pub last_seen: i64,
    /// Dismissed by the user: no longer offered.
    #[serde(default)]
    pub ignored: bool,
}

/// At most this many skipped types are remembered.
pub(crate) const MAX_SKIPPED: usize = 40;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn globbing() {
        for (p, t, want) in [
            ("zip", "zip", true),
            ("zip", "zipx", false),
            ("r0*", "r00", true),
            ("r0*", "rar", false),
            ("*", "", true),
            ("a*b*c", "aXXbYYc", true),
            ("a*b*c", "aXXbYY", false),
            ("https://x.org/*/raw/*", "https://x.org/u/raw/f.zip", true),
        ] {
            assert_eq!(glob(p, t), want, "{p} ~ {t}");
        }
    }

    #[test]
    fn site_names_are_normalized() {
        for (input, want) in [
            ("Example.COM", "example.com"),
            ("*.update.microsoft.com", "update.microsoft.com"),
            ("https://user@dl.x.org:8443/path?q", "dl.x.org"),
            (".x.org.", "x.org"),
        ] {
            assert_eq!(site_host(input), want);
        }
    }

    #[test]
    fn validation_normalizes() {
        let s = CaptureSettings {
            extensions: vec![".MP4".into(), "*.zip".into(), "mp4".into(), " ".into()],
            min_size: Some(0),
            excluded_sites: vec!["https://A.com/x".into(), "a.com".into()],
            excluded_addresses: vec!["  https://x/*  ".into()],
            capture_inline: false,
        }
        .validated()
        .unwrap();
        assert_eq!(s.extensions, ["mp4", "zip"]);
        assert_eq!(s.excluded_sites, ["a.com"]);
        assert_eq!(s.excluded_addresses, ["https://x/*"]);
        assert_eq!(s.min_size, None);
        let bad = CaptureSettings {
            extensions: vec!["a b".into()],
            ..CaptureSettings::default()
        };
        assert!(bad.validated().is_err());
    }

    #[test]
    fn defaults_cover_common_downloads_only() {
        let d = default_extensions();
        for yes in ["mp4", "zip", "exe", "iso", "pdf", "dmg"] {
            assert!(d.iter().any(|e| e == yes), "{yes}");
        }
        for no in ["txt", "jpg", "csv", "html"] {
            assert!(!d.iter().any(|e| e == no), "{no}");
        }
        assert_eq!(d.len(), dedup(d.clone().into_iter()).len(), "no duplicates");
    }

    /// The same cases run against `extension/lib/capture.js`.
    #[test]
    fn shared_cases() {
        let cases: Value =
            serde_json::from_str(include_str!("../../../extension/test/capture-cases.json"))
                .unwrap();
        let rules: CaptureSettings = serde_json::from_value(cases["rules"].clone()).unwrap();
        let rules = rules.validated().unwrap();
        let defaults: Vec<String> =
            serde_json::from_value(cases["defaultExtensions"].clone()).unwrap();
        assert_eq!(
            defaults,
            default_extensions(),
            "the extension's DEFAULT_RULES"
        );
        for case in cases["cases"].as_array().unwrap() {
            let url = case["url"].as_str().unwrap();
            let got = rules.decide(url, case["fileName"].as_str(), case["size"].as_u64());
            let got = serde_json::to_value(&got).unwrap();
            assert_eq!(got["decision"], case["expect"], "{case}");
            if let Some(ext) = case.get("extension") {
                assert_eq!(&got["extension"], ext, "{case}");
            }
        }
    }
}
