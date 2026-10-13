//! `cv daemon`: startup, shared state, request handling, event bridge, spool drain, shutdown.
//!
//! Startup order (brief pin 4): config → (shadow) scratch + Landlock → `StateDb` → `VaultFs` →
//! `VaultWriter::open` → writer+watcher thread — all on the calling thread, before any tokio
//! runtime exists ([`Daemon::start`]) — then, inside a runtime, bind the socket → drain the spool
//! → serve ([`Daemon::spawn`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use cv_protocol::{
    ChangeKind, CreateNoteResult, HelloResult, JobState, JobStatus, Mode, Notification, Origin,
    PROTOCOL_VERSION, Request, RpcError, ServiceHealth, StatusResult, UriResult, ViewsListResult,
    WriteNoteResult, WriteOp, error_codes,
};
use cv_state::{NewShadowWrite, QueueConfig, StateDb, StateError, StateReader};
use cv_vault::{
    ContentHash, EventKind, NoCutover, ShadowLog, VaultError, VaultEvent, VaultFs, VaultWriter,
    WatchConfig, WriterHandle,
};
use serde_json::{Value, json};
use tokio::sync::{broadcast, watch};

use crate::config::Config;
use crate::uri::{self, UriAction};
use crate::views::{ViewError, ViewRegistry, register_views};
use crate::{now_ms, spool};

/// Broadcast capacity for notifications (a lagging subscriber skips, never blocks).
const EVENT_CAPACITY: usize = 1024;
/// Upper bound on the ` N` collision suffix search.
const MAX_SUFFIX: u32 = 10_000;

/// Daemon start options.
#[derive(Debug, Clone, Copy)]
pub struct DaemonOptions {
    /// Apply [`cv_vault::restrict_shadow`] in shadow mode (true from `cv daemon`; in-process
    /// tests pass false because Landlock would restrict the whole test process).
    pub landlock: bool,
    /// Watcher timings.
    pub watch: WatchConfig,
    /// `CV_CONFIG_DIR` poll interval (2 s in production).
    pub config_poll: Duration,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            landlock: true,
            watch: WatchConfig::default(),
            config_poll: Duration::from_secs(2),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The writer's [`ShadowLog`]: appends through the daemon's single shared `StateDb` and broadcasts
/// `shadow_write{op, path}`. The lock is held only for the append.
struct SharedShadowLog {
    db: Arc<Mutex<StateDb>>,
    events: broadcast::Sender<Notification>,
}

impl ShadowLog for SharedShadowLog {
    fn record(&mut self, w: &NewShadowWrite) -> cv_vault::Result<()> {
        lock(&self.db).append_shadow_write(w)?;
        let op = match w.op.as_str() {
            "create" => WriteOp::Create,
            "delete" => WriteOp::Delete,
            "rename_from" | "rename_to" => WriteOp::Rename,
            _ => WriteOp::Write,
        };
        let _ = self.events.send(Notification::ShadowWrite {
            op,
            path: w.path.clone(),
        });
        Ok(())
    }
}

/// State shared by every connection and background task.
pub struct Shared {
    config: Config,
    vault_root: PathBuf,
    writer: RwLock<Option<WriterHandle>>,
    state: Arc<Mutex<StateDb>>,
    events: broadcast::Sender<Notification>,
    views: ViewRegistry,
    open_subscribers: AtomicUsize,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("vault_root", &self.vault_root)
            .finish_non_exhaustive()
    }
}

/// A started daemon (writer running, not yet serving).
pub struct Daemon {
    shared: Arc<Shared>,
    writer_join: JoinHandle<()>,
    vault_events: std::sync::mpsc::Receiver<VaultEvent>,
    opts: DaemonOptions,
}

impl std::fmt::Debug for Daemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Daemon").finish_non_exhaustive()
    }
}

