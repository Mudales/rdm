//! `rdm-manager` — the download manager behind the rdm front ends.
//!
//! Where `rdm-core` downloads one file, this crate manages a *list* of
//! downloads: it persists them in SQLite, files them into categories with
//! their own folders, runs queues with concurrency limits, keeps settings,
//! and publishes [`ManagerEvent`]s for a UI to render. It has no GUI
//! dependency, so the Tauri app, the native messaging host and the tests
//! all drive the same code.
//!
//! ```no_run
//! # async fn demo() -> rdm_manager::Result<()> {
//! use rdm_manager::{Manager, NewDownload};
//!
//! let manager = Manager::open("data".as_ref(), Default::default()).await?;
//! let mut events = manager.subscribe();
//! manager
//!     .add(NewDownload { url: "https://example.com/file.zip".into(), ..Default::default() })
//!     .await?;
//! while let Ok(event) = events.recv().await {
//!     println!("{event:?}");
//! }
//! # Ok(()) }
//! ```

mod antivirus;
mod capture;
mod checksum;
mod error;
mod manager;
mod model;
mod power;
mod schedule;
mod settings;
mod store;
mod transfer;

pub use capture::{
    CaptureDecision, CaptureSettings, FILE_TYPES, FileTypeGroup, SkippedType, default_extensions,
};
pub use checksum::{Algorithm, Checksum, hash_file};
pub use error::{Error, Result, error_chain};
pub use manager::{FfmpegStatus, Manager, ToolState, WebVideoStatus};
pub use model::{
    DownloadRecord, Id, LiveProgress, ManagerEvent, NewDownload, ProbeResult, QueueState,
    StartMode, Status, StreamChoice,
};
pub use power::shutdown_computer;
pub use rdm_media::{FfmpegConfig, Manifest, OutputFormat};
pub use settings::{
    Category, MAIN_QUEUE, ProxySetting, Queue, Schedule, Settings, WebVideoSettings, WhenDone,
};
pub use transfer::{ExportFormat, ImportReport};
