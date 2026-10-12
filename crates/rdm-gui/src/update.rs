//! In-app updates: signed packages published with each GitHub release
//! (`latest.json`, see docs/RELEASING.md). The signature is checked against
//! the public key in `tauri.conf.json` before anything is installed, which
//! is independent of OS code signing.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rdm_manager::Manager;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager as _, State};
use tauri_plugin_updater::{Update, UpdaterExt};

/// Tells the UI a newer version exists; the payload is an [`UpdateInfo`].
const UPDATE_AVAILABLE: &str = "rdm://update-available";
/// Download progress while installing: `{ downloaded, total }`.
const UPDATE_PROGRESS: &str = "rdm://update-progress";

const RELEASES_URL: &str = "https://github.com/Mudales/rdm/releases";
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(20);
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    version: String,
    current_version: String,
    notes: Option<String>,
    date: Option<String>,
}

impl From<&Update> for UpdateInfo {
    fn from(u: &Update) -> Self {
        Self {
            version: u.version.clone(),
            current_version: u.current_version.clone(),
            notes: u.body.clone().filter(|b| !b.trim().is_empty()),
            date: u.date.map(|d| d.date().to_string()),
        }
    }
}

/// The newest update found by the background check, for a UI that loads
/// after the event was sent.
#[derive(Default)]
pub struct LatestUpdate(Mutex<Option<UpdateInfo>>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    version: &'static str,
    /// Whether this installation can update itself. Linux packages
    /// installed by apt are updated by downloading the new `.deb`.
    can_install: bool,
    releases_url: &'static str,
    available: Option<UpdateInfo>,
}

#[tauri::command]
pub fn app_info(app: AppHandle) -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        can_install: can_install(),
        releases_url: RELEASES_URL,
        available: app
            .try_state::<LatestUpdate>()
            .and_then(|l| l.0.lock().unwrap_or_else(|e| e.into_inner()).clone()),
    }
}

/// The updater can replace a macOS app bundle, run the Windows installer,
/// or swap an AppImage; not a `.deb` installation.
fn can_install() -> bool {
    !cfg!(target_os = "linux") || std::env::var_os("APPIMAGE").is_some()
}

async fn find(app: &AppHandle) -> Result<Option<Update>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    updater.check().await.map_err(|e| e.to_string())
}

fn remember(app: &AppHandle, info: Option<&UpdateInfo>) {
    if let Some(latest) = app.try_state::<LatestUpdate>() {
        *latest.0.lock().unwrap_or_else(|e| e.into_inner()) = info.cloned();
    }
}

/// Opens the latest release in the browser (for installations that cannot
/// update themselves).
#[tauri::command]
pub fn open_releases_page(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(format!("{RELEASES_URL}/latest"), None::<&str>)
        .map_err(|e| e.to_string())
}

/// Looks for a newer version now.
#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    let info = find(&app).await?.as_ref().map(UpdateInfo::from);
    remember(&app, info.as_ref());
    Ok(info)
}

/// Downloads and verifies the update, saves every download's progress,
/// installs it and restarts rdm.
#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    manager: State<'_, Arc<Manager>>,
) -> Result<(), String> {
    if !can_install() {
        return Err(format!(
            "this installation cannot update itself; download the new version from {RELEASES_URL}"
        ));
    }
    let update = find(&app)
        .await?
        .ok_or_else(|| "rdm is up to date".to_owned())?;
    let mut downloaded = 0u64;
    let bytes = update
        .download(
            |chunk, total| {
                downloaded += chunk as u64;
                let _ = app.emit(
                    UPDATE_PROGRESS,
                    serde_json::json!({ "downloaded": downloaded, "total": total }),
                );
            },
            || {},
        )
        .await
        .map_err(|e| format!("could not download the update: {e}"))?;
    // On Windows the installer takes over and this process exits, so stop
    // and save everything first.
    manager.shutdown().await;
    update
        .install(bytes)
        .map_err(|e| format!("could not install the update: {e}"))?;
    app.restart();
}

/// Checks shortly after startup and then daily, when enabled in settings.
pub fn spawn_daily_check(app: AppHandle) {
    app.manage(LatestUpdate::default());
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            let enabled = app
                .try_state::<Arc<Manager>>()
                .is_some_and(|m| m.settings().check_updates);
            if enabled {
                match find(&app).await {
                    Ok(Some(update)) => {
                        let info = UpdateInfo::from(&update);
                        remember(&app, Some(&info));
                        let _ = app.emit(UPDATE_AVAILABLE, info);
                    }
                    Ok(None) => remember(&app, None),
                    // Offline or GitHub unreachable: try again tomorrow.
                    Err(e) => tracing::info!("update check failed: {e}"),
                }
            }
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}
