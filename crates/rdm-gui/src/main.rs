//! `rdm-gui` — the rdm desktop app (Tauri v2).
//!
//! A thin shell: all download logic lives in `rdm-manager`. This crate wires
//! manager calls to Tauri commands, forwards manager events to the web UI,
//! and owns the window, tray and single-instance behaviour.

// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod tray;

use std::sync::Arc;

use anyhow::Context;
use rdm_core::EngineConfig;
use rdm_manager::Manager;
use tauri::async_runtime::block_on;
use tauri::{AppHandle, Emitter, Manager as _, RunEvent, WindowEvent};
use tokio::sync::broadcast::error::RecvError;
use tracing_subscriber::EnvFilter;

/// Event name the UI listens on; the payload is a `ManagerEvent`.
const EVENT: &str = "rdm://event";
/// Tells the UI it missed events and should reload the full state.
const RESYNC: &str = "rdm://resync";

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
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .context("cannot determine the app data directory")?;
            let manager = block_on(Manager::open(&data_dir, EngineConfig::default()))
                .with_context(|| format!("cannot open the database in {}", data_dir.display()))?;
            let manager = Arc::new(manager);
            forward_events(app.handle().clone(), &manager);
            app.manage(manager);
            tray::create(app.handle())?;
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
