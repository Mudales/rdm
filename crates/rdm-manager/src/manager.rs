//! The download manager: owns the engine, the store and all download state.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rdm_core::{
    Destination, DownloadControl, DownloadHandle, DownloadSpec, Engine, EngineConfig, Event,
    Outcome, Progress, Request, State as CoreState,
};
use tokio::runtime::Handle as Runtime;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::error::{Error, Result, error_chain};
use crate::model::{
    DownloadRecord, Id, LiveProgress, ManagerEvent, NewDownload, ProbeResult, QueueState,
    StartMode, Status,
};
use crate::settings::{MAIN_QUEUE, Settings};
use crate::store::Store;

const DATABASE_FILE: &str = "rdm.sqlite3";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);
/// Running downloads' byte counts are written to the database this often
/// (in progress ticks), so the list is roughly right after a crash. The
/// authoritative resume state is the `.rdm` sidecar either way.
const PERSIST_EVERY_TICKS: u32 = 20;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
const EVENT_CAPACITY: usize = 1024;

/// The download manager. Create with [`Manager::open`] inside a Tokio
/// runtime; after that every method may be called from any thread.
pub struct Manager {
    inner: Arc<Inner>,
    ticker: JoinHandle<()>,
}

impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Manager").finish_non_exhaustive()
    }
}

struct Inner {
    engine: Engine,
    store: Store,
    runtime: Runtime,
    events: broadcast::Sender<ManagerEvent>,
    state: Mutex<State>,
}

struct State {
    settings: Settings,
    records: BTreeMap<Id, DownloadRecord>,
    active: HashMap<Id, Active>,
    /// Queues currently allowed to start downloads. Not persisted: queues
    /// start stopped when the app starts.
    running_queues: HashSet<u32>,
    /// Cookies are secrets, so they live in memory only.
    cookies: HashMap<Id, String>,
    next_position: i64,
}

struct Active {
    control: DownloadControl,
    progress: Option<Progress>,
    then: AfterStop,
}

/// What to do once a running download's task ends because we stopped it.
#[derive(Debug, Clone, Copy)]
enum AfterStop {
    Stopped,
    /// Its queue was stopped: put it back in line.
    Queued,
    Remove {
        delete_file: bool,
    },
}

impl Manager {
    /// Opens (or creates) the database in `data_dir` and restores state.
    /// Downloads that were running when the app last exited become
    /// `Stopped`; nothing starts automatically.
    pub async fn open(data_dir: &Path, engine_config: EngineConfig) -> Result<Self> {
        let runtime = Runtime::try_current().map_err(|_| rdm_core::Error::NoRuntime)?;
        std::fs::create_dir_all(data_dir).map_err(Error::io(data_dir))?;
        let store = Store::open(&data_dir.join(DATABASE_FILE))?;

        let settings = match store.load_settings()? {
            Some(s) => s.validated().unwrap_or_else(|e| {
                tracing::warn!("stored settings are invalid ({e}); using defaults");
                Settings::default()
            }),
            None => {
                let s = Settings::default();
                store.save_settings(&s)?;
                s
            }
        };

        let mut records = BTreeMap::new();
        let mut next_position = 0;
        for mut r in store.load_downloads()? {
            let before = r.clone();
            if r.status == Status::Downloading {
                r.status = Status::Stopped;
            }
            if !r.is_finished()
                && let Ok(Some(partial)) = rdm_core::inspect_partial(&r.path())
            {
                r.downloaded = partial.downloaded;
            }
            if r != before {
                store.update(&r)?;
            }
            next_position = next_position.max(r.position + 1);
            records.insert(r.id, r);
        }

        let engine = Engine::new(EngineConfig {
            global_speed_limit: settings.global_speed_limit.and_then(NonZeroU64::new),
            ..engine_config
        });
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let inner = Arc::new(Inner {
            engine,
            store,
            runtime: runtime.clone(),
            events,
            state: Mutex::new(State {
                settings,
                records,
                active: HashMap::new(),
                running_queues: HashSet::new(),
                cookies: HashMap::new(),
                next_position,
            }),
        });
        let ticker = runtime.spawn(ticker(Arc::downgrade(&inner)));
        Ok(Self { inner, ticker })
    }

