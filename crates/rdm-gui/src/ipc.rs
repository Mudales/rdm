//! Answers requests relayed by `rdm-host` from the browser extension.

use std::sync::Arc;

use rdm_ipc::{
    AddItem, AddMode, AppView, Endpoint, FormatChoice, PROTOCOL_VERSION, Request, Response, Server,
};
use rdm_manager::{Error, Manager, NewDownload, StartMode, StreamChoice, error_chain};
use tauri::{AppHandle, Emitter};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::{SHOW_VIEW, extension, request_add, show_main_window};

/// Batch adds probe each URL; this many at once.
const PARALLEL_ADDS: usize = 6;

/// Keeps the server alive for the app's lifetime.
pub struct IpcServer(#[allow(dead_code)] pub Server);

pub async fn start(app: AppHandle, manager: Arc<Manager>) -> rdm_ipc::Result<Server> {
    Server::start(Endpoint::for_current_user(), move |request| {
        let app = app.clone();
        let manager = manager.clone();
        async move { handle(&app, &manager, request).await }
    })
    .await
}

async fn handle(app: &AppHandle, manager: &Arc<Manager>, request: Request) -> Response {
    match request {
        Request::Ping => Response::Pong {
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL_VERSION,
            extension: None,
        },
        Request::Hello { extension } => {
            extension::note_hello(app, manager, &extension);
            Response::Pong {
                version: env!("CARGO_PKG_VERSION").into(),
                protocol: PROTOCOL_VERSION,
                extension: Some(extension::bundled_version()),
            }
        }
        Request::Formats { item } => formats(manager, *item).await,
        Request::Add { items, .. } if items.is_empty() => Response::error("nothing to add"),
        // The user confirms: one item opens the prefilled Add dialog,
        // several the batch dialog.
        Request::Add {
            items,
            mode: AddMode::Ask,
        } => {
            request_add(app, items);
            Response::Shown
        }
        Request::Add { items, mode } => {
            let start = match mode {
                AddMode::Now => StartMode::Now,
                AddMode::Queue | AddMode::Ask => StartMode::Queue,
            };
            match add_all(manager, items, start, None).await {
                Ok(ids) => Response::Added { ids },
                Err(e) => Response::error(e),
            }
        }
        Request::GetCaptureRules => rules(manager),
        Request::ReportSkipped { extension } => match manager.note_skipped(&extension) {
            Ok(()) => Response::Ok,
            Err(e) => Response::error(error_chain(&e)),
        },
        Request::SetSiteCapture { host, capture } => {
            match manager.set_site_capture(&host, capture) {
                Ok(_) => rules(manager),
                Err(e) => Response::error(error_chain(&e)),
            }
        }
        Request::Show { view } => {
            show_main_window(app);
            if view != AppView::Downloads {
                let _ = app.emit(SHOW_VIEW, view);
            }
            Response::Shown
        }
    }
}

/// The qualities offered on the page: yt-dlp's for a web page, the
/// playlist's for a stream. A plain file has none (it is downloaded as is).
async fn formats(manager: &Manager, item: AddItem) -> Response {
    let new = to_new_download(item, StartMode::Paused);
    let probe = match manager.probe(&new).await {
        Ok(p) => p,
        Err(Error::Media(rdm_media::Error::YtDlpMissing)) => {
            return Response::ToolMissing {
                tool: "yt-dlp".into(),
                message: "yt-dlp is needed to find the video on this page.".into(),
            };
        }
        Err(e) => return Response::error(error_chain(&e)),
    };
    if let Some(site) = probe.site {
        let choices = site
            .choices
            .into_iter()
            .map(|c| FormatChoice {
                id: c.id,
                label: c.label,
                audio_only: c.audio_only,
                size: c.size,
            })
            .collect();
        return Response::Formats {
            title: Some(site.title),
            choices,
        };
    }
    if let Some(stream) = probe.stream {
        let choices = stream
            .variants
            .into_iter()
            .map(|v| FormatChoice {
                id: v.id,
                label: v.label,
                audio_only: false,
                size: None,
            })
            .collect();
        return Response::Formats {
            title: new.title,
            choices,
        };
    }
    if probe.page {
        return Response::ToolMissing {
            tool: "yt-dlp".into(),
            message: "This is a web page; yt-dlp is needed to find its video.".into(),
        };
    }
    Response::Formats {
        title: None,
        choices: Vec::new(),
    }
}

fn rules(manager: &Manager) -> Response {
    match serde_json::to_value(manager.settings().capture) {
        Ok(rules) => Response::CaptureRules { rules },
        Err(e) => Response::error(e.to_string()),
    }
}

/// Adds several downloads (probing a few at a time). Fails only when none
/// could be added; partial failures are reported as a notice.
pub(crate) async fn add_all(
    manager: &Arc<Manager>,
    items: Vec<AddItem>,
    start: StartMode,
    queue_id: Option<u32>,
) -> Result<Vec<i64>, String> {
    let limit = Arc::new(Semaphore::new(PARALLEL_ADDS));
    let mut tasks = JoinSet::new();
    for (index, item) in items.into_iter().enumerate() {
        let manager = manager.clone();
        let limit = limit.clone();
        tasks.spawn(async move {
            let _permit = limit.acquire_owned().await;
            let url = item.url.clone();
            let mut new = to_new_download(item, start);
            new.queue_id = queue_id;
            let result = manager.add(new).await;
            (index, url, result)
        });
    }
    let mut added = Vec::new();
    let mut failures = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((index, _, Ok(record))) => added.push((index, record.id)),
            Ok((_, url, Err(e))) => failures.push(format!("{url}: {}", error_chain(&e))),
            Err(e) => failures.push(e.to_string()),
        }
    }
    added.sort_unstable();
    if added.is_empty() {
        return Err(failures
            .into_iter()
            .next()
            .unwrap_or_else(|| "nothing to add".into()));
    }
    if !failures.is_empty() {
        manager.notice(format!(
            "{} of {} links could not be added: {}",
            failures.len(),
            failures.len() + added.len(),
            failures.join("; ")
        ));
    }
    Ok(added.into_iter().map(|(_, id)| id).collect())
}

pub(crate) fn to_new_download(item: AddItem, start: StartMode) -> NewDownload {
    // A quality picked on the page, if any.
    let chosen = StreamChoice {
        variant: item.variant.filter(|v| !v.is_empty()),
        label: item.label,
        format: item
            .format
            .and_then(|f| serde_json::from_value(serde_json::Value::String(f)).ok())
            .unwrap_or_default(),
        ..StreamChoice::default()
    };
    let stream = match item.kind.as_deref() {
        Some("hls" | "dash") => Some(chosen),
        // A web page whose video yt-dlp finds.
        Some("page") => Some(StreamChoice {
            site: true,
            ..chosen
        }),
        _ => None,
    };
    NewDownload {
        stream,
        title: item.title,
        url: item.url,
        file_name: item.file_name.filter(|n| !n.trim().is_empty()),
        referer: item.referer.or(item.page_url),
        user_agent: item.user_agent,
        cookies: item.cookies,
        start,
        ..NewDownload::default()
    }
}
