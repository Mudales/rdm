//! Tauri commands: thin wrappers over `rdm_manager::Manager`.
//!
//! Errors cross to the UI as their full message chain.

use std::path::PathBuf;
use std::sync::Arc;

use rdm_manager::{
    DownloadRecord, Id, LiveProgress, Manager, NewDownload, ProbeResult, QueueState, Settings,
    error_chain,
};
use tauri::{AppHandle, State};
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
