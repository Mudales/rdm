//! Tauri commands: thin wrappers over `rdm_manager::Manager`.
//!
//! Errors cross to the UI as their full message chain.

use std::path::PathBuf;
use std::sync::Arc;

use rdm_host::install::{Browser, BrowserStatus, Installer};
use rdm_ipc::AddItem;
use rdm_manager::{
    Algorithm, DownloadRecord, ExportFormat, FILE_TYPES, FfmpegStatus, FileTypeGroup, Id,
    ImportReport, LiveProgress, Manager, NewDownload, ProbeResult, QueueState, Settings,
    SkippedType, StartMode, WebVideoStatus, default_extensions, error_chain,
};
use tauri::{AppHandle, State};

use crate::{PendingAdds, extension};
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
pub fn take_pending_adds(pending: State<'_, PendingAdds>) -> Vec<Vec<AddItem>> {
    std::mem::take(&mut *pending.0.lock().unwrap_or_else(|e| e.into_inner()))
}

/// File-type groups for the Capture settings, with the default selection.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureCatalog {
    groups: &'static [FileTypeGroup],
    defaults: Vec<String>,
}

#[tauri::command]
pub fn capture_catalog() -> CaptureCatalog {
    CaptureCatalog {
        groups: FILE_TYPES,
        defaults: default_extensions(),
    }
}

#[tauri::command]
pub fn skipped_types(manager: Mgr<'_>) -> Vec<SkippedType> {
    manager.skipped_types()
}

#[tauri::command]
pub fn ignore_skipped(manager: Mgr<'_>, extension: String) -> CmdResult<()> {
    manager.ignore_skipped(&extension).map_err(msg)
}

/// Adds the links chosen in the batch dialog.
#[tauri::command]
pub async fn add_batch(
    manager: Mgr<'_>,
    items: Vec<AddItem>,
    start: StartMode,
    queue_id: Option<u32>,
) -> CmdResult<Vec<Id>> {
    crate::ipc::add_all(manager.inner(), items, start, queue_id)
        .await
        .map_err(|e| e.to_string())
}

