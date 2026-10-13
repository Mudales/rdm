//! Messages between the browser extension, `rdm-host` and the app.
//!
//! JSON with a `type` tag and camelCase fields, e.g.
//!
//! ```json
//! {"type":"add","mode":"ask","items":[{"url":"https://x/a.zip","referer":"https://x/"}]}
//! ```

use serde::{Deserialize, Serialize};

/// Bumped on incompatible changes; reported by [`Response::Pong`].
pub const PROTOCOL_VERSION: u32 = 1;

/// A request to the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Request {
    /// Is the app there, and which version?
    Ping,
    /// The extension introduces itself (when it starts, and when its popup
    /// opens): answered with [`Response::Pong`], whose `extension` says
    /// which extension version this app ships, so an older one updates.
    Hello { extension: ExtensionInfo },
    /// Add one or more downloads.
    Add {
        items: Vec<AddItem>,
        #[serde(default)]
        mode: AddMode,
    },
    /// Which downloads to take over (answered with
    /// [`Response::CaptureRules`]).
    GetCaptureRules,
    /// The browser kept a download because its file type is not selected.
    ReportSkipped { extension: String },
    /// Take over (or stop taking over) downloads from a site.
    SetSiteCapture { host: String, capture: bool },
    /// Bring the app's window forward.
    Show {
        #[serde(default)]
        view: AppView,
    },
    /// The qualities a video can be downloaded in, for the extension to
    /// offer on the page: a web page's (read by yt-dlp) or a streaming
    /// playlist's. Answered with [`Response::Formats`], or
    /// [`Response::ToolMissing`] when yt-dlp is needed and not installed.
    Formats { item: Box<AddItem> },
}

impl Request {
    /// Whether `rdm-host` should start the app to answer. Requests that
    /// only query or tweak settings are answered with
    /// [`Response::NotRunning`] instead.
    pub fn launches_app(&self) -> bool {
        matches!(
            self,
            Request::Add { .. } | Request::Show { .. } | Request::Formats { .. }
        )
    }
}

/// What [`Request::Show`] opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppView {
    #[default]
    Downloads,
    /// Settings, on the tab that chooses which downloads are taken over.
    CaptureSettings,
    /// The setup assistant (browser extension, yt-dlp, ffmpeg).
    Setup,
}

/// The extension, as it describes itself in [`Request::Hello`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExtensionInfo {
    /// `manifest.json` version.
    pub version: String,
    /// "chrome", "edge", "brave", "chromium", "firefox", or another name.
    pub browser: Option<String>,
}

/// One quality in [`Response::Formats`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatChoice {
    /// Sent back as [`AddItem::variant`].
    pub id: String,
    /// "1080p60 · H.264 · 258 MB".
    pub label: String,
    #[serde(default)]
    pub audio_only: bool,
    pub size: Option<u64>,
}

/// One URL to download, with the browser context servers often require.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AddItem {
    pub url: String,
    /// Suggested name (e.g. from the browser's download item).
    pub file_name: Option<String>,
    /// Sent as `Referer`; usually the page the link or media was on.
    pub referer: Option<String>,
    /// The page the request came from (informational).
    pub page_url: Option<String>,
    pub user_agent: Option<String>,
    /// `Cookie` header value for `url`. Kept in memory by the app only.
    pub cookies: Option<String>,
    /// What the extension detected: "video", "audio", "hls" or "dash".
    pub kind: Option<String>,
    /// The page title, used to name streams.
    pub title: Option<String>,
    /// A quality already chosen on the page: a [`FormatChoice::id`].
    pub variant: Option<String>,
    /// Its label, shown in the app.
    pub label: Option<String>,
    /// Output format: "auto", "mp4", "mkv", "m4a" or "mp3".
    pub format: Option<String>,
}

/// What the app should do with added items.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AddMode {
    /// Show the Add dialog, prefilled, so the user confirms (single item).
    #[default]
    Ask,
    /// Start right away.
    Now,
    /// Put in the main queue.
    Queue,
}

