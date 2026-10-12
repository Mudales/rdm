//! `rdm-gui` — the rdm desktop app (Tauri v2).
//!
//! A thin shell: all download logic lives in `rdm-manager`. This crate wires
//! manager calls to Tauri commands, forwards manager events to the web UI,
//! and owns the window, tray and single-instance behaviour.

// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clipboard;
mod commands;
mod ipc;
mod tray;
mod update;

use std::sync::{Arc, Mutex};

use anyhow::Context;
use rdm_core::EngineConfig;
use rdm_ipc::AddItem;
use rdm_manager::{Manager, ManagerEvent, WhenDone};
use tauri::async_runtime::block_on;
use tauri::{AppHandle, Emitter, Manager as _, RunEvent, WindowEvent};
use tokio::sync::broadcast::error::RecvError;
use tracing_subscriber::EnvFilter;

/// Event name the UI listens on; the payload is a `ManagerEvent`.
const EVENT: &str = "rdm://event";
/// Tells the UI it missed events and should reload the full state.
const RESYNC: &str = "rdm://resync";
/// Tells the UI that add requests (from the browser or the clipboard) are
/// waiting; it fetches them with the `take_pending_adds` command.
pub(crate) const ADD_REQUEST: &str = "rdm://add-request";
/// Asks the UI to open a view; the payload is an `rdm_ipc::AppView`.
pub(crate) const SHOW_VIEW: &str = "rdm://show-view";

/// Add requests waiting for the UI, each a list of items: one item opens
/// the Add dialog, several the batch dialog. Kept here rather than sent as
/// event payloads so that a request arriving before the web UI has loaded
/// (e.g. the browser just launched the app) is not lost.
#[derive(Default)]
pub(crate) struct PendingAdds(pub Mutex<Vec<Vec<AddItem>>>);

/// Queues an add request for confirmation in the UI and brings it forward.
pub(crate) fn request_add(app: &AppHandle, items: Vec<AddItem>) {
    if items.is_empty() {
        return;
    }
    if let Some(pending) = app.try_state::<PendingAdds>() {
        pending
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(items);
    }
    show_main_window(app);
    let _ = app.emit(ADD_REQUEST, ());
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("RDM_LOG").unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();
    if let Err(e) = run() {
        tracing::error!("{e:#}");
        eprintln!("rdm: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let app = tauri::Builder::default()
        // Must be registered first: a second launch focuses this instance.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(PendingAdds::default())
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .context("cannot determine the app data directory")?;
            let manager = block_on(Manager::open(&data_dir, EngineConfig::default()))
                .with_context(|| format!("cannot open the database in {}", data_dir.display()))?;
            let manager = Arc::new(manager);
            forward_events(app.handle().clone(), &manager);
            match block_on(ipc::start(app.handle().clone(), manager.clone())) {
                Ok(server) => {
                    app.manage(ipc::IpcServer(server));
                }
                // The browser extension will not reach this instance, but
                // everything else works.
                Err(e) => tracing::warn!("browser integration unavailable: {e}"),
            }
            clipboard::spawn(app.handle().clone(), manager.clone());
            app.manage(manager);
            tray::create(app.handle())?;
            update::spawn_daily_check(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let to_tray = window
                    .try_state::<Arc<Manager>>()
                    .is_some_and(|m| m.settings().close_to_tray);
                if to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_downloads,
            commands::live_progress,
            commands::get_settings,
            commands::update_settings,
            commands::get_queues,
            commands::probe_url,
            commands::add_download,
            commands::start_download,
            commands::stop_download,
            commands::enqueue_download,
            commands::remove_download,
            commands::start_queue,
            commands::stop_queue,
            commands::open_file,
            commands::show_in_folder,
            commands::pick_folder,
            commands::take_pending_adds,
            commands::browser_integration_status,
            commands::install_browser_integration,
            commands::uninstall_browser_integration,
            commands::ffmpeg_status,
            commands::install_ffmpeg,
            commands::export_list,
            commands::import_list,
            commands::import_text,
            commands::compute_checksum,
            commands::shutdown_computer,
            commands::capture_catalog,
            commands::skipped_types,
            commands::ignore_skipped,
            commands::add_batch,
            update::app_info,
            update::check_update,
            update::install_update,
            update::open_releases_page,
        ])
        .build(tauri::generate_context!())
        .context("failed to start the user interface")?;

    app.run(|app, event| match event {
        // Save every running download's progress before the process ends.
        RunEvent::Exit => {
            if let Some(manager) = app.try_state::<Arc<Manager>>() {
                block_on(manager.shutdown());
            }
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show_main_window(app),
        _ => {}
    });
    Ok(())
}

/// Relays manager events to the web UI.
fn forward_events(app: AppHandle, manager: &Manager) {
    let mut rx = manager.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = app.emit(EVENT, &event);
                    match &event {
                        // "Quit when done": exit (progress is saved on Exit).
                        ManagerEvent::QueueFinished {
                            when_done: WhenDone::Quit,
                            ..
                        } => app.exit(0),
                        // The UI shows a cancellable shutdown countdown.
                        ManagerEvent::QueueFinished {
                            when_done: WhenDone::Shutdown,
                            ..
                        } => show_main_window(&app),
                        _ => {}
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    let _ = app.emit(RESYNC, ());
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

pub(crate) fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}