    /// Change notifications. Slow receivers skip events (`Lagged`) rather
    /// than slow downloads down; re-read with [`list`](Self::list) then.
    pub fn subscribe(&self) -> broadcast::Receiver<ManagerEvent> {
        self.inner.events.subscribe()
    }

    /// All downloads in list order.
    pub fn list(&self) -> Vec<DownloadRecord> {
        let st = self.inner.state();
        let mut v: Vec<_> = st.records.values().cloned().collect();
        v.sort_by_key(|r| r.position);
        v
    }

    pub fn get(&self, id: Id) -> Option<DownloadRecord> {
        self.inner.state().records.get(&id).cloned()
    }

    /// Latest progress of the running downloads.
    pub fn progress(&self) -> Vec<LiveProgress> {
        let st = self.inner.state();
        live_progress(&st)
    }

    pub fn settings(&self) -> Settings {
        self.inner.state().settings.clone()
    }

    /// Validates, saves and applies new settings. Downloads in a removed
    /// queue move to the main queue; downloads in a removed category lose
    /// their category (their folder does not change).
    pub fn update_settings(&self, settings: Settings) -> Result<Settings> {
        let settings = settings.validated().map_err(Error::InvalidSettings)?;
        let inner = &self.inner;
        inner.store.save_settings(&settings)?;
        inner
            .engine
            .set_global_speed_limit(settings.global_speed_limit.and_then(NonZeroU64::new));

        let mut guard = inner.state();
        let st = &mut *guard;
        st.settings = settings.clone();
        st.running_queues.retain(|q| settings.queue(*q).is_some());
        let mut changed = Vec::new();
        for r in st.records.values_mut() {
            let mut dirty = false;
            if settings.queue(r.queue_id).is_none() {
                r.queue_id = MAIN_QUEUE;
                dirty = true;
            }
            if r.category_id
                .is_some_and(|c| settings.category(c).is_none())
            {
                r.category_id = None;
                dirty = true;
            }
            if dirty {
                inner.save(r);
                changed.push(r.clone());
            }
        }
        inner.schedule(st);
        let queues = queue_states(st);
        drop(guard);

        for r in changed {
            inner.emit(ManagerEvent::Updated(r));
        }
        inner.emit(ManagerEvent::Settings(Box::new(settings.clone())));
        inner.emit(ManagerEvent::Queues(queues));
        Ok(settings)
    }

    pub fn queues(&self) -> Vec<QueueState> {
        queue_states(&self.inner.state())
    }

    /// Asks the server about a URL and suggests a file name (unique in its
    /// folder), category and folder, without adding anything.
    pub async fn probe(&self, new: &NewDownload) -> Result<ProbeResult> {
        let request = {
            let st = self.inner.state();
            build_request(
                &st.settings,
                &new.url,
                new.referer.as_deref(),
                new.user_agent.as_deref(),
                new.cookies.as_deref(),
            )?
        };
        let info = self.inner.engine.probe(&request).await?;
        let st = self.inner.state();
        let name = new
            .file_name
            .as_deref()
            .map(rdm_core::sanitize_filename)
            .unwrap_or_else(|| info.filename.clone());
        let (file_name, category_id, directory) = placement(&st, new, &name);
        Ok(ProbeResult {
            url: info.url.to_string(),
            final_url: info.final_url.to_string(),
            file_name,
            size: info.size,
            resumable: info.resumable,
            content_type: info.content_type,
            category_id,
            directory,
        })
    }