/// The app's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Response {
    Pong {
        version: String,
        protocol: u32,
        /// The version of the browser extension this app ships (answering
        /// [`Request::Hello`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extension: Option<String>,
    },
    /// Items added, in request order.
    Added {
        ids: Vec<i64>,
    },
    /// The Add dialog (or the window) was shown to the user.
    Shown,
    /// The app's capture settings (`rdm_manager::CaptureSettings` as JSON:
    /// `extensions`, `minSize`, `excludedSites`, `excludedAddresses`,
    /// `captureInline`). Kept as JSON so the host relays it unchanged.
    CaptureRules {
        rules: serde_json::Value,
    },
    /// Done; nothing to report.
    Ok,
    /// The qualities of a video, best first.
    Formats {
        /// The video's title, when known.
        title: Option<String>,
        choices: Vec<FormatChoice>,
    },
    /// A tool the request needs is not installed (`tool`: "yt-dlp").
    ToolMissing {
        tool: String,
        message: String,
    },
    /// Sent by `rdm-host` for a `ping` when the app is not running (pings
    /// never launch the app; adding a download does).
    NotRunning,
    Error {
        message: String,
    },
}

impl Response {
    pub fn error(message: impl Into<String>) -> Self {
        Response::Error {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format() {
        let r: Request = serde_json::from_str(
            r#"{"type":"add","items":[{"url":"https://x/a.zip","pageUrl":"https://x/"}]}"#,
        )
        .unwrap();
        assert_eq!(
            r,
            Request::Add {
                items: vec![AddItem {
                    url: "https://x/a.zip".into(),
                    page_url: Some("https://x/".into()),
                    ..AddItem::default()
                }],
                mode: AddMode::Ask,
            }
        );
        assert_eq!(
            serde_json::to_string(&Request::Ping).unwrap(),
            r#"{"type":"ping"}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::Added { ids: vec![3] }).unwrap(),
            r#"{"type":"added","ids":[3]}"#
        );
        assert!(serde_json::from_str::<Request>(r#"{"type":"format-disk"}"#).is_err());

        let r: Request =
            serde_json::from_str(r#"{"type":"setSiteCapture","host":"x.org","capture":false}"#)
                .unwrap();
        assert!(!r.launches_app());
        let r: Request = serde_json::from_str(r#"{"type":"show"}"#).unwrap();
        assert_eq!(
            r,
            Request::Show {
                view: AppView::Downloads
            }
        );
        assert!(r.launches_app());
        let r: Request =
            serde_json::from_str(r#"{"type":"show","view":"captureSettings"}"#).unwrap();
        assert_eq!(
            r,
            Request::Show {
                view: AppView::CaptureSettings
            }
        );
        assert_eq!(
            serde_json::to_string(&Request::GetCaptureRules).unwrap(),
            r#"{"type":"getCaptureRules"}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::Ok).unwrap(),
            r#"{"type":"ok"}"#
        );
    }

    #[test]
    fn hello_and_formats() {
        let r: Request = serde_json::from_str(
            r#"{"type":"hello","extension":{"version":"0.4.0","browser":"edge"}}"#,
        )
        .unwrap();
        assert_eq!(
            r,
            Request::Hello {
                extension: ExtensionInfo {
                    version: "0.4.0".into(),
                    browser: Some("edge".into()),
                }
            }
        );
        assert!(!r.launches_app());
        // Older apps answer without the extension version, and newer ones
        // leave it out of a plain ping's answer.
        let pong = Response::Pong {
            version: "0.4.0".into(),
            protocol: 1,
            extension: None,
        };
        assert_eq!(
            serde_json::to_string(&pong).unwrap(),
            r#"{"type":"pong","version":"0.4.0","protocol":1}"#
        );
        assert_eq!(
            serde_json::from_str::<Response>(r#"{"type":"pong","version":"0.3.1","protocol":1}"#)
                .unwrap(),
            Response::Pong {
                version: "0.3.1".into(),
                protocol: 1,
                extension: None
            }
        );

        let r: Request = serde_json::from_str(
            r#"{"type":"formats","item":{"url":"https://x/watch?v=1","kind":"page"}}"#,
        )
        .unwrap();
        assert!(r.launches_app());
        let item: AddItem = serde_json::from_str(
            r#"{"url":"https://x/watch?v=1","kind":"page","variant":"299+140","format":"mp4"}"#,
        )
        .unwrap();
        assert_eq!(item.variant.as_deref(), Some("299+140"));
        assert_eq!(item.format.as_deref(), Some("mp4"));
        assert_eq!(
            serde_json::to_string(&Response::ToolMissing {
                tool: "yt-dlp".into(),
                message: "m".into()
            })
            .unwrap(),
            r#"{"type":"toolMissing","tool":"yt-dlp","message":"m"}"#
        );
        let r: Request = serde_json::from_str(r#"{"type":"show","view":"setup"}"#).unwrap();
        assert_eq!(
            r,
            Request::Show {
                view: AppView::Setup
            }
        );
    }
}
