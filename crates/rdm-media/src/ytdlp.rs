//! Videos on web pages, found by yt-dlp.
//!
//! yt-dlp knows how to read over a thousand sites. rdm uses it only to find
//! the real media addresses (`yt-dlp -J`); rdm's own engine then downloads
//! them: plain files with several connections (in short requests, which
//! servers such as YouTube's serve at full speed), HLS and DASH segments in
//! parallel. Formats rdm cannot fetch itself (RTMP, Smooth Streaming, ...)
//! are left to yt-dlp's downloader.
//!
//! DRM is never worked around: protected formats are dropped, and a video
//! with nothing else is reported as protected content. rdm never passes
//! yt-dlp's `--allow-unplayable-formats`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rdm_core::{ProxyConfig, Request, Url};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

use crate::error::{Error, Result};
use crate::model::{Container, Track};
use crate::tools;

/// Overrides the yt-dlp executable.
pub const YTDLP_ENV: &str = "RDM_YTDLP";
/// Overrides the Deno executable.
pub const DENO_ENV: &str = "RDM_DENO";

/// Folder names under the tools directory.
pub(crate) const YTDLP_DIR: &str = "yt-dlp";
pub(crate) const DENO_DIR: &str = "deno";

/// How long a page may take to be read.
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(120);

/// Locates yt-dlp and Deno: `$RDM_YTDLP` / `$RDM_DENO`, rdm's own copies in
/// the tools folder, then `PATH` and the usual install locations.
#[derive(Debug, Clone, Default)]
pub struct YtDlp {
    tools_dir: Option<PathBuf>,
    disabled: bool,
    /// An explicit program, ahead of every search.
    program: Option<PathBuf>,
}

impl YtDlp {
    pub fn new(tools_dir: Option<PathBuf>) -> Self {
        Self {
            tools_dir,
            disabled: false,
            program: None,
        }
    }

    /// Never finds anything (tests, or a build without web-page support).
    pub fn disabled() -> Self {
        Self {
            tools_dir: None,
            disabled: true,
            program: None,
        }
    }

    /// Uses this yt-dlp executable, with no JavaScript runtime and no cache.
    pub fn at(program: PathBuf) -> Self {
        Self {
            tools_dir: None,
            disabled: false,
            program: Some(program),
        }
    }

    pub fn tools_dir(&self) -> Option<&Path> {
        self.tools_dir.as_deref()
    }

    pub fn locate(&self) -> Option<PathBuf> {
        if self.disabled {
            return None;
        }
        if let Some(p) = &self.program {
            return Some(p.clone()).filter(|p| p.is_file());
        }
        if let Some(p) = std::env::var_os(YTDLP_ENV).filter(|p| !p.is_empty()) {
            return Some(PathBuf::from(p)).filter(|p| p.is_file());
        }
        self.managed_ytdlp().or_else(|| {
            search_path(if cfg!(windows) {
                "yt-dlp.exe"
            } else {
                "yt-dlp"
            })
        })
    }

    pub fn locate_deno(&self) -> Option<PathBuf> {
        if self.disabled || self.program.is_some() {
            return None;
        }
        if let Some(p) = std::env::var_os(DENO_ENV).filter(|p| !p.is_empty()) {
            return Some(PathBuf::from(p)).filter(|p| p.is_file());
        }
        let exe = if cfg!(windows) { "deno.exe" } else { "deno" };
        self.tools_dir
            .as_ref()
            .map(|d| d.join(DENO_DIR).join(exe))
            .filter(|p| p.is_file())
            .or_else(|| search_path(exe))
            .or_else(|| {
                dirs::home_dir()
                    .map(|h| h.join(".deno").join("bin").join(exe))
                    .filter(|p| p.is_file())
            })
    }

    /// rdm's own copy, if installed.
    pub fn managed_ytdlp(&self) -> Option<PathBuf> {
        let dir = self.tools_dir.as_ref()?.join(YTDLP_DIR);
        tools::find_program(&dir, "yt-dlp")
    }

    /// Whether `path` is one of rdm's own copies (updated by rdm).
    pub fn is_managed(&self, path: &Path) -> bool {
        self.tools_dir.as_ref().is_some_and(|d| path.starts_with(d))
    }

    /// `yt-dlp --version` / `deno --version` (first line).
    pub async fn version(path: &Path) -> Result<String> {
        let out = crate::background_command(path)
            .arg("--version")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(Error::io(path))?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .map(|l| l.trim().to_owned())
            .filter(|l| out.status.success() && !l.is_empty())
            .ok_or_else(|| Error::YtDlp(format!("{} does not run", path.display())))
    }