    /// Adds a download. Without a `file_name` the URL is probed first to get
    /// the server's name for it.
    pub async fn add(&self, new: NewDownload) -> Result<DownloadRecord> {
        let (name, size) = match &new.file_name {
            Some(name) => (rdm_core::sanitize_filename(name), None),
            None => {
                let p = self.probe(&new).await?;
                (p.file_name, p.size)
            }
        };
        Request::new(&new.url)?; // validate before storing

        let inner = &self.inner;
        let mut guard = inner.state();
        let st = &mut *guard;
        let (file_name, category_id, directory) = placement(st, &new, &name);
        let queue_id = new
            .queue_id
            .filter(|q| st.settings.queue(*q).is_some())
            .unwrap_or(MAIN_QUEUE);
        let mut record = DownloadRecord {
            id: 0,
            url: new.url.clone(),
            file_name,
            directory,
            category_id,
            queue_id,
            status: match new.start {
                StartMode::Queue => Status::Queued,
                StartMode::Now | StartMode::Paused => Status::Stopped,
            },
            error: None,
            size,
            downloaded: 0,
            resumable: None,
            referer: new.referer.clone(),
            user_agent: new.user_agent.clone(),
            connections: new.connections,
            speed_limit: new.speed_limit.filter(|l| *l > 0),
            added_at: now(),
            completed_at: None,
            position: st.next_position,
        };
        record.id = inner.store.insert(&record)?;
        st.next_position += 1;
        if let Some(c) = new.cookies.filter(|c| !c.trim().is_empty()) {
            st.cookies.insert(record.id, c);
        }
        st.records.insert(record.id, record.clone());
        inner.emit(ManagerEvent::Added(record.clone()));

        match new.start {
            StartMode::Now => inner.start_or_fail(st, record.id),
            StartMode::Queue => inner.schedule(st),
            StartMode::Paused => {}
        }
        let queues = queue_states(st);
        let record = st.records.get(&record.id).cloned().unwrap_or(record);
        drop(guard);
        inner.emit(ManagerEvent::Queues(queues));
        Ok(record)
    }

    /// Starts (or resumes, or retries) a download now, regardless of its
    /// queue's limit. Completed downloads are left alone.
    pub fn start(&self, id: Id) -> Result<()> {
        let mut guard = self.inner.state();
        let result = self.inner.start_locked(&mut guard, id);
        let queues = queue_states(&guard);
        drop(guard);
        self.inner.emit(ManagerEvent::Queues(queues));
        result
    }

    /// Stops a download, keeping its progress.
    pub fn stop(&self, id: Id) -> Result<()> {
        let inner = &self.inner;
        let mut guard = inner.state();
        let st = &mut *guard;
        if let Some(a) = st.active.get_mut(&id) {
            a.then = AfterStop::Stopped;
            a.control.stop();
            return Ok(());
        }
        let r = st.records.get_mut(&id).ok_or(Error::NotFound(id))?;
        if r.status == Status::Queued {
            r.status = Status::Stopped;
            inner.save(r);
            let r = r.clone();
            let queues = queue_states(st);
            drop(guard);
            inner.emit(ManagerEvent::Updated(r));
            inner.emit(ManagerEvent::Queues(queues));
        }
        Ok(())
    }

    /// Puts a download in line in its queue.
    pub fn enqueue(&self, id: Id) -> Result<()> {
        let inner = &self.inner;
        let mut guard = inner.state();
        let st = &mut *guard;
        if st.active.contains_key(&id) {
            return Ok(());
        }
        let r = st.records.get_mut(&id).ok_or(Error::NotFound(id))?;
        if r.status != Status::Completed && r.status != Status::Queued {
            r.status = Status::Queued;
            inner.save(r);
            inner.emit(ManagerEvent::Updated(r.clone()));
        }
        inner.schedule(st);
        let queues = queue_states(st);
        drop(guard);
        inner.emit(ManagerEvent::Queues(queues));
        Ok(())
    }

