//! `cv daemon`: startup, shared state, request dispatch and shutdown.
//!
//! Submodules: [`errors`] (RPC error mapping), [`notes`] (vault/note RPCs), [`queue`]
//! (job/queue/intake/tracker/status RPCs), [`bridge`] (vault events → notifications → views),
//! [`background`] (spool drain, config poller).
//!
//! Startup order (brief pin 4): config → `StateDb` → `VaultFs` →
//! `VaultWriter::open` → writer+watcher thread — all on the calling thread, before any tokio
//! runtime exists ([`Daemon::start`]) — then, inside a runtime, bind the socket → drain the spool
//! → serve ([`Daemon::spawn`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use cv_protocol::{
    ChangeKind, CreateNoteResult, DeleteMode, DeleteNoteParams, DeleteNoteResult, DeleteParams,
    DeletePreviewItem, DeletePreviewResult, DeleteResult, HelloResult, IntakeListResult, JobStatus,
    JobsListResult, Method, Notification, Origin, PROTOCOL_VERSION, PathParams, RenameParams,
    RenameResult, Request, RpcError, StatusResult, UriResult, ViewsListResult,
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
use crate::sync::{lock, read, write};
use crate::uri::{self, UriAction};
use crate::views::{ViewError, ViewRegistry};
use crate::{now_ms, spool};

mod background;
mod bridge;
mod errors;
mod mail_watch;
mod memory;
mod notes;
mod queue;
mod read_sync;
mod trash;

pub(crate) use background::clear_stale_socket;
use background::{config_poller, drain_spool};
use bridge::{generation_listener, start_event_bridge};
pub use errors::rpc_err;
pub(crate) use errors::{JobError, rpc_from_state, rpc_from_vault};
use errors::{internal, reply, unavailable};

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
    memory: memory::Memory,
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
        let fs = VaultFs::live(&config.vault)
            .context("opening the vault")?
            .with_link_write_roots(config.link_write_roots.clone());
        let vault_root = fs.root().to_path_buf();
        // `open` creates the index when absent and rebuilds on a schema/generation mismatch.
        let mut writer = VaultWriter::open(fs, &config.index_db).context("opening the index")?;
        // Plan tidiness-trash-orphans WP-1: every trash move reaches `trash_entries`.
        trash::install_ledger(&mut writer, Arc::clone(&state));
        let (handle, vault_events, writer_join) =
            cv_vault::spawn_writer_with_watcher(writer, opts.watch).context("starting writer")?;
        let views = ViewRegistry::start(
            &config.config_dir,
            &config.index_db,
            Some(&config.state_db),
            events.clone(),
        )
        .context("starting the view host")?;
        // `tracker_items` inserts invalidate `source = "tracker_items"` views (round-5 WP-10).
        lock(&state).set_change_hook(views.change_hook());
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
                memory: memory::Memory::new(),
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
            data_home: shared.config.data_home.clone(),
        });
        runner.reload_config();
        if shared.runner.set(runner.clone()).is_err() {
            bail!("runner already started");
        }
        trash::apply_trash_config(&shared);
        memory::register_gauges(&shared);
        let mut tasks = vec![
            runner.spawn_claim_loop(stop_rx.clone()),
            runner.spawn_event_starter(stop_rx.clone()),
            runner.spawn_scheduler(stop_rx.clone()),
            mail_watch::spawn(runner.clone(), stop_rx.clone()),
        ];
        tasks.extend([
            tokio::spawn(generation_listener(Arc::clone(&shared), stop_rx.clone())),
            tokio::spawn(memory::memory_logger(Arc::clone(&shared), stop_rx.clone())),
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
        read(&self.writer).clone()
    }

    fn drop_writer(&self) {
        *write(&self.writer) = None;
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
            Request::ReconcileIntake(p) => {
                reply::<m::ReconcileIntake>(self.reconcile_intake(p).await)
            }
            Request::Uri(p) => reply::<m::Uri>(self.intake(&p.url).await),
            Request::Status => reply::<m::Status>(match self.status() {
                Ok(mut s) => {
                    s.memory = Some(Box::new(self.memory_status().await));
                    Ok(s)
                }
                Err(e) => Err(e),
            }),
            Request::ViewsList => reply::<m::ViewsList>(Ok(ViewsListResult {
                view_ids: self.views.view_ids(),
            })),
            Request::ViewRun(p) => reply::<m::ViewRun>(
                self.views
                    .run(&p.view_id, p.this_path, p.sort, p.select)
                    .await
                    .map_err(|e| match e {
                        ViewError::NotFound(id) => {
                            rpc_err(error_codes::NOT_FOUND, format!("no view {id}"))
                        }
                        ViewError::Failed(m) => internal(m),
                    }),
            ),
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
            Request::MemoryReport(p) => {
                self.memory_report(p);
                reply::<m::MemoryReport>(Ok(()))
            }
        }
    }
}