    /// Reads a web page with yt-dlp: a video with its formats, or a
    /// playlist's entries. Browser cookies, Referer, User-Agent and the
    /// proxy are taken from `request`.
    pub async fn extract(&self, request: &Request) -> Result<SiteInfo> {
        let exe = self.locate().ok_or(Error::YtDlpMissing)?;
        let session = Session::new(self, request)?;
        let mut args = session.common_args();
        args.extend(
            [
                "--dump-single-json",
                "--flat-playlist",
                "--no-playlist",
                "--no-progress",
            ]
            .map(OsString::from),
        );
        args.push(request.url.as_str().into());

        let mut child = crate::background_command(&exe)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(Error::io(&exe))?;
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let read = async {
            let mut out = Vec::new();
            let mut err = Vec::new();
            if let Some(s) = stdout.as_mut() {
                s.read_to_end(&mut out).await.map_err(Error::io(&exe))?;
            }
            if let Some(s) = stderr.as_mut() {
                let _ = s.read_to_end(&mut err).await;
            }
            let status = child.wait().await.map_err(Error::io(&exe))?;
            Ok::<_, Error>((status, out, String::from_utf8_lossy(&err).into_owned()))
        };
        let (status, out, err) = tokio::time::timeout(EXTRACT_TIMEOUT, read)
            .await
            .map_err(|_| Error::YtDlp("reading the page took too long".into()))??;
        if !status.success() {
            return Err(ytdlp_error(&err));
        }
        let json: Value = serde_json::from_slice(&out)
            .map_err(|e| Error::YtDlp(format!("unexpected output: {e}")))?;
        SiteInfo::from_json(json)
    }
}

fn search_path(exe: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if cfg!(unix) {
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    }
    dirs.into_iter().map(|d| d.join(exe)).find(|p| p.is_file())
}

/// Turns yt-dlp's last `ERROR:` line into an error, recognizing DRM.
fn ytdlp_error(stderr: &str) -> Error {
    let line = stderr
        .lines()
        .rev()
        .find(|l| l.starts_with("ERROR:"))
        .or_else(|| stderr.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("yt-dlp failed")
        .trim_start_matches("ERROR:")
        .trim();
    let lower = line.to_ascii_lowercase();
    if lower.contains("drm") {
        Error::Protected("the site uses DRM".into())
    } else if lower.contains("unsupported url") {
        Error::NoVideoFound
    } else if lower.contains("live event will begin") || lower.contains("premieres in") {
        Error::YtDlp("the video has not started yet".into())
    } else {
        Error::YtDlp(line.to_owned())
    }
}

/// Per-run arguments and the temporary cookie file (deleted on drop).
struct Session {
    args: Vec<OsString>,
    _cookies: Option<CookieFile>,
}

impl Session {
    fn new(tools: &YtDlp, request: &Request) -> Result<Self> {
        let mut args: Vec<OsString> = ["--ignore-config", "--no-warnings", "--no-color"]
            .map(OsString::from)
            .to_vec();
        if let Some(deno) = tools.locate_deno() {
            let mut v = OsString::from("deno:");
            v.push(deno.as_os_str());
            args.push("--js-runtimes".into());
            args.push(v);
        }
        if let Some(dir) = tools.tools_dir() {
            args.push("--cache-dir".into());
            args.push(dir.join("yt-dlp-cache").into());
        }
        for (name, value) in &request.headers {
            if name.eq_ignore_ascii_case("user-agent") {
                args.extend(["--user-agent".into(), value.into()]);
            } else if name.eq_ignore_ascii_case("referer") {
                args.extend(["--referer".into(), value.into()]);
            } else if !name.eq_ignore_ascii_case("cookie") {
                args.extend(["--add-header".into(), format!("{name}:{value}").into()]);
            }
        }
        match &request.proxy {
            ProxyConfig::System => {}
            ProxyConfig::Direct => args.extend(["--proxy".into(), "".into()]),
            ProxyConfig::Custom { url, .. } => args.extend(["--proxy".into(), url.into()]),
        }
        let cookies = match request.cookies.as_deref().filter(|c| !c.trim().is_empty()) {
            Some(c) => Some(CookieFile::write(&request.url, c)?),
            None => None,
        };
        if let Some(c) = &cookies {
            args.push("--cookies".into());
            args.push(c.path.clone().into());
        }
        Ok(Self {
            args,
            _cookies: cookies,
        })
    }

    fn common_args(&self) -> Vec<OsString> {
        self.args.clone()
    }
}

/// Browser cookies in the Netscape format yt-dlp reads, in a private
/// temporary folder that is removed afterwards. Cookies never go to a
/// permanent file.
struct CookieFile {
    dir: PathBuf,
    path: PathBuf,
}

impl CookieFile {
    fn write(url: &Url, header: &str) -> Result<Self> {
        let host = url.host_str().unwrap_or_default();
        // Cookies the browser sent for this page apply to the whole site.
        let domain = format!(".{}", host.trim_start_matches("www."));
        let secure = if url.scheme() == "https" {
            "TRUE"
        } else {
            "FALSE"
        };
        let mut text = String::from("# Netscape HTTP Cookie File\n");
        for pair in header.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            if name.contains(['\t', '\n']) || value.contains(['\t', '\n']) {
                continue;
            }
            text.push_str(&format!(
                "{domain}\tTRUE\t/\t{secure}\t0\t{name}\t{value}\n"
            ));
        }
        let dir = private_dir("cookies")?;
        let path = dir.join("cookies.txt");
        std::fs::write(&path, text).map_err(Error::io(&path))?;
        Ok(Self { dir, path })
    }
}