    /// Moves a download to another queue.
    pub fn move_to_queue(&self, id: Id, queue_id: u32) -> Result<()> {
        let inner = &self.inner;
        let mut guard = inner.state();
        let st = &mut *guard;
        if st.settings.queue(queue_id).is_none() {
            return Err(Error::QueueNotFound(queue_id));
        }
        let r = st.records.get_mut(&id).ok_or(Error::NotFound(id))?;
        r.queue_id = queue_id;
        inner.save(r);
        inner.emit(ManagerEvent::Updated(r.clone()));
        inner.schedule(st);
        let queues = queue_states(st);
        drop(guard);
        inner.emit(ManagerEvent::Queues(queues));
        Ok(())
    }

    /// Removes a download from the list. With `delete_file`, the file (or
    /// the partial file and its `.rdm` metadata) is deleted too.
    pub fn remove(&self, id: Id, delete_file: bool) -> Result<()> {
        let inner = &self.inner;
        let mut guard = inner.state();
        let st = &mut *guard;
        if let Some(a) = st.active.get_mut(&id) {
            a.then = AfterStop::Remove { delete_file };
            if delete_file {
                a.control.cancel();
            } else {
                a.control.stop();
            }
            return Ok(());
        }
        let r = st.records.remove(&id).ok_or(Error::NotFound(id))?;
        st.cookies.remove(&id);
        inner.store.delete(id)?;
        let queues = queue_states(st);
        drop(guard);
        if delete_file {
            remove_files(&r.path());
        }
        inner.emit(ManagerEvent::Removed(id));
        inner.emit(ManagerEvent::Queues(queues));
        Ok(())
    }

    /// Replaces the in-memory cookies for a download (e.g. fresh ones from
    /// the browser after a restart).
    pub fn set_cookies(&self, id: Id, cookies: Option<String>) -> Result<()> {
        let mut st = self.inner.state();
        if !st.records.contains_key(&id) {
            return Err(Error::NotFound(id));
        }
        match cookies.filter(|c| !c.trim().is_empty()) {
            Some(c) => st.cookies.insert(id, c),
            None => st.cookies.remove(&id),
        };
        Ok(())
    }

    /// Lets a queue start its waiting downloads, up to its limit.
    pub fn start_queue(&self, queue_id: u32) -> Result<()> {
        let inner = &self.inner;
        let mut guard = inner.state();
        let st = &mut *guard;
        if st.settings.queue(queue_id).is_none() {
            return Err(Error::QueueNotFound(queue_id));
        }
        st.running_queues.insert(queue_id);
        inner.schedule(st);
        let queues = queue_states(st);
        drop(guard);
        inner.emit(ManagerEvent::Queues(queues));
        Ok(())
    }

    /// Stops a queue; its running downloads stop and go back in line.
    pub fn stop_queue(&self, queue_id: u32) -> Result<()> {
        let inner = &self.inner;
        let mut guard = inner.state();
        let st = &mut *guard;
        if st.settings.queue(queue_id).is_none() {
            return Err(Error::QueueNotFound(queue_id));
        }
        st.running_queues.remove(&queue_id);
        for (id, a) in st.active.iter_mut() {
            if st.records.get(id).is_some_and(|r| r.queue_id == queue_id) {
                a.then = AfterStop::Queued;
                a.control.stop();
            }
        }
        let queues = queue_states(st);
        drop(guard);
        inner.emit(ManagerEvent::Queues(queues));
        Ok(())
    }

    /// Shows a message to the user (as a [`ManagerEvent::Notice`]).
    pub fn notice(&self, message: impl Into<String>) {
        self.inner.emit(ManagerEvent::Notice {
            id: None,
            message: message.into(),
        });
    }

    /// Stops every download (keeping progress) and waits for them to save
    /// their state, up to a few seconds. Call before the process exits.
    pub async fn shutdown(&self) {
        {
            let mut st = self.inner.state();
            st.running_queues.clear();
            for a in st.active.values_mut() {
                a.then = AfterStop::Stopped;
                a.control.stop();
            }
        }
        let deadline = tokio::time::Instant::now() + SHUTDOWN_GRACE;
        while !self.inner.state().active.is_empty() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.ticker.abort();
    }
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn emit(&self, event: ManagerEvent) {
        let _ = self.events.send(event);
    }

