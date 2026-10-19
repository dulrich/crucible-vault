//! `cv daemon`: startup, shared state, request handling, event bridge, spool drain, shutdown.
//!
//! Startup order (brief pin 4): config → `StateDb` → `VaultFs` →
//! `VaultWriter::open` → writer+watcher thread — all on the calling thread, before any tokio
//! runtime exists ([`Daemon::start`]) — then, inside a runtime, bind the socket → drain the spool
//! → serve ([`Daemon::spawn`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use cv_protocol::{
    ChangeKind, CreateNoteResult, DeleteMode, DeleteNoteParams, DeleteNoteResult, DeleteParams,
    DeletePreviewItem, DeletePreviewResult, DeleteResult, HelloResult, IntakeListResult, JobState,
    JobStatus, JobsListResult, Method, Notification, Origin, PROTOCOL_VERSION, PathParams,
    RenameParams, RenameResult, Request, RpcError, StatusResult, UriResult, ViewsListResult,
    WriteAttachmentParams, WriteAttachmentResult, WriteNoteParams, WriteNoteResult, error_codes,
};
use cv_state::{QueueConfig, RetryOutcome, StateDb, StateError, StateReader};
use cv_vault::{
    ContentHash, EventKind, VaultError, VaultEvent, VaultFs, VaultWriter, WatchConfig, WriterHandle,
};
use serde_json::{Value, json};
use tokio::sync::{broadcast, watch};

use crate::config::Config;
use crate::lookup::{LookupBases, Lookups};
use crate::runner::{ProductionExecutor, Runner, RunnerDeps, StepExecutor, SystemClock};
use crate::uri::{self, UriAction};
use crate::views::{ViewError, ViewRegistry, register_views};
use crate::{now_ms, spool};

/// Broadcast capacity for notifications (a lagging subscriber skips, never blocks).
const EVENT_CAPACITY: usize = 1024;
/// Upper bound on the ` N` collision suffix search.
const MAX_SUFFIX: u32 = 10_000;
/// `write_attachment` decoded-size cap (25 MiB).
const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;

fn too_large() -> RpcError {
    rpc_err(
        error_codes::INVALID_PARAMS,
        format!("attachment exceeds {MAX_ATTACHMENT_BYTES} bytes"),
    )
}

/// Daemon start options.
#[derive(Debug, Clone, Copy)]
pub struct DaemonOptions {
    /// Watcher timings.
    pub watch: WatchConfig,
    /// `CV_CONFIG_DIR` poll interval (2 s in production).
    pub config_poll: Duration,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            watch: WatchConfig::default(),
            config_poll: Duration::from_secs(2),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
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
    runner: OnceLock<Runner>,
    lookups: Lookups,
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
    /// Synchronous startup (see the module docs).
    pub fn start(config: Config, opts: DaemonOptions) -> Result<Self> {
        config.create_dirs()?;
        let state = Arc::new(Mutex::new(
            StateDb::open(&config.state_db, now_ms(), QueueConfig::default())
                .with_context(|| format!("opening {}", config.state_db.display()))?,
        ));
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let fs = VaultFs::live(
            &config.vault,
            &config.protected_vault,
            &crate::cutover::StateCutover::new(Arc::clone(&state), &config.protected_vault),
        )
        .context("live mode refused")?
        .with_link_write_roots(config.link_write_roots.clone());
        let vault_root = fs.root().to_path_buf();
        // `open` creates the index when absent and rebuilds on a schema/generation mismatch.
        let writer = VaultWriter::open(fs, &config.index_db).context("opening the index")?;
        let (handle, vault_events, writer_join) =
            cv_vault::spawn_writer_with_watcher(writer, opts.watch).context("starting writer")?;
        let mut views =
            ViewRegistry::new(&config.index_db, events.clone()).with_state_db(&config.state_db);
        register_views(&mut views, &config.config_dir);
        // `tracker_items` inserts invalidate `source = "tracker_items"` views (round-5 WP-10).
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_change_hook(views.state_notifier());
        let lookups = Lookups::new(
            &config.config_dir,
            Arc::clone(&state),
            LookupBases::default(),
        );
        Ok(Self {
            shared: Arc::new(Shared {
                config,
                vault_root,
                writer: RwLock::new(Some(handle)),
                state,
                events,
                views,
                open_subscribers: AtomicUsize::new(0),
                runner: OnceLock::new(),
                lookups,
            }),
            writer_join,
            vault_events,
            opts,
        })
    }

