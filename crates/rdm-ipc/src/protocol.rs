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
    /// Add one or more downloads.
    Add {
        items: Vec<AddItem>,
        #[serde(default)]
        mode: AddMode,
    },
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
    },
    /// Items added, in request order.
    Added {
        ids: Vec<i64>,
    },
    /// The Add dialog was shown to the user.
    Shown,
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
    }
}