    /// Persists a record; failures are logged (used on background paths).
    fn save(&self, r: &DownloadRecord) {
        if let Err(e) = self.store.update(r) {
            tracing::error!(id = r.id, "saving download failed: {}", error_chain(&e));
        }
    }

    fn start_locked(self: &Arc<Self>, st: &mut State, id: Id) -> Result<()> {
        if st.active.contains_key(&id) {
            return Ok(());
        }
        let r = st.records.get(&id).ok_or(Error::NotFound(id))?;
        if r.status == Status::Completed {
            return Ok(());
        }
        let request = build_request(
            &st.settings,
            &r.url,
            r.referer.as_deref(),
            r.user_agent.as_deref(),
            st.cookies.get(&id).map(String::as_str),
        )?;
        let mut spec = DownloadSpec::new(request, Destination::File(r.path()));
        spec.connections =
            NonZeroUsize::new(r.connections.unwrap_or(st.settings.connections).into())
                .unwrap_or(NonZeroUsize::MIN);
        spec.min_split = st.settings.min_split;
        spec.speed_limit = r.speed_limit.and_then(NonZeroU64::new);

        let handle = {
            let _enter = self.runtime.enter();
            self.engine.start(spec)?
        };
        st.active.insert(
            id,
            Active {
                control: handle.control(),
                progress: None,
                then: AfterStop::Stopped,
            },
        );
        if let Some(r) = st.records.get_mut(&id) {
            r.status = Status::Downloading;
            r.error = None;
            self.save(r);
            self.emit(ManagerEvent::Updated(r.clone()));
        }
        let this = self.clone();
        self.runtime
            .spawn(async move { this.watch(id, handle).await });
        Ok(())
    }

    /// Starts a download; a failure to even start marks it failed.
    fn start_or_fail(self: &Arc<Self>, st: &mut State, id: Id) {
        if let Err(e) = self.start_locked(st, id)
            && let Some(r) = st.records.get_mut(&id)
        {
            r.status = Status::Failed;
            r.error = Some(error_chain(&e));
            self.save(r);
            self.emit(ManagerEvent::Updated(r.clone()));
        }
    }

    /// Starts waiting downloads in running queues while they have room.
    fn schedule(self: &Arc<Self>, st: &mut State) {
        let queues: Vec<(u32, usize)> = st
            .settings
            .queues
            .iter()
            .filter(|q| st.running_queues.contains(&q.id))
            .map(|q| (q.id, q.max_concurrent as usize))
            .collect();
        for (queue_id, limit) in queues {
            let running = st
                .active
                .keys()
                .filter(|id| st.records.get(id).is_some_and(|r| r.queue_id == queue_id))
                .count();
            let mut waiting: Vec<(i64, Id)> = st
                .records
                .values()
                .filter(|r| {
                    r.queue_id == queue_id
                        && r.status == Status::Queued
                        && !st.active.contains_key(&r.id)
                })
                .map(|r| (r.position, r.id))
                .collect();
            waiting.sort_unstable();
            for (_, id) in waiting.into_iter().take(limit.saturating_sub(running)) {
                self.start_or_fail(st, id);
            }
        }
    }

    async fn watch(self: Arc<Self>, id: Id, handle: DownloadHandle) {
        let mut rx = handle.subscribe();
        let this = self.clone();
        let pump = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(Event::Started {
                        info, segmented, ..
                    }) => this.on_started(id, info.size, segmented),
                    Ok(Event::Progress(p)) => this.on_progress(id, p),
                    Ok(Event::Warning(message)) => this.emit(ManagerEvent::Notice {
                        id: Some(id),
                        message,
                    }),
                    Ok(Event::State(
                        CoreState::Completed
                        | CoreState::Stopped
                        | CoreState::Cancelled
                        | CoreState::Failed(_),
                    )) => break,
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        let result = handle.wait().await;
        // The final state event is sent before `wait` returns; give the pump
        // a moment to apply anything still queued before it.
        let _ = tokio::time::timeout(Duration::from_millis(500), pump).await;
        self.on_finished(id, result);
    }