impl Drop for CookieFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A fresh folder of rdm's own under the temporary directory, readable only
/// by this user, for what is handed to yt-dlp and never kept.
fn private_dir(what: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("rdm-{what}-{}-{}", std::process::id(), unique()));
    std::fs::create_dir(&dir).map_err(Error::io(&dir))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(Error::io(&dir))?;
    }
    Ok(dir)
}

fn unique() -> u64 {
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()));
    nanos ^ (NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) << 32)
}

// ---------------------------------------------------------------------------
// What a page offers
// ---------------------------------------------------------------------------

type Value = serde_json::Value;

/// A web page read by yt-dlp.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteInfo {
    pub title: String,
    /// The site, e.g. "youtube".
    pub extractor: String,
    /// The page's canonical address.
    pub page_url: String,
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
    pub uploader: Option<String>,
    /// Qualities to choose from, best first (empty for a playlist).
    pub choices: Vec<SiteChoice>,
    /// The videos of a playlist or channel page.
    pub entries: Vec<PlaylistEntry>,
    #[serde(skip)]
    formats: Vec<SiteFormat>,
}

/// One downloadable quality.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteChoice {
    /// yt-dlp format selector: `"299+140"` (video + audio) or `"18"`.
    pub id: String,
    /// "1080p60 · H.264 · 258 MB".
    pub label: String,
    pub height: Option<u32>,
    pub audio_only: bool,
    /// Total size when the site reports it.
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistEntry {
    pub url: String,
    pub title: Option<String>,
    pub duration: Option<f64>,
}

/// A format as yt-dlp describes it (the parts rdm uses).
#[derive(Debug, Clone, PartialEq, Default)]
struct SiteFormat {
    id: String,
    ext: String,
    protocol: String,
    url: String,
    vcodec: Option<String>,
    acodec: Option<String>,
    height: Option<u32>,
    fps: Option<f64>,
    tbr: Option<f64>,
    size: Option<u64>,
    headers: Vec<(String, String)>,
    /// `downloader_options.http_chunk_size`: request at most this much at
    /// once (YouTube slows longer requests to playback speed).
    chunk: Option<u64>,
    fragment_base: Option<String>,
    fragments: Vec<String>,
    drm: bool,
}