impl Daemon {
    /// Synchronous startup (see the module docs). Must run before any tokio runtime or other
    /// thread exists when `opts.landlock` is set.
    pub fn start(config: Config, opts: DaemonOptions) -> Result<Self> {
        config.create_dirs()?;
        if config.mode == Mode::Shadow {
            let repo_roots: Vec<PathBuf> = cv_vault::DEFAULT_REPO_ROOTS
                .iter()
                .map(PathBuf::from)
                .collect();
            let scratch = cv_vault::validate_scratch(&config.scratch, &config.vault, &repo_roots)
                .context("validating CV_SCRATCH")?;
            if opts.landlock {
                let writable = vec![
                    scratch,
                    config.state_dir(),
                    config.index_dir(),
                    config.socket_dir(),
                    config.config_dir.clone(),
                ];
                let status = cv_vault::restrict_shadow(&config.vault, &writable)
                    .context("applying Landlock")?;
                tracing::info!(?status, "landlock");
            }
        }
        let state = Arc::new(Mutex::new(
            StateDb::open(&config.state_db, now_ms(), QueueConfig::default())
                .with_context(|| format!("opening {}", config.state_db.display()))?,
        ));
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let fs = match config.mode {
            Mode::Live => VaultFs::live(&config.vault, &config.protected_vault, &NoCutover)
                .context("live mode refused")?,
            Mode::Shadow => {
                let repo_roots: Vec<PathBuf> = cv_vault::DEFAULT_REPO_ROOTS
                    .iter()
                    .map(PathBuf::from)
                    .collect();
                VaultFs::shadow(
                    &config.vault,
                    &config.scratch,
                    &repo_roots,
                    Box::new(SharedShadowLog {
                        db: Arc::clone(&state),
                        events: events.clone(),
                    }),
                )
                .context("opening the shadow overlay")?
            }
        };
        let vault_root = fs.root().to_path_buf();
        // `open` creates the index when absent and rebuilds on a schema/generation mismatch.
        let writer = VaultWriter::open(fs, &config.index_db).context("opening the index")?;
        let (handle, vault_events, writer_join) =
            cv_vault::spawn_writer_with_watcher(writer, opts.watch).context("starting writer")?;
        let mut views = ViewRegistry::new(&config.index_db, events.clone());
        register_views(&mut views);
        Ok(Self {
            shared: Arc::new(Shared {
                config,
                vault_root,
                writer: RwLock::new(Some(handle)),
                state,
                events,
                views,
                open_subscribers: AtomicUsize::new(0),
            }),
            writer_join,
            vault_events,
            opts,
        })
    }

    /// Binds the socket, drains the spool, then serves in the background. Call inside a tokio
    /// runtime.
    pub async fn spawn(self) -> Result<RunningDaemon> {
        let Self {
            shared,
            writer_join,
            vault_events,
            opts,
        } = self;
        let listener = crate::server::bind(&shared.config.socket)?;
        start_event_bridge(&shared, vault_events)?;
        drain_spool(&shared).await;
        let (stop_tx, stop_rx) = watch::channel(false);
        let tasks = vec![
            tokio::spawn(generation_listener(Arc::clone(&shared), stop_rx.clone())),
            tokio::spawn(config_poller(
                Arc::clone(&shared),
                opts.config_poll,
                stop_rx.clone(),
            )),
        ];
        let server = tokio::spawn(crate::server::serve(listener, Arc::clone(&shared), stop_rx));
        tracing::info!(socket = %shared.config.socket.display(), mode = ?shared.config.mode, "daemon serving");
        Ok(RunningDaemon {
            shared,
            stop_tx,
            server,
            tasks,
            writer_join,
        })
    }
}

/// A serving daemon: the in-process handle WP-9's runner consumes.
pub struct RunningDaemon {
    shared: Arc<Shared>,
    stop_tx: watch::Sender<bool>,
    server: tokio::task::JoinHandle<()>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    writer_join: JoinHandle<()>,
}

impl std::fmt::Debug for RunningDaemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningDaemon").finish_non_exhaustive()
    }
}

impl RunningDaemon {
    /// The shared daemon state.
    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// In-process subscription to every notification (all topics).
    pub fn subscribe(&self) -> broadcast::Receiver<Notification> {
        self.shared.subscribe()
    }

    /// Stops accepting, ends every connection, drops every `WriterHandle`, joins the writer
    /// thread and removes the socket file.
    pub async fn shutdown(self) -> Result<()> {
        let _ = self.stop_tx.send(true);
        if let Err(e) = self.server.await {
            tracing::warn!(error = %e, "server task");
        }
        for t in self.tasks {
            t.abort();
            let _ = t.await;
        }
        self.shared.drop_writer();
        let join = self.writer_join;
        tokio::task::spawn_blocking(move || join.join())
            .await
            .map_err(|e| anyhow!("joining writer: {e}"))?
            .map_err(|_| anyhow!("writer thread panicked"))?;
        match std::fs::remove_file(&self.shared.config.socket) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(error = %e, "removing socket"),
        }
        tracing::info!("daemon stopped");
        Ok(())
    }
}