    fn on_started(&self, id: Id, size: Option<u64>, segmented: bool) {
        let mut st = self.state();
        if let Some(r) = st.records.get_mut(&id) {
            r.size = size;
            r.resumable = Some(segmented);
            self.save(r);
            self.emit(ManagerEvent::Updated(r.clone()));
        }
    }

    fn on_progress(&self, id: Id, p: Progress) {
        let mut guard = self.state();
        let st = &mut *guard;
        if let Some(r) = st.records.get_mut(&id) {
            r.downloaded = p.downloaded;
            if p.total.is_some() {
                r.size = p.total;
            }
        }
        if let Some(a) = st.active.get_mut(&id) {
            a.progress = Some(p);
        }
    }

    fn on_finished(self: &Arc<Self>, id: Id, result: rdm_core::Result<Outcome>) {
        let mut guard = self.state();
        let st = &mut *guard;
        let then = st.active.remove(&id).map_or(AfterStop::Stopped, |a| a.then);

        let mut removed = None;
        let mut updated = None;
        if let AfterStop::Remove { delete_file } = then {
            if let Some(r) = st.records.remove(&id) {
                st.cookies.remove(&id);
                if let Err(e) = self.store.delete(id) {
                    tracing::error!(id, "deleting download failed: {}", error_chain(&e));
                }
                removed = Some((r, delete_file));
            }
        } else if let Some(r) = st.records.get_mut(&id) {
            match result {
                Ok(Outcome::Completed { size, .. }) => {
                    r.status = Status::Completed;
                    r.size = Some(size);
                    r.downloaded = size;
                    r.completed_at = Some(now());
                    r.error = None;
                }
                Ok(Outcome::Stopped { downloaded, .. }) => {
                    r.status = match then {
                        AfterStop::Queued => Status::Queued,
                        _ => Status::Stopped,
                    };
                    if downloaded > 0 {
                        r.downloaded = downloaded;
                    }
                }
                Ok(Outcome::Cancelled) => {
                    r.status = Status::Stopped;
                    r.downloaded = 0;
                }
                Err(e) => {
                    r.status = Status::Failed;
                    r.error = Some(error_chain(&e));
                    r.downloaded = rdm_core::inspect_partial(&r.path())
                        .ok()
                        .flatten()
                        .map_or(r.downloaded, |p| p.downloaded);
                }
            }
            self.save(r);
            updated = Some(r.clone());
        }

        self.schedule(st);
        let queues = queue_states(st);
        drop(guard);

        if let Some((r, delete_file)) = removed {
            if delete_file {
                remove_files(&r.path());
            }
            self.emit(ManagerEvent::Removed(id));
        }
        if let Some(r) = updated {
            self.emit(ManagerEvent::Updated(r));
        }
        self.emit(ManagerEvent::Queues(queues));
    }

    fn tick(&self, persist: bool) {
        let mut guard = self.state();
        let st = &mut *guard;
        if st.active.is_empty() {
            return;
        }
        let items = live_progress(st);
        if persist {
            for id in st.active.keys() {
                if let Some(r) = st.records.get(id) {
                    self.save(r);
                }
            }
        }
        drop(guard);
        if !items.is_empty() {
            self.emit(ManagerEvent::Progress(items));
        }
    }
}

