//! Tauri commands: thin wrappers over `rdm_manager::Manager`.
//!
//! Errors cross to the UI as their full message chain.

use std::path::PathBuf;
use std::sync::Arc;

use rdm_host::install::{Browser, BrowserStatus, Installer};
use rdm_ipc::AddItem;
use rdm_manager::{
    Algorithm, DownloadRecord, ExportFormat, FfmpegStatus, Id, ImportReport, LiveProgress, Manager,
    NewDownload, ProbeResult, QueueState, Settings, StartMode, error_chain,
};
use tauri::{AppHandle, State};

use crate::PendingAdds;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

type Mgr<'a> = State<'a, Arc<Manager>>;
type CmdResult<T> = Result<T, String>;

fn msg(e: impl std::error::Error + 'static) -> String {
    error_chain(&e)
}

fn path_of(manager: &Manager, id: Id) -> CmdResult<PathBuf> {
    manager
        .get(id)
        .map(|r| r.path())
        .ok_or_else(|| format!("download {id} not found"))
}

#[tauri::command]
pub fn list_downloads(manager: Mgr<'_>) -> Vec<DownloadRecord> {
    let list = manager.list();
    tracing::debug!(count = list.len(), "UI loaded the download list");
    list
}

#[tauri::command]
pub fn live_progress(manager: Mgr<'_>) -> Vec<LiveProgress> {
    manager.progress()
}

#[tauri::command]
pub fn get_settings(manager: Mgr<'_>) -> Settings {
    manager.settings()
}

#[tauri::command]
pub fn update_settings(manager: Mgr<'_>, settings: Settings) -> CmdResult<Settings> {
    manager.update_settings(settings).map_err(msg)
}

#[tauri::command]
pub fn get_queues(manager: Mgr<'_>) -> Vec<QueueState> {
    manager.queues()
}

#[tauri::command]
pub async fn probe_url(manager: Mgr<'_>, request: NewDownload) -> CmdResult<ProbeResult> {
    manager.probe(&request).await.map_err(msg)
}

#[tauri::command]
pub async fn add_download(manager: Mgr<'_>, request: NewDownload) -> CmdResult<DownloadRecord> {
    manager.add(request).await.map_err(msg)
}

#[tauri::command]
pub fn start_download(manager: Mgr<'_>, id: Id) -> CmdResult<()> {
    manager.start(id).map_err(msg)
}

#[tauri::command]
pub fn stop_download(manager: Mgr<'_>, id: Id) -> CmdResult<()> {
    manager.stop(id).map_err(msg)
}

#[tauri::command]
pub fn enqueue_download(manager: Mgr<'_>, id: Id) -> CmdResult<()> {
    manager.enqueue(id).map_err(msg)
}

#[tauri::command]
pub fn remove_download(manager: Mgr<'_>, id: Id, delete_file: bool) -> CmdResult<()> {
    manager.remove(id, delete_file).map_err(msg)
}

#[tauri::command]
pub fn start_queue(manager: Mgr<'_>, id: u32) -> CmdResult<()> {
    manager.start_queue(id).map_err(msg)
}

#[tauri::command]
pub fn stop_queue(manager: Mgr<'_>, id: u32) -> CmdResult<()> {
    manager.stop_queue(id).map_err(msg)
}

#[tauri::command]
pub fn open_file(app: AppHandle, manager: Mgr<'_>, id: Id) -> CmdResult<()> {
    let path = path_of(&manager, id)?;
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(msg)
}

#[tauri::command]
pub fn show_in_folder(app: AppHandle, manager: Mgr<'_>, id: Id) -> CmdResult<()> {
    let path = path_of(&manager, id)?;
    if path.exists() {
        app.opener().reveal_item_in_dir(&path).map_err(msg)
    } else {
        // Not created yet: open the folder it will go into.
        let dir = path.parent().map(PathBuf::from).unwrap_or_default();
        app.opener()
            .open_path(dir.to_string_lossy(), None::<&str>)
            .map_err(msg)
    }
}

/// Native folder picker; `None` if cancelled.
#[tauri::command]
pub async fn pick_folder(app: AppHandle, start: Option<PathBuf>) -> CmdResult<Option<PathBuf>> {
    let mut dialog = app.dialog().file();
    if let Some(dir) = start.filter(|d| d.is_dir()) {
        dialog = dialog.set_directory(dir);
    }
    Ok(dialog
        .blocking_pick_folder()
        .and_then(|p| p.into_path().ok()))
}