/// An RPC failure (code + message + optional data).
pub fn rpc_err(code: i64, message: impl Into<String>) -> RpcError {
    RpcError {
        code,
        message: message.into(),
        data: None,
    }
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    rpc_err(error_codes::INTERNAL_ERROR, e.to_string())
}

fn vault_err(e: VaultError) -> RpcError {
    match e {
        VaultError::Conflict { found, .. } => RpcError {
            code: error_codes::CONFLICT,
            message: "content hash conflict".into(),
            data: Some(json!({ "current_hash": found.map(|h| h.as_str().to_owned()) })),
        },
        VaultError::NotFound(p) => rpc_err(error_codes::NOT_FOUND, format!("not found: {p}")),
        e @ (VaultError::InvalidPath(_) | VaultError::Exists(_)) => {
            rpc_err(error_codes::INVALID_PARAMS, e.to_string())
        }
        e => internal(e),
    }
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, RpcError> {
    serde_json::to_value(v).map_err(internal)
}

fn map_job(j: &cv_state::Job) -> JobStatus {
    let status = match j.status {
        cv_state::JobStatus::Queued if j.defer_until_ms.is_some() => JobState::Deferred,
        cv_state::JobStatus::Queued => JobState::Pending,
        cv_state::JobStatus::Running => JobState::Running,
        cv_state::JobStatus::Done => JobState::Done,
        cv_state::JobStatus::Failed => JobState::Failed,
        cv_state::JobStatus::Cancelled => JobState::Cancelled,
    };
    JobStatus {
        job_id: j.id.clone(),
        status,
        message: j.error.clone(),
        output_paths: Vec::new(),
    }
}

impl Shared {
    /// The resolved config.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The canonical vault root.
    pub fn vault_root(&self) -> &Path {
        &self.vault_root
    }

    /// A `WriterHandle` (the single writer, I2); `None` once shutdown began.
    pub fn writer(&self) -> Option<WriterHandle> {
        self.writer
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn drop_writer(&self) {
        *self.writer.write().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The daemon's single `StateDb` (lock only for a call; never across `.await`).
    pub fn state(&self) -> &Arc<Mutex<StateDb>> {
        &self.state
    }

    /// In-process subscription to every notification.
    pub fn subscribe(&self) -> broadcast::Receiver<Notification> {
        self.events.subscribe()
    }

    /// Broadcasts `n` (no receivers is fine).
    pub fn publish(&self, n: Notification) {
        let _ = self.events.send(n);
    }

    /// The view registry.
    pub fn views(&self) -> &ViewRegistry {
        &self.views
    }

    pub(crate) fn open_subscribers(&self) -> &AtomicUsize {
        &self.open_subscribers
    }

    /// Runs `f` with the writer on a blocking thread.
    pub async fn with_writer<T: Send + 'static>(
        &self,
        f: impl FnOnce(WriterHandle) -> T + Send + 'static,
    ) -> Result<T, RpcError> {
        let w = self
            .writer()
            .ok_or_else(|| internal("daemon is shutting down"))?;
        tokio::task::spawn_blocking(move || f(w))
            .await
            .map_err(internal)
    }

    /// Rebuilds the index; on success broadcasts `index_generation{n}`. On failure the writer
    /// has already re-opened the newest generation (`VaultWriter::rebuild`); the error is logged.
    pub async fn rebuild_index(&self) -> Result<u64, RpcError> {
        match self.with_writer(|w| w.rebuild()).await? {
            Ok(n) => {
                self.publish(Notification::IndexGeneration { n });
                Ok(n)
            }
            Err(e) => {
                tracing::error!(error = %e, "index rebuild failed; writer reopened current");
                Err(vault_err(e))
            }
        }
    }

    /// The `hello` result.
    pub fn hello(&self) -> HelloResult {
        HelloResult {
            protocol_version: PROTOCOL_VERSION,
            index_schema: cv_index::SCHEMA_VERSION,
            mode: self.config.mode,
            vault_root: self.vault_root.display().to_string(),
            index_db: self.config.index_db.display().to_string(),
            state_db: self.config.state_db.display().to_string(),
            generation: self.generation(),
        }
    }

    fn generation(&self) -> u64 {
        cv_index::current_generation(&self.config.index_db)
            .ok()
            .flatten()
            .unwrap_or(0)
    }

    /// Handles every request except `hello`/`subscribe` (connection-level).
    pub async fn handle(&self, req: Request) -> Result<Value, RpcError> {
        match req {
            Request::Hello(_) | Request::Subscribe(_) => Err(rpc_err(
                error_codes::INVALID_REQUEST,
                "hello/subscribe are connection-level",
            )),
            Request::WriteNote(p) => {
                let expected =
                    (!p.base_hash.is_empty()).then(|| ContentHash::from_hex(p.base_hash));
                let res = self
                    .with_writer(move |w| w.write(&p.path, p.content.into_bytes(), expected))
                    .await?;
                match res {
                    Ok(h) => to_value(&WriteNoteResult::Written {
                        hash: h.as_str().to_owned(),
                    }),
                    Err(VaultError::Conflict { found, .. }) => {
                        to_value(&WriteNoteResult::Conflict {
                            current_hash: found.map(|h| h.as_str().to_owned()).unwrap_or_default(),
                        })
                    }
                    Err(e) => Err(vault_err(e)),
                }
            }
            Request::CreateNote(p) => {
                let path = self.create_suffixed(p.path, p.content).await?;
                to_value(&CreateNoteResult { path })
            }
            Request::Rename(p) => {
                // Read the hash and rename in one writer job: the CAS token is the hash the writer
                // itself just read, so no other writer job can interleave.
                let res = self
                    .with_writer(move |w| {
                        w.call(move |vw| {
                            let h = vw
                                .fs()
                                .hash(&p.from)?
                                .ok_or_else(|| VaultError::NotFound(p.from.clone()))?;
                            vw.rename(&p.from, &p.to, h)
                        })
                    })
                    .await?;
                let out = res.and_then(|r| r).map_err(vault_err)?;
                Ok(
                    json!({ "rewritten": out.rewritten, "skipped_conflicts": out.skipped_conflicts }),
                )
            }
            Request::Delete(p) => {
                let res = self
                    .with_writer(move |w| {
                        w.call(move |vw| {
                            let h = vw
                                .fs()
                                .hash(&p.path)?
                                .ok_or_else(|| VaultError::NotFound(p.path.clone()))?;
                            vw.delete(&p.path, h)
                        })
                    })
                    .await?;
                res.and_then(|r| r).map_err(vault_err)?;
                Ok(json!({}))
            }
            // WP-9 registers actions and implements the runner.
            Request::ActionsList => Ok(json!([])),
            Request::RunAction(_) => Err(rpc_err(error_codes::NOT_FOUND, "no actions registered")),
            Request::Job(p) => {
                let job = lock(&self.state).job(&p.job_id).map_err(internal)?;
                match job {
                    Some(j) => to_value(&map_job(&j)),
                    None => Err(rpc_err(
                        error_codes::NOT_FOUND,
                        format!("no job {}", p.job_id),
                    )),
                }
            }
            Request::Cancel(p) => {
                let res = {
                    let mut db = lock(&self.state);
                    db.cancel(&p.job_id, now_ms())
                        .and_then(|()| db.job(&p.job_id))
                };
                match res {
                    Ok(Some(j)) => to_value(&map_job(&j)),
                    Ok(None) | Err(StateError::NoSuchJob(_)) => Err(rpc_err(
                        error_codes::NOT_FOUND,
                        format!("no job {}", p.job_id),
                    )),
                    Err(e @ StateError::WrongStatus { .. }) => {
                        Err(rpc_err(error_codes::INVALID_PARAMS, e.to_string()))
                    }
                    Err(e) => Err(internal(e)),
                }
            }
            Request::Uri(p) => {
                let r = self.intake(&p.url).await?;
                to_value(&r)
            }
            Request::Status => to_value(&self.status()?),
            Request::ViewsList => to_value(&ViewsListResult {
                view_ids: self.views.view_ids(),
            }),
            Request::ViewRun(p) => match self.views.run(&p.view_id, p.this_path).await {
                Ok(r) => to_value(&r),
                Err(ViewError::NotFound(id)) => {
                    Err(rpc_err(error_codes::NOT_FOUND, format!("no view {id}")))
                }
                Err(ViewError::Failed(m)) => Err(internal(m)),
            },
        }
    }

    /// The `status` result.
    pub fn status(&self) -> Result<StatusResult, RpcError> {
        let counts = lock(&self.state).counts().map_err(internal)?;
        let mut queue: BTreeMap<String, u64> = BTreeMap::new();
        for c in counts {
            *queue.entry(c.status.as_str().to_owned()).or_default() +=
                u64::try_from(c.count).unwrap_or(0);
        }
        let services = StateReader::open(&self.config.state_db)
            .and_then(|r| r.service_health())
            .map_err(internal)?
            .into_iter()
            .map(|h| ServiceHealth {
                service: h.service,
                state: serde_json::to_value(h.state)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default(),
            })
            .collect();
        Ok(StatusResult {
            mode: self.config.mode,
            generation: self.generation(),
            queue,
            config_errors: Vec::new(),
            services,
        })
    }

    /// Creates `path` with Obsidian's ` N` collision suffix; returns the final path.
    async fn create_suffixed(&self, path: String, content: String) -> Result<String, RpcError> {
        let res = self
            .with_writer(move |w| {
                for n in 0..MAX_SUFFIX {
                    let p = uri::suffixed(&path, n);
                    match w.create(&p, content.clone().into_bytes()) {
                        Ok(_) => return Ok(p),
                        Err(VaultError::Exists(_)) => continue,
                        Err(e) => return Err(e),
                    }
                }
                Err(VaultError::Exists(path))
            })
            .await?;
        res.map_err(vault_err)
    }

    /// `obsidian://` intake (see [`crate::uri`]).
    pub async fn intake(&self, url: &str) -> Result<UriResult, RpcError> {
        let name = self
            .vault_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let action = uri::parse(url, &name)
            .map_err(|e| rpc_err(error_codes::INVALID_PARAMS, e.to_string()))?;
        match action {
            UriAction::New {
                path,
                content,
                silent,
            } => {
                let path = self.create_suffixed(path, content).await?;
                // Obsidian opens a non-silent new note; we only tell subscribed clients.
                if !silent && self.open_subscribers.load(Ordering::SeqCst) > 0 {
                    self.publish(Notification::OpenRequested { path: path.clone() });
                }
                Ok(UriResult::Created { path })
            }
            UriAction::Open { path } => {
                if self.open_subscribers.load(Ordering::SeqCst) > 0 {
                    self.publish(Notification::OpenRequested { path: path.clone() });
                } else if let Some(cmd) = self.config.open_cmd.clone() {
                    let abs = self.vault_root.join(&path);
                    tokio::task::spawn_blocking(move || run_open_cmd(&cmd, &abs));
                } else {
                    tracing::info!(%path, "open requested; no subscriber and no CV_OPEN_CMD");
                }
                Ok(UriResult::OpenRequested { path })
            }
        }
    }
}

fn run_open_cmd(cmd: &Path, abs: &Path) {
    match std::process::Command::new(cmd).arg(abs).spawn() {
        Ok(mut child) => match child.wait() {
            Ok(s) if s.success() => {}
            Ok(s) => tracing::warn!(status = %s, "CV_OPEN_CMD exited unsuccessfully"),
            Err(e) => tracing::warn!(error = %e, "waiting for CV_OPEN_CMD"),
        },
        Err(e) => tracing::warn!(error = %e, cmd = %cmd.display(), "running CV_OPEN_CMD"),
    }
}

/// Bridges the writer's std receiver into the broadcast channel on a dedicated thread. Exits when
/// the writer thread does.
fn start_event_bridge(
    shared: &Arc<Shared>,
    rx: std::sync::mpsc::Receiver<VaultEvent>,
) -> Result<()> {
    let weak = Arc::downgrade(shared);
    let events = shared.events.clone();
    std::thread::Builder::new()
        .name("cv-events".into())
        .spawn(move || {
            while let Ok(ev) = rx.recv() {
                let (kind, want_hash) = match ev.kind {
                    EventKind::Created => (ChangeKind::Created, true),
                    EventKind::Modified => (ChangeKind::Modified, true),
                    EventKind::Deleted => (ChangeKind::Deleted, false),
                    EventKind::Renamed { .. } => (ChangeKind::Renamed, false),
                };
                let hash = if want_hash {
                    weak.upgrade().and_then(|s| s.writer()).and_then(|w| {
                        let p = ev.path.clone();
                        w.call(move |vw| vw.fs().hash(&p))
                            .ok()
                            .and_then(Result::ok)
                            .flatten()
                            .map(|h| h.as_str().to_owned())
                    })
                } else {
                    None
                };
                let origin = match ev.origin {
                    cv_vault::Origin::Daemon => Origin::Daemon,
                    cv_vault::Origin::External => Origin::External,
                };
                let _ = events.send(Notification::FileChanged {
                    path: ev.path,
                    kind,
                    hash,
                    origin,
                });
            }
        })
        .context("spawning event bridge")?;
    Ok(())
}

/// Drains the URI spool in filename order (I4: intake, not replay). A file is deleted only after
/// its intake succeeded; a permanent refusal moves it to `failed/`; a transient failure leaves it
/// for the next start.
async fn drain_spool(shared: &Shared) {
    let dir = shared.config.spool_dir();
    let files = match spool::pending(&dir) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, "listing the URI spool");
            return;
        }
    };
    for f in files {
        let url = match std::fs::read_to_string(&f) {
            Ok(u) => u,
            Err(e) => {
                tracing::error!(error = %e, file = %f.display(), "reading spool file");
                continue;
            }
        };
        match shared.intake(url.trim_end()).await {
            Ok(r) => {
                tracing::info!(?r, "spooled URI ingested");
                if let Err(e) = std::fs::remove_file(&f) {
                    tracing::error!(error = %e, file = %f.display(), "removing spool file");
                }
            }
            Err(e) if e.code == error_codes::INVALID_PARAMS => {
                tracing::error!(error = %e.message, file = %f.display(), "spooled URI refused; quarantined");
                if let Err(e) = spool::quarantine(&f) {
                    tracing::error!(error = %e, "quarantining spool file");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e.message, file = %f.display(), "spooled URI intake failed; kept");
            }
        }
    }
}