async fn ticker(inner: Weak<Inner>) {
    let mut interval = tokio::time::interval(PROGRESS_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut n = 0u32;
    loop {
        interval.tick().await;
        let Some(inner) = inner.upgrade() else { return };
        n = n.wrapping_add(1);
        inner.tick(n.is_multiple_of(PERSIST_EVERY_TICKS));
    }
}

fn live_progress(st: &State) -> Vec<LiveProgress> {
    st.active
        .iter()
        .filter_map(|(id, a)| {
            let p = a.progress.as_ref()?;
            Some(LiveProgress {
                id: *id,
                downloaded: p.downloaded,
                total: p.total,
                speed: p.speed,
                eta_secs: p.eta.map(|d| d.as_secs()),
                connections: p.connections,
                segments: p
                    .segments
                    .iter()
                    .map(|s| [s.start, s.position, s.end])
                    .collect(),
            })
        })
        .collect()
}

fn queue_states(st: &State) -> Vec<QueueState> {
    st.settings
        .queues
        .iter()
        .map(|q| {
            let in_queue = |r: &&DownloadRecord| r.queue_id == q.id;
            QueueState {
                id: q.id,
                name: q.name.clone(),
                max_concurrent: q.max_concurrent,
                running: st.running_queues.contains(&q.id),
                active: st
                    .records
                    .values()
                    .filter(in_queue)
                    .filter(|r| st.active.contains_key(&r.id))
                    .count(),
                waiting: st
                    .records
                    .values()
                    .filter(in_queue)
                    .filter(|r| r.status == Status::Queued && !st.active.contains_key(&r.id))
                    .count(),
            }
        })
        .collect()
}

fn build_request(
    settings: &Settings,
    url: &str,
    referer: Option<&str>,
    user_agent: Option<&str>,
    cookies: Option<&str>,
) -> Result<Request> {
    let mut r = Request::new(url)?.proxy(settings.proxy_config());
    if let Some(v) = referer.filter(|v| !v.is_empty()) {
        r = r.referer(v);
    }
    if let Some(v) = user_agent.filter(|v| !v.is_empty()) {
        r = r.user_agent(v);
    }
    if let Some(v) = cookies.filter(|v| !v.is_empty()) {
        r = r.cookies(v);
    }
    Ok(r)
}

/// Final file name (unique in its folder), category and folder.
fn placement(st: &State, new: &NewDownload, name: &str) -> (String, Option<u32>, PathBuf) {
    let settings = &st.settings;
    let category_id = new
        .category_id
        .filter(|c| settings.category(*c).is_some())
        .or_else(|| settings.category_for(name).map(|c| c.id));
    let directory = new
        .directory
        .clone()
        .unwrap_or_else(|| settings.directory_for(category_id));
    let file_name = unique_name(&st.records, &directory, name);
    (file_name, category_id, directory)
}

/// `name`, or `stem (n).ext` if a file, a partial download or another
/// download in the list already uses that path.
fn unique_name(records: &BTreeMap<Id, DownloadRecord>, dir: &Path, name: &str) -> String {
    let taken = |candidate: &str| {
        let path = dir.join(candidate);
        path.exists()
            || rdm_core::sidecar_path(&path).exists()
            || records
                .values()
                .any(|r| r.directory == dir && r.file_name == candidate)
    };
    if !taken(name) {
        return name.to_owned();
    }
    let p = Path::new(name);
    let stem = p
        .file_stem()
        .map_or_else(|| name.into(), |s| s.to_string_lossy().into_owned());
    let ext = p.extension().map(|e| e.to_string_lossy().into_owned());
    (1..10_000)
        .map(|i| match &ext {
            Some(ext) => format!("{stem} ({i}).{ext}"),
            None => format!("{stem} ({i})"),
        })
        .find(|c| !taken(c))
        .unwrap_or_else(|| format!("{stem}-{}", now()))
}

/// Deletes a file and its resume metadata; missing files are fine.
fn remove_files(path: &Path) {
    for p in [path.to_path_buf(), rdm_core::sidecar_path(path)] {
        if let Err(e) = std::fs::remove_file(&p)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("could not delete {}: {e}", p.display());
        }
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