    /// Binds the socket, drains the spool, then serves in the background. Call inside a tokio
    /// runtime.
    pub async fn spawn(self) -> Result<RunningDaemon> {
        self.spawn_with(Arc::new(ProductionExecutor)).await
    }

    /// [`Daemon::spawn`] with an injected step executor (tests).
    pub async fn spawn_with(self, executor: Arc<dyn StepExecutor>) -> Result<RunningDaemon> {
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
        let writer_source: Arc<dyn crate::runner::WriterSource> = Arc::new(Arc::downgrade(&shared));
        let runner = Runner::new(RunnerDeps {
            state: Arc::clone(&shared.state),
            writer: writer_source,
            events: shared.events.clone(),
            executor,
            clock: Arc::new(SystemClock),
            concurrency: 2,
            vault_root: shared.vault_root.clone(),
            index_db: shared.config.index_db.clone(),
            config_dir: shared.config.config_dir.clone(),
            search_db: shared.config.search_db.clone(),
        });
        runner.reload_config();
        if shared.runner.set(runner.clone()).is_err() {
            bail!("runner already started");
        }
        let mut tasks = vec![
            runner.spawn_claim_loop(stop_rx.clone()),
            runner.spawn_event_starter(stop_rx.clone()),
            runner.spawn_scheduler(stop_rx.clone()),
        ];
        tasks.extend([
            tokio::spawn(generation_listener(Arc::clone(&shared), stop_rx.clone())),
            tokio::spawn(config_poller(
                Arc::clone(&shared),
                opts.config_poll,
                stop_rx.clone(),
            )),
        ]);
        let server = tokio::spawn(crate::server::serve(listener, Arc::clone(&shared), stop_rx));
        tracing::info!(socket = %shared.config.socket.display(), "daemon serving");
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

/// Every [`VaultError`] → its RPC code (exhaustive by design: a new variant must pick one).
pub(crate) fn rpc_from_vault(e: VaultError) -> RpcError {
    use error_codes as c;
    match e {
        VaultError::Conflict { found, .. } => RpcError {
            code: c::CONFLICT,
            message: "content hash conflict".into(),
            data: Some(json!({ "current_hash": found.map(|h| h.as_str().to_owned()) })),
        },
        e @ VaultError::NotFound(_) => rpc_err(c::NOT_FOUND, e.to_string()),
        e @ (VaultError::InvalidPath(_) | VaultError::Exists(_)) => {
            rpc_err(c::INVALID_PARAMS, e.to_string())
        }
        e @ (VaultError::ProtectedRoot { .. } | VaultError::LinkWriteRefused { .. }) => {
            rpc_err(c::REFUSED, e.to_string())
        }
        e @ VaultError::WriterGone => rpc_err(c::UNAVAILABLE, e.to_string()),
        e @ (VaultError::Io { .. }
        | VaultError::Index(_)
        | VaultError::Watch(_)
        | VaultError::Json(_)) => internal(e),
    }
}

/// Every [`StateError`] → its RPC code (exhaustive by design).
pub(crate) fn rpc_from_state(e: StateError) -> RpcError {
    use error_codes as c;
    match e {
        StateError::NoSuchJob(id) => rpc_err(c::NOT_FOUND, format!("no job {id}")),
        e @ StateError::WrongStatus { .. } => rpc_err(c::INVALID_PARAMS, e.to_string()),
        e @ (StateError::Sqlite(_)
        | StateError::Json(_)
        | StateError::NewerSchema { .. }
        | StateError::Corrupt { .. }) => internal(e),
    }
}

fn unavailable(m: &str) -> RpcError {
    rpc_err(error_codes::UNAVAILABLE, m)
}

/// A writer-job failure: the vault's, or a refusal of the request's own params.
pub(crate) enum JobError {
    Vault(VaultError),
    Invalid(String),
}

impl From<VaultError> for JobError {
    fn from(e: VaultError) -> Self {
        Self::Vault(e)
    }
}

/// Serialises method `M`'s typed result (ties each handler's return type to the contract).
fn reply<M: Method>(r: Result<M::Result, RpcError>) -> Result<Value, RpcError> {
    serde_json::to_value(r?).map_err(internal)
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
        open_path: None,
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

    /// The action runner (set once the daemon is serving).
    pub fn runner(&self) -> Option<&Runner> {
        self.runner.get()
    }

    fn runner_or_err(&self) -> Result<&Runner, RpcError> {
        self.runner()
            .ok_or_else(|| unavailable("runner not started"))
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
            .ok_or_else(|| unavailable("daemon is shutting down"))?;
        tokio::task::spawn_blocking(move || f(w))
            .await
            .map_err(internal)
    }

    /// Runs `f` as one writer job; every layer's failure (shutdown, writer gone, the job's own
    /// error) flattens into one [`RpcError`].
    pub(crate) async fn writer_job<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut cv_vault::VaultWriter) -> Result<T, JobError> + Send + 'static,
    ) -> Result<T, RpcError> {
        match self.with_writer(move |w| w.call(f)).await? {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(JobError::Invalid(m))) => Err(rpc_err(error_codes::INVALID_PARAMS, m)),
            Ok(Err(JobError::Vault(e))) | Err(e) => Err(rpc_from_vault(e)),
        }
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
                Err(rpc_from_vault(e))
            }
        }
    }

    /// The `hello` result.
    pub fn hello(&self) -> HelloResult {
        HelloResult {
            protocol_version: PROTOCOL_VERSION,
            index_schema: cv_index::SCHEMA_VERSION,
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

    /// Handles every request except `hello`/`subscribe` (connection-level): one handler per
    /// method, each returning that method's [`Method::Result`].
    pub async fn handle(&self, req: Request) -> Result<Value, RpcError> {
        use cv_protocol::methods as m;
        match req {
            Request::Hello(_) | Request::Subscribe(_) => Err(rpc_err(
                error_codes::INVALID_REQUEST,
                "hello/subscribe are connection-level",
            )),
            Request::WriteNote(p) => reply::<m::WriteNote>(self.write_note(p).await),
            Request::CreateNote(p) => reply::<m::CreateNote>(
                self.create_suffixed(p.path, p.content)
                    .await
                    .map(|path| CreateNoteResult { path }),
            ),
            Request::WriteAttachment(p) => {
                reply::<m::WriteAttachment>(self.write_attachment(p).await)
            }
            Request::Rename(p) => reply::<m::Rename>(self.rename(p).await),
            Request::Delete(p) => reply::<m::Delete>(self.delete(p).await),
            Request::DeletePreview(p) => reply::<m::DeletePreview>(self.delete_preview(p).await),
            Request::DeleteNote(p) => reply::<m::DeleteNote>(self.delete_note(p).await),
            Request::ActionsList => {
                reply::<m::ActionsList>(self.runner_or_err().map(crate::runner::actions_list))
            }
            Request::RunAction(p) => reply::<m::RunAction>(
                crate::runner::run_action(self, self.runner_or_err()?, p).await,
            ),
            Request::Job(p) => reply::<m::Job>(self.job(&p.job_id)),
            Request::Cancel(p) => reply::<m::Cancel>(self.cancel(&p.job_id)),
            Request::JobsList(p) => reply::<m::JobsList>(self.jobs_list(&p)),
            Request::JobRetry(p) => reply::<m::JobRetry>(self.job_retry(&p.job_id)),
            Request::IntakeList(p) => reply::<m::IntakeList>(self.intake_list(&p)),
            Request::Uri(p) => reply::<m::Uri>(self.intake(&p.url).await),
            Request::Status => reply::<m::Status>(self.status()),
            Request::ViewsList => reply::<m::ViewsList>(Ok(ViewsListResult {
                view_ids: self.views.view_ids(),
            })),
            Request::ViewRun(p) => {
                reply::<m::ViewRun>(self.views.run(&p.view_id, p.this_path).await.map_err(|e| {
                    match e {
                        ViewError::NotFound(id) => {
                            rpc_err(error_codes::NOT_FOUND, format!("no view {id}"))
                        }
                        ViewError::Failed(m) => internal(m),
                    }
                }))
            }
            Request::LookupCurrencies => {
                reply::<m::LookupCurrencies>(self.lookups.currencies().await)
            }
            Request::LookupGeocode(p) => {
                reply::<m::LookupGeocode>(self.lookups.geocode(&p.query).await)
            }
            Request::LookupModels(p) => {
                reply::<m::LookupModels>(self.lookups.models(&p.provider).await)
            }
            Request::LookupChannels(p) => {
                reply::<m::LookupChannels>(self.lookups.channels(&p.ids).await)
            }
            Request::Search(p) => reply::<m::Search>(self.search(&p).await),
            Request::TrackerItemSetStatus(p) => reply::<m::TrackerItemSetStatus>(
                self.set_tracker_status(&p.source_kind, &p.source_key, &p.item_id, p.status),
            ),
            Request::NoteSetBool(p) => reply::<m::NoteSetBool>(self.note_set_bool(p).await),
        }
    }

    /// `write_note`: a CAS write; a hash conflict is a result, not an error.
    async fn write_note(&self, p: WriteNoteParams) -> Result<WriteNoteResult, RpcError> {
        let expected = (!p.base_hash.is_empty()).then(|| ContentHash::from_hex(p.base_hash));
        let res = self
            .with_writer(move |w| w.write(&p.path, p.content.into_bytes(), expected))
            .await?;
        match res {
            Ok(h) => Ok(WriteNoteResult::Written {
                hash: h.as_str().to_owned(),
            }),
            Err(VaultError::Conflict { found, .. }) => Ok(WriteNoteResult::Conflict {
                current_hash: found.map(|h| h.as_str().to_owned()).unwrap_or_default(),
            }),
            Err(e) => Err(rpc_from_vault(e)),
        }
    }

    /// `rename`: read the hash and rename in one writer job, so the CAS token is the hash the
    /// writer itself just read and no other writer job can interleave.
    async fn rename(&self, p: RenameParams) -> Result<RenameResult, RpcError> {
        let out = self
            .writer_job(move |vw| {
                let h = vw
                    .fs()
                    .hash(&p.from)?
                    .ok_or_else(|| VaultError::NotFound(p.from.clone()))?;
                Ok(vw.rename(&p.from, &p.to, h)?)
            })
            .await?;
        Ok(RenameResult {
            rewritten: out.rewritten,
            skipped_conflicts: out.skipped_conflicts,
        })
    }

    /// `delete`: hash read + CAS delete in one writer job.
    async fn delete(&self, p: DeleteParams) -> Result<DeleteResult, RpcError> {
        let permanent = p.mode == DeleteMode::Permanent;
        self.writer_job(move |vw| {
            let h = vw
                .fs()
                .hash(&p.path)?
                .ok_or_else(|| VaultError::NotFound(p.path.clone()))?;
            Ok(vw.delete_with(&p.path, h, permanent)?)
        })
        .await?;
        Ok(DeleteResult {})
    }

    /// `delete_preview`: the note's embeds/attachments and whether anything else references them.
    async fn delete_preview(&self, p: PathParams) -> Result<DeletePreviewResult, RpcError> {
        let items = self
            .writer_job(move |vw| {
                Ok(cv_index::delete_preview(&vw.index().query(), &p.path)
                    .map_err(VaultError::from)?)
            })
            .await?;
        Ok(DeletePreviewResult {
            items: items
                .into_iter()
                .map(|(path, referenced_elsewhere)| DeletePreviewItem {
                    path,
                    referenced_elsewhere,
                })
                .collect(),
        })
    }

    /// `delete_note`: one writer job — the note's CAS delete, then each attachment re-checked
    /// against the index at that moment (never removes something referenced since the preview).
    async fn delete_note(&self, p: DeleteNoteParams) -> Result<DeleteNoteResult, RpcError> {
        let permanent = p.mode == DeleteMode::Permanent;
        let (removed, skipped) = self
            .writer_job(move |vw| Ok(vw.delete_note(&p.path, &p.attachments, permanent)?))
            .await?;
        Ok(DeleteNoteResult { removed, skipped })
    }

    fn job(&self, id: &str) -> Result<JobStatus, RpcError> {
        let job = lock(&self.state).job(id).map_err(rpc_from_state)?;
        job.map(|j| map_job(&j))
            .ok_or_else(|| rpc_from_state(StateError::NoSuchJob(id.to_owned())))
    }

    /// `cancel`: a queued job settles now; a running (claimed) one gets the durable
    /// `cancel_requested` flag and stops before its next step (no claim→running race, SR10).
    fn cancel(&self, id: &str) -> Result<JobStatus, RpcError> {
        lock(&self.state)
            .cancel(id, now_ms())
            .map_err(rpc_from_state)
            .and_then(|()| self.job(id))
    }

    fn jobs_list(&self, p: &cv_protocol::JobsListParams) -> Result<JobsListResult, RpcError> {
        let now = now_ms();
        let q =
            crate::jobs::list_query(p, now).map_err(|m| rpc_err(error_codes::INVALID_PARAMS, m))?;
        let jobs = StateReader::open(&self.config.state_db)
            .and_then(|r| r.list(&q))
            .map_err(rpc_from_state)?;
        Ok(JobsListResult {
            jobs: jobs
                .iter()
                .map(|j| crate::jobs::job_summary(j, now))
                .collect(),
        })
    }

    fn job_retry(&self, id: &str) -> Result<JobStatus, RpcError> {
        let outcome = lock(&self.state)
            .retry_job(id, now_ms())
            .map_err(rpc_from_state)?;
        match outcome {
            RetryOutcome::Retried => self.job(id),
            RetryOutcome::Duplicate(other) => Err(rpc_err(
                error_codes::CONFLICT,
                format!("job {other} with the same dedupe key is already pending"),
            )),
        }
    }

    fn intake_list(&self, p: &cv_protocol::IntakeListParams) -> Result<IntakeListResult, RpcError> {
        let q =
            crate::jobs::intake_query(p).map_err(|m| rpc_err(error_codes::INVALID_PARAMS, m))?;
        let items = StateReader::open(&self.config.state_db)
            .and_then(|r| r.tracker_items(&q))
            .map_err(rpc_from_state)?;
        Ok(IntakeListResult {
            items: items.into_iter().map(crate::jobs::intake_item).collect(),
        })
    }

    /// `note_set_bool` (plan round-8-intake-views-anchor WP-3): read → frontmatter edit → CAS
    /// write in one writer job (the CAS token is the hash the writer itself read), so the writer's
    /// refusals apply. An already-equal value writes nothing.
    async fn note_set_bool(
        &self,
        p: cv_protocol::NoteSetBoolParams,
    ) -> Result<cv_protocol::NoteSetBoolResult, RpcError> {
        let hash = self
            .writer_job(move |vw| {
                let bytes = vw
                    .fs()
                    .read(&p.path)?
                    .ok_or_else(|| VaultError::NotFound(p.path.clone()))?;
                let hash = ContentHash::of(&bytes);
                let text = String::from_utf8(bytes)
                    .map_err(|_| JobError::Invalid(format!("{} is not UTF-8", p.path)))?;
                let mut doc = cv_lint::FrontmatterDoc::new(text);
                match doc.set(&p.key, &cv_lint::JsValue::Bool(p.value)) {
                    Err(e) => Err(JobError::Invalid(format!("{}: {e}", p.path))),
                    Ok(false) => Ok(hash),
                    Ok(true) => Ok(vw.write(&p.path, doc.text().as_bytes(), Some(hash))?),
                }
            })
            .await?;
        Ok(cv_protocol::NoteSetBoolResult {
            hash: hash.as_str().to_owned(),
        })
    }

    /// `search` (plan semsearch-crate WP-4): hybrid search over the runner's shared index,
    /// off the request loop ([`crate::search_query`]).
    pub async fn search(
        &self,
        params: &cv_protocol::SearchParams,
    ) -> Result<cv_protocol::SearchResult, RpcError> {
        let runner = self.runner_or_err()?;
        let mgr = crate::steps::agent::manager().map_err(|e| internal(e.to_string()))?;
        crate::search_query::search(runner.search_db(), &runner.config(), mgr, params)
            .await
            .map_err(internal)
    }

    /// `tracker_item_set_status`.
    fn set_tracker_status(
        &self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
        status: Option<char>,
    ) -> Result<(), RpcError> {
        let matched = lock(&self.state)
            .set_tracker_item_status(source_kind, source_key, item_id, status)
            .map_err(internal)?;
        if !matched {
            return Err(rpc_err(
                error_codes::NOT_FOUND,
                format!("no tracker item {source_kind}/{source_key}/{item_id}"),
            ));
        }
        Ok(())
    }

    /// The `status` result.
    pub fn status(&self) -> Result<StatusResult, RpcError> {
        let counts = lock(&self.state).counts().map_err(internal)?;
        let mut queue: BTreeMap<String, u64> = BTreeMap::new();
        for c in counts {
            *queue.entry(c.status.as_str().to_owned()).or_default() +=
                u64::try_from(c.count).unwrap_or(0);
        }
        let reader = StateReader::open(&self.config.state_db).map_err(internal)?;
        let services = reader
            .service_health()
            .map_err(internal)?
            .into_iter()
            .map(crate::jobs::service_health)
            .collect();
        let (tracker_items, link_records) = crate::jobs::store_counts(&reader);
        Ok(StatusResult {
            generation: self.generation(),
            queue,
            config_errors: self.runner().map(Runner::config_errors).unwrap_or_default(),
            services,
            tracker_items,
            link_records,
        })
    }

    /// `write_attachment`: validates, resolves the path with the `localize` default templates,
    /// reuses an existing file (content-MD5 name ⇒ identical bytes), else creates it through the
    /// writer.
    async fn write_attachment(
        &self,
        p: WriteAttachmentParams,
    ) -> Result<WriteAttachmentResult, RpcError> {
        use base64::Engine as _;
        use cv_text::attachments::{
            DEFAULT_FOLDER_TEMPLATE, DEFAULT_NAME_TEMPLATE, resolve_attachment_path,
        };
        let ext_ok = (1..=8).contains(&p.ext.len())
            && p.ext
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
        if !ext_ok {
            return Err(rpc_err(
                error_codes::INVALID_PARAMS,
                format!("invalid attachment ext {:?}", p.ext),
            ));
        }
        // Reject before decoding: base64 is 4/3 of the decoded size.
        if p.data_base64.len() / 4 * 3 > MAX_ATTACHMENT_BYTES + 3 {
            return Err(too_large());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(p.data_base64.as_bytes())
            .map_err(|e| rpc_err(error_codes::INVALID_PARAMS, format!("bad base64: {e}")))?;
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(too_large());
        }
        let md5 = crate::steps::util::md5_hex(&bytes);
        let target = resolve_attachment_path(
            DEFAULT_FOLDER_TEMPLATE,
            DEFAULT_NAME_TEMPLATE,
            &p.note_path,
            "",
            &p.ext,
            &md5,
            chrono::Local::now().naive_local(),
        );
        let t = target.clone();
        self.writer_job(move |vw| {
            if vw.fs().hash(&t)?.is_none() {
                match vw.create(&t, &bytes) {
                    Ok(_) | Err(VaultError::Exists(_)) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(())
        })
        .await?;
        Ok(WriteAttachmentResult { path: target })
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
        res.map_err(rpc_from_vault)
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
                // A `.gitignore` create/modify/delete/rename changes index membership: rebuild
                // (plan editor-ui-round-3 WP-1). Checked before anything else, since the file is
                // itself a dot-file the index never holds.
                let ignore_changed = cv_index::is_gitignore_path(&ev.path)
                    || matches!(&ev.kind, EventKind::Renamed { from } if cv_index::is_gitignore_path(from));
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
                if ignore_changed
                    && let Some(w) = weak.upgrade().and_then(|s| s.writer())
                {
                    match w.call(|vw| vw.rebuild()) {
                        Ok(Ok(n)) => {
                            let _ = events.send(Notification::IndexGeneration { n });
                        }
                        Ok(Err(e)) => {
                            tracing::error!(error = %e, "index rebuild after .gitignore change failed");
                        }
                        Err(e) => tracing::warn!(error = %e, "writer gone during .gitignore rebuild"),
                    }
                }
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

/// Coalescing window for incremental note changes → one `views_invalidated` (plan
/// editor-ui-round-3 WP-2).
pub(crate) const VIEWS_COALESCE: Duration = Duration::from_millis(150);

/// Forwards `index_generation` to the view registry, and coalesces `file_changed` (daemon- and
/// external-origin; the index is already updated when it is sent) into one [`ViewRegistry::notify_notes_changed`] per [`VIEWS_COALESCE`] window.
async fn generation_listener(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let mut rx = shared.subscribe();
    let mut due: Option<tokio::time::Instant> = None;
    loop {
        let flush = async {
            match due {
                Some(t) => tokio::time::sleep_until(t).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = stop.changed() => return,
            () = flush => {
                due = None;
                shared.views.notify_notes_changed();
            }
            r = rx.recv() => match r {
                Ok(Notification::IndexGeneration { n }) => {
                    // A generation invalidates everything itself; a pending batch is subsumed.
                    due = None;
                    shared.views.notify_generation(n);
                }
                Ok(Notification::FileChanged { .. }) => {
                    due.get_or_insert_with(|| tokio::time::Instant::now() + VIEWS_COALESCE);
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(k)) => {
                    tracing::warn!(skipped = k, "generation listener lagged");
                    due.get_or_insert_with(|| tokio::time::Instant::now() + VIEWS_COALESCE);
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

/// Polls `CV_CONFIG_DIR`; on change notifies view services, reloads the action config (WP-9) and
/// broadcasts `config_reloaded{errors}` with the loader's errors.
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
                    shared.lookups.invalidate_models();
                    let errors = shared.runner().map(Runner::reload_config).unwrap_or_default();
                    shared.publish(Notification::ConfigReloaded { errors });
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

#[cfg(test)]
mod error_map_tests {
    use super::*;

    /// WP-R3 (B2): policy refusals and unavailability get their own codes, not INTERNAL_ERROR.
    #[test]
    fn vault_and_state_errors_map_to_their_codes() {
        use error_codes as c;
        let s = |s: &str| s.to_owned();
        let cases = [
            (
                VaultError::LinkWriteRefused {
                    path: s("a.md"),
                    reason: s("r"),
                },
                c::REFUSED,
            ),
            (
                VaultError::ProtectedRoot {
                    root: s("r"),
                    protected: s("p"),
                },
                c::REFUSED,
            ),
            (VaultError::WriterGone, c::UNAVAILABLE),
            (VaultError::NotFound(s("a.md")), c::NOT_FOUND),
            (VaultError::Exists(s("a.md")), c::INVALID_PARAMS),
            (VaultError::InvalidPath(s("../a")), c::INVALID_PARAMS),
        ];
        for (e, code) in cases {
            let msg = e.to_string();
            let r = rpc_from_vault(e);
            assert_eq!(r.code, code, "{msg}");
        }
        let wrong = StateError::WrongStatus {
            id: s("j"),
            status: cv_state::JobStatus::Running,
            expected: "queued",
        };
        let r = rpc_from_state(wrong);
        assert_eq!(
            (r.code, r.message.as_str()),
            (c::INVALID_PARAMS, "job j is running, expected queued")
        );
        assert_eq!(
            rpc_from_state(StateError::NoSuchJob(s("j"))).message,
            "no job j"
        );
        let conflict = rpc_from_vault(VaultError::Conflict {
            path: s("a"),
            expected: None,
            found: None,
        });
        assert_eq!(conflict.code, c::CONFLICT);
        assert_eq!(conflict.data, Some(json!({ "current_hash": null })));
    }
}