impl SiteFormat {
    fn from_json(f: &Value) -> Option<Self> {
        let s = |k: &str| f.get(k).and_then(Value::as_str).map(str::to_owned);
        let codec = |k: &str| s(k).filter(|c| !c.is_empty());
        let headers = f
            .get("http_headers")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        let fragments = f
            .get("fragments")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|fr| {
                        fr.get("url")
                            .or_else(|| fr.get("path"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            id: s("format_id")?,
            ext: s("ext").unwrap_or_default(),
            protocol: s("protocol").unwrap_or_default(),
            url: s("url").unwrap_or_default(),
            vcodec: codec("vcodec"),
            acodec: codec("acodec"),
            height: f
                .get("height")
                .and_then(Value::as_u64)
                .and_then(|h| u32::try_from(h).ok()),
            fps: f.get("fps").and_then(Value::as_f64),
            tbr: f.get("tbr").and_then(Value::as_f64),
            size: f
                .get("filesize")
                .and_then(Value::as_u64)
                .or_else(|| f.get("filesize_approx").and_then(Value::as_u64)),
            headers,
            chunk: f
                .pointer("/downloader_options/http_chunk_size")
                .and_then(Value::as_u64)
                .filter(|c| *c > 0),
            fragment_base: s("fragment_base_url"),
            fragments,
            drm: f.get("has_drm").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    fn has_video(&self) -> bool {
        self.vcodec.as_deref() != Some("none")
    }

    /// yt-dlp leaves codecs out when it does not know them; a format with
    /// neither marked `none` is treated as having both.
    fn has_audio(&self) -> bool {
        match self.acodec.as_deref() {
            Some("none") => false,
            Some(_) => true,
            None => self.vcodec.as_deref() != Some("none") || self.protocol.starts_with("m3u8"),
        }
    }

    fn is_media(&self) -> bool {
        !self.drm
            && self.protocol != "mhtml"
            && !self.url.is_empty()
            && (self.has_video() || self.has_audio())
    }

    /// Lower is better: how directly rdm can download it.
    fn protocol_rank(&self) -> u8 {
        match self.protocol.as_str() {
            "https" | "http" => 0,
            "m3u8_native" | "m3u8" => 1,
            "http_dash_segments" => 2,
            _ => 3,
        }
    }

    /// Lower is better: how widely the video codec plays.
    fn codec_rank(&self) -> u8 {
        match self.vcodec.as_deref().unwrap_or_default() {
            c if c.starts_with("avc") || c.starts_with("h264") => 0,
            c if c.starts_with("vp09") || c.starts_with("vp9") => 1,
            c if c.starts_with("av01") => 2,
            c if c.starts_with("hev") || c.starts_with("hvc") => 3,
            _ => 4,
        }
    }

    fn codec_name(&self) -> Option<&'static str> {
        if !self.has_video() {
            return None;
        }
        Some(match self.codec_rank() {
            0 => "H.264",
            1 => "VP9",
            2 => "AV1",
            3 => "HEVC",
            _ => return None,
        })
    }
}

impl SiteInfo {
    fn from_json(json: Value) -> Result<Self> {
        let s = |k: &str| json.get(k).and_then(Value::as_str).map(str::to_owned);
        if json.get("is_live").and_then(Value::as_bool) == Some(true)
            || s("live_status").as_deref() == Some("is_live")
        {
            return Err(Error::Live);
        }
        let title = s("title")
            .or_else(|| s("id"))
            .unwrap_or_else(|| "video".into());
        let page_url = s("webpage_url")
            .or_else(|| s("original_url"))
            .unwrap_or_default();
        let mut info = SiteInfo {
            title,
            extractor: s("extractor_key")
                .or_else(|| s("extractor"))
                .unwrap_or_default(),
            page_url,
            duration: json.get("duration").and_then(Value::as_f64),
            thumbnail: s("thumbnail"),
            uploader: s("uploader").or_else(|| s("channel")),
            choices: Vec::new(),
            entries: Vec::new(),
            formats: Vec::new(),
        };

        if s("_type")
            .as_deref()
            .is_some_and(|t| t == "playlist" || t == "multi_video")
        {
            info.entries = json
                .get("entries")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|e| {
                            let url = e
                                .get("webpage_url")
                                .or_else(|| e.get("url"))
                                .and_then(Value::as_str)?;
                            Url::parse(url)
                                .ok()
                                .filter(|u| u.scheme().starts_with("http"))?;
                            Some(PlaylistEntry {
                                url: url.to_owned(),
                                title: e.get("title").and_then(Value::as_str).map(str::to_owned),
                                duration: e.get("duration").and_then(Value::as_f64),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if info.entries.is_empty() {
                return Err(Error::NoVideoFound);
            }
            return Ok(info);
        }

        let all: Vec<SiteFormat> = match json.get("formats").and_then(Value::as_array) {
            Some(list) => list.iter().filter_map(SiteFormat::from_json).collect(),
            // A page with a single file and no format list.
            None => SiteFormat::from_json(&json).into_iter().collect(),
        };
        let usable: Vec<SiteFormat> = all.iter().filter(|f| f.is_media()).cloned().collect();
        if usable.is_empty() {
            if all.iter().any(|f| f.drm)
                || json.get("_has_drm").and_then(Value::as_bool) == Some(true)
            {
                return Err(Error::Protected("the site uses DRM".into()));
            }
            return Err(Error::NoVideoFound);
        }
        info.choices = choices(&usable);
        info.formats = usable;
        Ok(info)
    }

    /// The formats a choice (`"299+140"`) is made of.
    fn formats_for(&self, choice: &str) -> Result<Vec<&SiteFormat>> {
        choice
            .split('+')
            .map(|id| {
                self.formats
                    .iter()
                    .find(|f| f.id == id)
                    .ok_or_else(|| Error::NoSuchVariant(id.to_owned()))
            })
            .collect()
    }

    /// The best choice (what "default" means).
    pub fn best(&self) -> Option<&SiteChoice> {
        self.choices
            .iter()
            .find(|c| !c.audio_only)
            .or(self.choices.first())
    }
}

/// One choice per resolution (the most compatible, most direct format at
/// that height, with the best audio when the video has none), plus "audio
/// only". Best first.
fn choices(formats: &[SiteFormat]) -> Vec<SiteChoice> {
    let audio = best_audio(formats);
    let mut by_height: HashMap<u32, &SiteFormat> = HashMap::new();
    for f in formats.iter().filter(|f| f.has_video()) {
        let h = f.height.unwrap_or(0);
        let better = match by_height.get(&h) {
            None => true,
            Some(cur) => {
                let key = |x: &SiteFormat| {
                    (
                        // Combined formats need no audio track, but only win
                        // when they are as direct.
                        x.protocol_rank(),
                        x.codec_rank(),
                        std::cmp::Reverse((x.fps.unwrap_or(0.0) * 100.0) as u64),
                        std::cmp::Reverse((x.tbr.unwrap_or(0.0) * 100.0) as u64),
                    )
                };
                key(f) < key(cur)
            }
        };
        if better {
            by_height.insert(h, f);
        }
    }
    let mut heights: Vec<u32> = by_height.keys().copied().collect();
    heights.sort_unstable_by(|a, b| b.cmp(a));

    let mut out = Vec::new();
    for h in heights {
        let Some(v) = by_height.get(&h) else { continue };
        let (id, size) = match (v.has_audio(), audio) {
            (false, Some(a)) => (
                format!("{}+{}", v.id, a.id),
                v.size.zip(a.size).map(|(x, y)| x + y),
            ),
            _ => (v.id.clone(), v.size),
        };
        let mut parts = Vec::new();
        if h > 0 {
            let fps = v
                .fps
                .filter(|f| *f > 31.0)
                .map(|f| format!("{}", f.round() as u32));
            parts.push(format!("{h}p{}", fps.unwrap_or_default()));
        } else {
            parts.push("Video".into());
        }
        if let Some(c) = v.codec_name() {
            parts.push(c.into());
        }
        if let Some(s) = size {
            parts.push(human_size(s));
        }
        out.push(SiteChoice {
            id,
            label: parts.join(" · "),
            height: v.height,
            audio_only: false,
            size,
        });
    }
    if let Some(a) = audio {
        let mut parts = vec!["Audio only".to_owned(), a.ext.to_ascii_uppercase()];
        if let Some(s) = a.size {
            parts.push(human_size(s));
        }
        out.push(SiteChoice {
            id: a.id.clone(),
            label: parts.join(" · "),
            height: None,
            audio_only: true,
            size: a.size,
        });
    }
    out
}

/// The audio track to pair with video-only formats: AAC in MP4 first (it
/// merges into MP4), then the highest bitrate; the original mix before
/// dynamic-range-compressed copies.
fn best_audio(formats: &[SiteFormat]) -> Option<&SiteFormat> {
    formats
        .iter()
        .filter(|f| !f.has_video() && f.has_audio())
        .min_by_key(|f| {
            (
                f.protocol_rank(),
                u8::from(f.id.contains("drc")),
                u8::from(!f.acodec.as_deref().unwrap_or_default().starts_with("mp4a")),
                std::cmp::Reverse((f.tbr.unwrap_or(0.0) * 100.0) as u64),
            )
        })
}

fn human_size(bytes: u64) -> String {
    let mb = bytes as f64 / 1e6;
    if mb >= 1000.0 {
        format!("{:.1} GB", mb / 1000.0)
    } else if mb >= 10.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{mb:.1} MB")
    }
}

// ---------------------------------------------------------------------------
// From a choice to downloadable tracks
// ---------------------------------------------------------------------------

/// A track to download, with the request it needs.
#[derive(Debug, Clone)]
pub(crate) struct SiteTrack {
    pub name: &'static str,
    pub track: SiteTrackSource,
    pub request: Request,
    pub request_size: Option<NonZeroU64>,
}

#[derive(Debug, Clone)]
pub(crate) enum SiteTrackSource {
    /// Ready to download.
    Track(Box<Track>),
    /// An HLS media playlist to read first.
    Hls(Url),
}

/// Why a choice cannot be downloaded by rdm itself.
#[derive(Debug)]
pub(crate) struct NeedsYtDlp(pub String);

/// The tracks for `choice` (or the best one), or the reason rdm's own
/// downloader cannot handle them (yt-dlp downloads instead).
pub(crate) fn plan_site(
    info: &SiteInfo,
    choice: Option<&str>,
    base: &Request,
) -> Result<std::result::Result<Vec<SiteTrack>, NeedsYtDlp>> {
    let id = match choice {
        Some(c) => c.to_owned(),
        None => info
            .best()
            .map(|c| c.id.clone())
            .ok_or(Error::NoVideoFound)?,
    };
    let formats = info.formats_for(&id)?;
    let mut tracks = Vec::new();
    for (i, f) in formats.iter().enumerate() {
        let name = match (formats.len(), i) {
            (1, _) if !f.has_video() => "audio",
            (1, _) | (_, 0) => "video",
            _ => "audio",
        };
        let supported = matches!(
            f.protocol.as_str(),
            "https" | "http" | "m3u8_native" | "m3u8"
        ) || (f.protocol == "http_dash_segments" && !f.fragments.is_empty());
        if !supported {
            return Ok(Err(NeedsYtDlp(format!("the `{}` protocol", f.protocol))));
        }
        let url = Url::parse(&f.url).map_err(|e| Error::parse("format URL", e.to_string()))?;
        let mut request = Request::new(url.as_str())?.proxy(base.proxy.clone());
        request.headers = f.headers.clone();
        request.cookies = base.cookies.clone();
        let container = container_for(f);
        let source = match f.protocol.as_str() {
            "https" | "http" => SiteTrackSource::Track(Box::new(Track {
                init: None,
                segments: Vec::new(),
                container,
                whole: Some(url),
            })),
            "m3u8_native" | "m3u8" => SiteTrackSource::Hls(url),
            "http_dash_segments" if !f.fragments.is_empty() => {
                let base_url = f.fragment_base.as_deref().unwrap_or(&f.url);
                let base_url = Url::parse(base_url)
                    .map_err(|e| Error::parse("fragment URL", e.to_string()))?;
                let segments = f
                    .fragments
                    .iter()
                    .map(|p| {
                        base_url
                            .join(p)
                            .map(|url| crate::model::SegmentRef {
                                url,
                                range: None,
                                key: None,
                            })
                            .map_err(|e| Error::parse("fragment URL", e.to_string()))
                    })
                    .collect::<Result<Vec<_>>>()?;
                SiteTrackSource::Track(Box::new(Track {
                    // yt-dlp lists the initialization segment first.
                    init: None,
                    segments,
                    container,
                    whole: None,
                }))
            }
            other => return Ok(Err(NeedsYtDlp(format!("the `{other}` protocol")))),
        };
        tracks.push(SiteTrack {
            name,
            track: source,
            request,
            request_size: f.chunk.and_then(NonZeroU64::new),
        });
    }
    Ok(Ok(tracks))
}

fn container_for(f: &SiteFormat) -> Container {
    let audio_only = !f.has_video();
    match f.ext.as_str() {
        "webm" if audio_only => Container::Audio("webm"),
        "webm" => Container::WebM,
        "m4a" => Container::Audio("m4a"),
        "mp3" => Container::Audio("mp3"),
        "ogg" | "oga" => Container::Audio("ogg"),
        "opus" => Container::Audio("opus"),
        "aac" => Container::Audio("aac"),
        "ts" => Container::Ts,
        "mp4" if audio_only => Container::Audio("m4a"),
        "mkv" => Container::WebM,
        _ if f.protocol.starts_with("m3u8") => Container::Ts,
        _ => Container::Mp4,
    }
}

// ---------------------------------------------------------------------------
// yt-dlp's own downloader (fallback)
// ---------------------------------------------------------------------------

/// Progress reported by yt-dlp's downloader.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct YtDlpProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
    pub speed: Option<f64>,
    pub eta: Option<f64>,
}

/// Parses a `--progress-template "download:RDM %(progress)j"` line.
pub(crate) fn parse_progress(line: &str) -> Option<YtDlpProgress> {
    let json: Value = serde_json::from_str(line.strip_prefix("RDM ")?).ok()?;
    let n = |k: &str| json.get(k).and_then(Value::as_f64);
    Some(YtDlpProgress {
        downloaded: n("downloaded_bytes")? as u64,
        total: n("total_bytes")
            .or_else(|| n("total_bytes_estimate"))
            .map(|t| t as u64),
        speed: n("speed"),
        eta: n("eta"),
    })
}

/// Where yt-dlp writes the finished file's path, in a private temporary
/// folder that is removed afterwards.
///
/// The path cannot be read from yt-dlp's stdout: yt-dlp encodes everything
/// it prints with the system's preferred encoding, which on Windows is the
/// ANSI code page and not UTF-8, so a file name outside that code page
/// comes back either as bytes rdm cannot decode or with the characters
/// dropped. `--print-to-file` is written as UTF-8 whatever the system says.
struct ReportFile {
    dir: PathBuf,
    path: PathBuf,
}

impl ReportFile {
    fn new() -> Result<Self> {
        let dir = private_dir("ytdlp")?;
        let path = dir.join("filepath.txt");
        Ok(Self { dir, path })
    }

    /// The last path yt-dlp appended, if any.
    fn read(&self) -> Option<PathBuf> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        text.lines()
            .map(str::trim_end)
            .rfind(|l| !l.is_empty())
            .map(PathBuf::from)
    }
}

impl Drop for ReportFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One line of yt-dlp's output, with its line ending removed. What is not
/// UTF-8 is replaced rather than ending the stream: see [`ReportFile`].
fn output_line(buf: &[u8]) -> std::borrow::Cow<'_, str> {
    let end = buf.len() - buf.iter().rev().take_while(|b| b"\r\n".contains(b)).count();
    String::from_utf8_lossy(&buf[..end])
}

/// A yt-dlp download run.
pub(crate) struct YtDlpDownload<'a> {
    pub tools: &'a YtDlp,
    pub request: &'a Request,
    pub choice: Option<&'a str>,
    /// Output path without extension; yt-dlp adds the right one.
    pub output_stem: &'a Path,
    pub merge_format: Option<&'static str>,
    pub audio_format: Option<&'static str>,
    pub ffmpeg: Option<PathBuf>,
    pub speed_limit: Option<NonZeroU64>,
    pub fragments: usize,
}

impl YtDlpDownload<'_> {
    /// Runs until done, calling `progress` for each update. `stop`
    /// resolving kills yt-dlp (its `.part` files are kept for resuming).
    /// Returns the final file, or `None` if stopped.
    pub async fn run(
        &self,
        mut progress: impl FnMut(YtDlpProgress),
        stop: impl Future<Output = ()>,
    ) -> Result<Option<PathBuf>> {
        let exe = self.tools.locate().ok_or(Error::YtDlpMissing)?;
        let session = Session::new(self.tools, self.request)?;
        let mut args = session.common_args();
        let mut push = |a: &[&str]| args.extend(a.iter().map(OsString::from));
        push(&["--no-playlist", "--newline", "--continue", "--no-mtime"]);
        push(&["--progress-template", "download:RDM %(progress)j"]);
        // `--print` implies `--quiet`, and quiet turns the progress off:
        // without `--progress` the template above is never printed.
        push(&["--progress"]);
        push(&["--print", "after_move:RDMFILE %(filepath)s"]);
        push(&[
            "--concurrent-fragments",
            &self.fragments.clamp(1, 8).to_string(),
        ]);
        if let Some(choice) = self.choice {
            push(&["-f", choice]);
        }
        if let Some(fmt) = self.merge_format {
            push(&["--merge-output-format", fmt]);
        }
        if let Some(fmt) = self.audio_format {
            push(&["--extract-audio", "--audio-format", fmt]);
        }
        if let Some(limit) = self.speed_limit {
            push(&["--limit-rate", &limit.get().to_string()]);
        }
        if let Some(f) = &self.ffmpeg {
            args.push("--ffmpeg-location".into());
            args.push(f.into());
        }
        let report = ReportFile::new()?;
        args.push("--print-to-file".into());
        args.push("after_move:%(filepath)s".into());
        args.push(report.path.clone().into());
        let mut template = self.output_stem.as_os_str().to_owned();
        template.push(".%(ext)s");
        args.push("-o".into());
        args.push(template);
        args.push(self.request.url.as_str().into());

        let mut child = crate::background_command(&exe)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(Error::io(&exe))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Task("no stdout".into()))?;
        let mut stderr = child.stderr.take();
        let mut out = BufReader::new(stdout);
        let mut buf = Vec::new();
        let mut file = None;
        tokio::pin!(stop);
        loop {
            tokio::select! {
                biased;
                () = &mut stop => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    return Ok(None);
                }
                read = out.read_until(b'\n', &mut buf) => {
                    if !matches!(read, Ok(1..)) {
                        break;
                    }
                    let line = output_line(&buf);
                    if let Some(p) = parse_progress(&line) {
                        progress(p);
                    } else if let Some(path) = line.strip_prefix("RDMFILE ") {
                        file = Some(PathBuf::from(path));
                    }
                    buf.clear();
                }
            }
        }
        let mut err = Vec::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_end(&mut err).await;
        }
        let status = child.wait().await.map_err(Error::io(&exe))?;
        if !status.success() {
            return Err(ytdlp_error(&String::from_utf8_lossy(&err)));
        }
        report
            .read()
            .or(file)
            .filter(|f| f.is_file())
            .map(Some)
            .ok_or_else(|| Error::YtDlp("yt-dlp finished without reporting the file".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn yt_like() -> Value {
        let hdr = json!({"User-Agent": "UA", "Accept": "*/*"});
        json!({
            "_type": "video", "id": "abc", "title": "A / Film", "extractor_key": "Youtube",
            "webpage_url": "https://www.youtube.com/watch?v=abc", "duration": 635.0,
            "formats": [
                {"format_id": "sb0", "ext": "mhtml", "protocol": "mhtml", "url": "https://i.ytimg.com/sb", "vcodec": "none", "acodec": "none"},
                {"format_id": "139-drc", "ext": "m4a", "protocol": "https", "url": "https://v.example/139d", "vcodec": "none", "acodec": "mp4a.40.5", "tbr": 49.0, "filesize": 3_000_000},
                {"format_id": "140", "ext": "m4a", "protocol": "https", "url": "https://v.example/140", "vcodec": "none", "acodec": "mp4a.40.2", "tbr": 129.0, "filesize": 10_000_000, "http_headers": hdr, "downloader_options": {"http_chunk_size": 10485760}},
                {"format_id": "251", "ext": "webm", "protocol": "https", "url": "https://v.example/251", "vcodec": "none", "acodec": "opus", "tbr": 135.0, "filesize": 10_200_000},
                {"format_id": "299", "ext": "mp4", "protocol": "https", "url": "https://v.example/299", "vcodec": "avc1.64002A", "acodec": "none", "height": 1080, "fps": 60, "tbr": 3247.0, "filesize": 257_000_000, "http_headers": hdr, "downloader_options": {"http_chunk_size": 10485760}},
                {"format_id": "303", "ext": "webm", "protocol": "https", "url": "https://v.example/303", "vcodec": "vp9", "acodec": "none", "height": 1080, "fps": 60, "tbr": 2127.0, "filesize": 168_000_000},
                {"format_id": "312", "ext": "mp4", "protocol": "m3u8_native", "url": "https://v.example/312.m3u8", "vcodec": "avc1.64002A", "height": 1080, "fps": 60, "tbr": 7417.0},
                {"format_id": "315", "ext": "webm", "protocol": "https", "url": "https://v.example/315", "vcodec": "vp9", "acodec": "none", "height": 2160, "fps": 60, "tbr": 17174.0, "filesize": 1_362_000_000},
                {"format_id": "18", "ext": "mp4", "protocol": "https", "url": "https://v.example/18", "vcodec": "avc1.42001E", "acodec": "mp4a.40.2", "height": 360, "fps": 30, "filesize": 30_000_000},
                {"format_id": "drm-1080", "ext": "mp4", "protocol": "https", "url": "https://v.example/x", "vcodec": "avc1", "acodec": "none", "height": 4320, "has_drm": true}
            ]
        })
    }

    #[test]
    fn choices_are_one_per_height_most_compatible_first() {
        let info = SiteInfo::from_json(yt_like()).unwrap();
        assert_eq!(info.title, "A / Film");
        let got: Vec<(&str, &str)> = info
            .choices
            .iter()
            .map(|c| (c.id.as_str(), c.label.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("315+140", "2160p60 · VP9 · 1.4 GB"),
                ("299+140", "1080p60 · H.264 · 267 MB"),
                ("18", "360p · H.264 · 30 MB"),
                ("140", "Audio only · M4A · 10 MB"),
            ],
            "DRM and storyboard formats are dropped; H.264 over HTTPS wins at 1080p; \
             the AAC original beats opus and the DRC copy"
        );
        assert_eq!(info.best().map(|c| c.id.as_str()), Some("315+140"));
    }

    #[test]
    fn plans_use_the_format_headers_and_chunk_size() {
        let info = SiteInfo::from_json(yt_like()).unwrap();
        let base = Request::new("https://www.youtube.com/watch?v=abc")
            .unwrap()
            .cookies("a=1");
        let tracks = plan_site(&info, Some("299+140"), &base).unwrap().unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!((tracks[0].name, tracks[1].name), ("video", "audio"));
        assert_eq!(
            tracks[0].request.headers,
            [
                ("Accept".into(), "*/*".into()),
                ("User-Agent".into(), "UA".into())
            ]
        );
        assert_eq!(
            tracks[0].request_size.map(NonZeroU64::get),
            Some(10_485_760)
        );
        assert_eq!(tracks[1].request.cookies.as_deref(), Some("a=1"));
        match &tracks[1].track {
            SiteTrackSource::Track(t) => assert_eq!(t.container, Container::Audio("m4a")),
            other => panic!("{other:?}"),
        }
        let hls = plan_site(&info, Some("312"), &base).unwrap().unwrap();
        assert!(matches!(hls[0].track, SiteTrackSource::Hls(_)));
        assert!(plan_site(&info, Some("999"), &base).is_err());
    }

    #[test]
    fn other_protocols_go_to_ytdlp() {
        let json = json!({"title": "t", "formats": [
            {"format_id": "rtmp-1", "ext": "flv", "protocol": "rtmp", "url": "rtmp://x/y", "vcodec": "h264", "acodec": "aac", "height": 720}
        ]});
        let info = SiteInfo::from_json(json).unwrap();
        let base = Request::new("https://example.com/v").unwrap();
        assert!(plan_site(&info, None, &base).unwrap().is_err());
    }

    #[test]
    fn drm_live_and_playlists() {
        let drm = json!({"title": "t", "formats": [
            {"format_id": "a", "ext": "mp4", "protocol": "https", "url": "https://x/a", "vcodec": "avc1", "acodec": "mp4a", "has_drm": true}
        ]});
        assert!(matches!(SiteInfo::from_json(drm), Err(Error::Protected(_))));
        let live = json!({"title": "t", "is_live": true, "formats": []});
        assert!(matches!(SiteInfo::from_json(live), Err(Error::Live)));
        let list = json!({"_type": "playlist", "title": "Talks", "entries": [
            {"url": "https://www.youtube.com/watch?v=1", "title": "One", "duration": 60},
            {"url": "not a url"},
            {"webpage_url": "https://www.youtube.com/watch?v=2", "title": "Two"}
        ]});
        let info = SiteInfo::from_json(list).unwrap();
        assert!(info.choices.is_empty());
        assert_eq!(info.entries.len(), 2);
        assert_eq!(info.entries[1].title.as_deref(), Some("Two"));
    }

    #[test]
    fn error_messages() {
        assert!(matches!(
            ytdlp_error("WARNING: x\nERROR: [youtube] abc: This video is DRM protected"),
            Error::Protected(_)
        ));
        assert!(matches!(
            ytdlp_error("ERROR: Unsupported URL: https://example.com/"),
            Error::NoVideoFound
        ));
        match ytdlp_error("ERROR: [vimeo] 1: Private video") {
            Error::YtDlp(m) => assert_eq!(m, "[vimeo] 1: Private video"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn progress_lines() {
        let p = parse_progress(r#"RDM {"status": "downloading", "downloaded_bytes": 1024, "total_bytes": 4096, "speed": 512.5, "eta": 6}"#).unwrap();
        assert_eq!((p.downloaded, p.total), (1024, Some(4096)));
        let p = parse_progress(r#"RDM {"downloaded_bytes": 10, "total_bytes_estimate": 99.7}"#)
            .unwrap();
        assert_eq!(p.total, Some(99));
        assert!(parse_progress("[download] 10%").is_none());
    }

    #[test]
    fn cookie_file_is_private_and_removed() {
        let url = Url::parse("https://www.example.com/watch").unwrap();
        let file = CookieFile::write(&url, "a=1; b=x=y; bad").unwrap();
        let text = std::fs::read_to_string(&file.path).unwrap();
        assert!(text.contains(".example.com\tTRUE\t/\tTRUE\t0\ta\t1\n"));
        assert!(text.contains("\tb\tx=y\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file.dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let dir = file.dir.clone();
        drop(file);
        assert!(!dir.exists());
    }
}