/// Add requests from the browser or clipboard waiting for confirmation.
#[tauri::command]
pub fn take_pending_adds(pending: State<'_, PendingAdds>) -> Vec<AddItem> {
    std::mem::take(&mut *pending.0.lock().unwrap_or_else(|e| e.into_inner()))
}

fn installer() -> CmdResult<Installer> {
    let host = rdm_host::install::sibling_host_path().map_err(msg)?;
    if !host.is_file() {
        return Err(format!(
            "rdm-host was not found next to the app ({}). Reinstall rdm, or build it \
             with `cargo build -p rdm-host` when running from source.",
            host.display()
        ));
    }
    Installer::new(host).map_err(msg)
}

fn browsers(
    browser: Option<String>,
    installer: &Installer,
    only_detected: bool,
) -> CmdResult<Vec<Browser>> {
    match browser {
        Some(b) => Ok(vec![Browser::parse(&b).map_err(msg)?]),
        None => Ok(Browser::ALL
            .into_iter()
            .filter(|b| !only_detected || installer.detected(*b))
            .collect()),
    }
}

#[tauri::command]
pub fn browser_integration_status() -> CmdResult<Vec<BrowserStatus>> {
    Ok(installer()?.status())
}

/// Registers the native messaging host with one browser, or with every
/// browser found on this system.
#[tauri::command]
pub fn install_browser_integration(browser: Option<String>) -> CmdResult<Vec<BrowserStatus>> {
    let installer = installer()?;
    for b in browsers(browser, &installer, true)? {
        installer.install(b).map_err(msg)?;
    }
    Ok(installer.status())
}

#[tauri::command]
pub fn uninstall_browser_integration(browser: Option<String>) -> CmdResult<Vec<BrowserStatus>> {
    let installer = installer()?;
    for b in browsers(browser, &installer, false)? {
        installer.uninstall(b).map_err(msg)?;
    }
    Ok(installer.status())
}

#[tauri::command]
pub async fn ffmpeg_status(manager: Mgr<'_>) -> CmdResult<FfmpegStatus> {
    Ok(manager.ffmpeg_status().await)
}

/// Downloads a static ffmpeg build. The UI asks for consent first, showing
/// the publisher, license and size from `ffmpeg_status`.
#[tauri::command]
pub async fn install_ffmpeg(manager: Mgr<'_>) -> CmdResult<FfmpegStatus> {
    manager.install_ffmpeg().await.map_err(msg)
}

/// Asks where to save, then writes the list (or the given downloads).
/// Returns the path, or `None` if the user cancelled.
#[tauri::command]
pub async fn export_list(
    app: AppHandle,
    manager: Mgr<'_>,
    format: ExportFormat,
    ids: Option<Vec<Id>>,
) -> CmdResult<Option<PathBuf>> {
    let (name, ext) = match format {
        ExportFormat::Json => ("rdm-downloads.json", "json"),
        ExportFormat::Urls => ("rdm-downloads.txt", "txt"),
    };
    let Some(path) = app
        .dialog()
        .file()
        .set_file_name(name)
        .add_filter(ext.to_uppercase(), &[ext])
        .blocking_save_file()
        .and_then(|p| p.into_path().ok())
    else {
        return Ok(None);
    };
    let text = manager.export(format, ids.as_deref());
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(path))
}

/// Asks for a file (rdm JSON or a list of URLs) and imports it.
#[tauri::command]
pub async fn import_list(app: AppHandle, manager: Mgr<'_>) -> CmdResult<Option<ImportReport>> {
    let Some(path) = app
        .dialog()
        .file()
        .add_filter("Download lists", &["json", "txt", "lst", "csv"])
        .blocking_pick_file()
        .and_then(|p| p.into_path().ok())
    else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(manager.import(&text, StartMode::Paused).await))
}

/// Imports dropped or pasted text: URLs go to the queue.
#[tauri::command]
pub async fn import_text(manager: Mgr<'_>, text: String) -> CmdResult<ImportReport> {
    Ok(manager.import(&text, StartMode::Queue).await)
}

#[tauri::command]
pub async fn compute_checksum(manager: Mgr<'_>, id: Id, algorithm: String) -> CmdResult<String> {
    let algorithm =
        Algorithm::parse(&algorithm).ok_or_else(|| format!("unknown algorithm `{algorithm}`"))?;
    manager.compute_checksum(id, algorithm).await.map_err(msg)
}

/// Saves every download's progress, then powers the computer off. The UI
/// only calls this after its cancellable countdown.
#[tauri::command]
pub async fn shutdown_computer(manager: Mgr<'_>) -> CmdResult<()> {
    manager.shutdown().await;
    rdm_manager::shutdown_computer().map_err(|e| format!("could not shut down: {e}"))
}