fn installer(app: &AppHandle) -> CmdResult<Installer> {
    use tauri::Manager as _;
    // An AppImage's own files vanish when it exits; a copy goes here.
    let stable = app.path().app_data_dir().map_err(msg)?.join("browser-host");
    let host = rdm_host::install::registrable_host_path(&stable).map_err(msg)?;
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
pub fn browser_integration_status(app: AppHandle) -> CmdResult<Vec<BrowserStatus>> {
    Ok(installer(&app)?.status())
}

/// Registers the native messaging host with one browser, or with every
/// browser found on this system.
#[tauri::command]
pub fn install_browser_integration(
    app: AppHandle,
    browser: Option<String>,
) -> CmdResult<Vec<BrowserStatus>> {
    let installer = installer(&app)?;
    for b in browsers(browser, &installer, true)? {
        installer.install(b).map_err(msg)?;
    }
    Ok(installer.status())
}

#[tauri::command]
pub fn uninstall_browser_integration(
    app: AppHandle,
    browser: Option<String>,
) -> CmdResult<Vec<BrowserStatus>> {
    let installer = installer(&app)?;
    for b in browsers(browser, &installer, false)? {
        installer.uninstall(b).map_err(msg)?;
    }
    Ok(installer.status())
}

#[tauri::command]
pub async fn web_video_status(manager: Mgr<'_>) -> CmdResult<WebVideoStatus> {
    Ok(manager.web_video_status().await)
}

/// Downloads yt-dlp (and Deno). The UI asks for consent first, showing the
/// sources, licenses and sizes.
#[tauri::command]
pub async fn install_web_video(manager: Mgr<'_>) -> CmdResult<WebVideoStatus> {
    manager.install_web_video().await.map_err(msg)
}

/// Updates rdm's own yt-dlp; returns the new version, if any.
#[tauri::command]
pub async fn update_web_video(manager: Mgr<'_>) -> CmdResult<Option<String>> {
    manager.update_web_video().await.map_err(msg)
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

// ---------------------------------------------------------------------------
// Setup assistant: the browser extension, yt-dlp and ffmpeg
// ---------------------------------------------------------------------------

/// One browser, as the setup assistant shows it.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSetup {
    browser: Browser,
    name: &'static str,
    /// The browser seems to be installed.
    detected: bool,
    /// The helper (`rdm-host`) is registered with it, for this copy of rdm.
    registered: bool,
    /// The folder to load the extension from.
    folder: PathBuf,
    /// The address of its extensions page.
    page: &'static str,
    /// The last time its extension said hello.
    seen: Option<extension::SeenExtension>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionStatus {
    /// The extension version this app ships.
    bundled_version: String,
    browsers: Vec<BrowserSetup>,
    /// Extensions seen in other browsers (e.g. Opera, Vivaldi).
    others: Vec<extension::SeenExtension>,
    /// The user chose not to see the setup assistant at startup.
    dismissed: bool,
}

const SETUP_DISMISSED_KEY: &str = "setup_dismissed";

#[tauri::command]
pub fn extension_status(app: AppHandle, manager: Mgr<'_>) -> CmdResult<ExtensionStatus> {
    let root = extension::root(&app)?;
    // Without rdm-host next to the app (a development build), the browsers
    // are still listed; registering them reports the problem.
    let status = match installer(&app) {
        Ok(i) => i.status(),
        Err(_) => rdm_host::install::sibling_host_path()
            .map_err(msg)
            .and_then(|host| Installer::new(host).map_err(msg))?
            .status(),
    };
    let mut seen = extension::seen(&manager);
    let browsers = status
        .into_iter()
        .map(|s| {
            let key = s.name.to_ascii_lowercase();
            let found = seen.iter().position(|x| x.browser == key);
            BrowserSetup {
                browser: s.browser,
                name: s.name,
                detected: s.detected,
                registered: s.installed && s.current,
                folder: extension::folder(&root, extension::Flavor::of(s.browser)),
                page: extension::extensions_page(s.browser),
                seen: found.map(|i| seen.remove(i)),
            }
        })
        .collect();
    Ok(ExtensionStatus {
        bundled_version: extension::bundled_version(),
        browsers,
        others: seen,
        dismissed: manager.ui_value(SETUP_DISMISSED_KEY).as_deref() == Some("true"),
    })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionSetup {
    folder: PathBuf,
    page: &'static str,
    /// The browser's extensions page was opened.
    opened: bool,
}

/// Everything rdm can do to add its extension to `browser`: registers the
/// helper, writes the extension folder, opens the browser's extensions page
/// and shows the folder. The user then turns on developer mode and loads the
/// folder (browsers allow nothing else for extensions outside their stores).
#[tauri::command]
pub fn setup_extension(app: AppHandle, browser: String) -> CmdResult<ExtensionSetup> {
    let browser = Browser::parse(&browser).map_err(msg)?;
    installer(&app)?.install(browser).map_err(msg)?;
    let folder = extension::folder(&extension::root(&app)?, extension::Flavor::of(browser));
    extension::write(&folder, extension::Flavor::of(browser))
        .map_err(|e| format!("cannot write the extension to {}: {e}", folder.display()))?;
    let page = extension::extensions_page(browser);
    let opened = match extension::open_in_browser(browser, page) {
        Ok(()) => true,
        Err(e) => {
            tracing::info!("cannot open {page}: {e}");
            false
        }
    };
    Ok(ExtensionSetup {
        folder,
        page,
        opened,
    })
}

/// Shows the extension folder in the file manager.
#[tauri::command]
pub fn reveal_extension_folder(app: AppHandle, browser: String) -> CmdResult<()> {
    let browser = Browser::parse(&browser).map_err(msg)?;
    let flavor = extension::Flavor::of(browser);
    let folder = extension::folder(&extension::root(&app)?, flavor);
    if extension::installed_version(&folder).is_none() {
        extension::write(&folder, flavor).map_err(msg)?;
    }
    app.opener().reveal_item_in_dir(&folder).map_err(msg)
}

/// Opens the browser's extensions page again.
#[tauri::command]
pub fn open_extensions_page(browser: String) -> CmdResult<()> {
    let browser = Browser::parse(&browser).map_err(msg)?;
    extension::open_in_browser(browser, extension::extensions_page(browser))
        .map_err(|e| format!("could not open {}: {e}", browser.name()))
}

/// Remembers whether the setup assistant opens at startup.
#[tauri::command]
pub fn set_setup_dismissed(manager: Mgr<'_>, dismissed: bool) -> CmdResult<()> {
    manager
        .set_ui_value(
            SETUP_DISMISSED_KEY,
            if dismissed { "true" } else { "false" },
        )
        .map_err(msg)
}