/// Forwards `index_generation` to the view registry.
async fn generation_listener(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let mut rx = shared.subscribe();
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            r = rx.recv() => match r {
                Ok(Notification::IndexGeneration { n }) => shared.views.notify_generation(n),
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(k)) => {
                    tracing::warn!(skipped = k, "generation listener lagged");
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
        }
    }
}

/// A cheap change fingerprint of `CV_CONFIG_DIR`: (max mtime ns, entry count) over top-level
/// files and `views/`.
fn config_fingerprint(dir: &Path) -> (u128, usize) {
    let mut max = 0u128;
    let mut count = 0usize;
    for d in [dir.to_path_buf(), dir.join("views")] {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for ent in rd.flatten() {
            count += 1;
            if let Ok(m) = ent.metadata().and_then(|m| m.modified())
                && let Ok(t) = m.duration_since(std::time::UNIX_EPOCH)
            {
                max = max.max(t.as_nanos());
            }
        }
    }
    (max, count)
}

/// Polls `CV_CONFIG_DIR`; on change notifies view services and broadcasts
/// `config_reloaded{errors: []}`. WP-9's config loader supplies the real errors.
async fn config_poller(shared: Arc<Shared>, every: Duration, mut stop: watch::Receiver<bool>) {
    let dir = shared.config.config_dir.clone();
    let mut last = config_fingerprint(&dir);
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            _ = tick.tick() => {
                let fp = config_fingerprint(&dir);
                if fp != last {
                    last = fp;
                    shared.views.notify_config(&dir);
                    shared.publish(Notification::ConfigReloaded { errors: Vec::new() });
                }
            }
        }
    }
}

/// Ensures nothing is listening at `socket`; removes a stale socket file.
pub(crate) fn clear_stale_socket(socket: &Path) -> Result<()> {
    if socket.symlink_metadata().is_err() {
        return Ok(());
    }
    if std::os::unix::net::UnixStream::connect(socket).is_ok() {
        bail!("daemon already running at {}", socket.display());
    }
    std::fs::remove_file(socket).with_context(|| format!("removing stale {}", socket.display()))
}
