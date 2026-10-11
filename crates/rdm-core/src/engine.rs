//! The public entry points: [`Engine`] and [`DownloadHandle`].

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{Semaphore, broadcast, watch};
use tokio::task::JoinHandle;

use crate::config::{DownloadSpec, EngineConfig, Request};
use crate::download::{self, Control, Ctx};
use crate::error::{Error, Result};
use crate::events::{DownloadId, Event, Outcome};
use crate::http::HttpClient;
use crate::probe::{self, ProbeInfo};
use crate::throttle::TokenBucket;

const EVENT_CAPACITY: usize = 256;

/// Owns engine-wide state: the global speed limit and per-host connection
/// caps. Cheap to clone; clones share that state.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

struct Inner {
    config: EngineConfig,
    global: Arc<TokenBucket>,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
    next_id: AtomicU64,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        let global = Arc::new(TokenBucket::new(config.global_speed_limit));
        Self {
            inner: Arc::new(Inner {
                config,
                global,
                hosts: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
            }),
        }
    }

    pub fn config(&self) -> &EngineConfig {
        &self.inner.config
    }

    /// Changes the total bandwidth cap for all downloads, effective at once.
    pub fn set_global_speed_limit(&self, limit: Option<NonZeroU64>) {
        self.inner.global.set_rate(limit);
    }

    pub fn global_speed_limit(&self) -> Option<NonZeroU64> {
        self.inner.global.rate()
    }

    /// Asks the server about a resource without downloading it.
    pub async fn probe(&self, request: &Request) -> Result<ProbeInfo> {
        let client = HttpClient::new(&self.inner.config, request, 1)?;
        let host = self.host_limiter(&client.host_key());
        let _permit = host
            .acquire()
            .await
            .map_err(|_| Error::Task("connection limiter closed".into()))?;
        Ok(probe::probe(&client).await?.0)
    }

    /// Starts (or resumes, if a `.rdm` file is found) a download in the
    /// background. Must be called from within a Tokio runtime.
    ///
    /// Dropping the handle does not stop the download.
    pub fn start(&self, spec: DownloadSpec) -> Result<DownloadHandle> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| Error::NoRuntime)?;
        let host = self.host_limiter(&crate::http::host_key(&spec.request.url));
        let (control, _) = watch::channel(Control::Run);
        let control = Arc::new(control);
        let (events, first_rx) = broadcast::channel(EVENT_CAPACITY);
        let limit = Arc::new(TokenBucket::new(spec.speed_limit));
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);

        let ctx = Arc::new(Ctx {
            config: self.inner.config.clone(),
            spec,
            control: control.clone(),
            events: events.clone(),
            limit: limit.clone(),
            global: self.inner.global.clone(),
            host,
        });
        let task = runtime.spawn(download::run(ctx));
        Ok(DownloadHandle {
            control: DownloadControl {
                id,
                control,
                events,
                limit,
            },
            first_rx: Mutex::new(Some(first_rx)),
            task,
        })
    }

    fn host_limiter(&self, key: &str) -> Arc<Semaphore> {
        let mut hosts = self.inner.hosts.lock().unwrap_or_else(|e| e.into_inner());
        hosts
            .entry(key.to_owned())
            .or_insert_with(|| {
                Arc::new(Semaphore::new(
                    self.inner.config.max_connections_per_host.get(),
                ))
            })
            .clone()
    }
}

/// Owns one running download: control it and [`wait`](Self::wait) for it.
///
/// Dropping the handle does not stop the download.
#[derive(Debug)]
pub struct DownloadHandle {
    control: DownloadControl,
    first_rx: Mutex<Option<broadcast::Receiver<Event>>>,
    task: JoinHandle<Result<Outcome>>,
}

impl DownloadHandle {
    pub fn id(&self) -> DownloadId {
        self.control.id
    }

    /// A cloneable controller, usable while another task awaits
    /// [`wait`](Self::wait).
    pub fn control(&self) -> DownloadControl {
        self.control.clone()
    }

    /// Subscribes to events. The first subscriber also receives every event
    /// emitted since [`Engine::start`]; later ones only see new events.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.first_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(|| self.control.subscribe())
    }

    /// See [`DownloadControl::pause`].
    pub fn pause(&self) {
        self.control.pause();
    }

    /// See [`DownloadControl::resume`].
    pub fn resume(&self) {
        self.control.resume();
    }

    /// See [`DownloadControl::stop`].
    pub fn stop(&self) {
        self.control.stop();
    }

    /// See [`DownloadControl::cancel`].
    pub fn cancel(&self) {
        self.control.cancel();
    }

    /// See [`DownloadControl::set_speed_limit`].
    pub fn set_speed_limit(&self, limit: Option<NonZeroU64>) {
        self.control.set_speed_limit(limit);
    }

    /// Waits for the download to end.
    pub async fn wait(self) -> Result<Outcome> {
        self.task.await.map_err(Error::task)?
    }
}

/// Cloneable remote control for a download.
#[derive(Debug, Clone)]
pub struct DownloadControl {
    id: DownloadId,
    control: Arc<watch::Sender<Control>>,
    events: broadcast::Sender<Event>,
    limit: Arc<TokenBucket>,
}

impl DownloadControl {
    pub fn id(&self) -> DownloadId {
        self.id
    }

    /// Subscribes to events emitted from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Suspends transfers, keeping the task alive so [`resume`](Self::resume)
    /// continues quickly. Progress is checkpointed.
    pub fn pause(&self) {
        self.control.send_if_modified(|c| {
            let change = *c == Control::Run;
            if change {
                *c = Control::Pause;
            }
            change
        });
    }

    /// Continues a paused download.
    pub fn resume(&self) {
        self.control.send_if_modified(|c| {
            let change = *c == Control::Pause;
            if change {
                *c = Control::Run;
            }
            change
        });
    }

    /// Ends the task, keeping the partial file and its `.rdm` metadata so a
    /// later [`Engine::start`] with the same destination resumes.
    pub fn stop(&self) {
        self.control.send_if_modified(|c| {
            let change = matches!(*c, Control::Run | Control::Pause);
            if change {
                *c = Control::Stop;
            }
            change
        });
    }

    /// Ends the task and deletes the partial file and its metadata.
    pub fn cancel(&self) {
        self.control.send_replace(Control::Cancel);
    }

    /// Changes this download's bandwidth cap, effective at once.
    pub fn set_speed_limit(&self, limit: Option<NonZeroU64>) {
        self.limit.set_rate(limit);
    }
}
