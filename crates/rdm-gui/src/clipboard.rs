//! Clipboard monitoring: copying a link to a known file type offers to
//! download it, so rdm is useful even without the browser extension.
//!
//! Polls once a second. Only the latest text is kept in memory, and only to
//! notice changes; nothing is logged or stored.

use std::sync::Arc;
use std::time::Duration;

use rdm_ipc::AddItem;
use rdm_manager::Manager;
use tauri::AppHandle;
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::request_add;

const POLL: Duration = Duration::from_secs(1);

pub fn spawn(app: AppHandle, manager: Arc<Manager>) {
    tauri::async_runtime::spawn(async move {
        // Whatever is on the clipboard at startup was not copied for us.
        let mut last = app.clipboard().read_text().unwrap_or_default();
        let mut interval = tokio::time::interval(POLL);
        loop {
            interval.tick().await;
            let text = app.clipboard().read_text().unwrap_or_default();
            if text == last {
                continue;
            }
            last = text;
            let settings = manager.settings();
            if !settings.clipboard_monitoring || !settings.is_download_url(&last) {
                continue;
            }
            let url = last.trim().to_owned();
            if manager.list().iter().any(|r| r.url == url) {
                continue; // already in the list
            }
            request_add(
                &app,
                AddItem {
                    url,
                    ..AddItem::default()
                },
            );
        }
    });
}
