//! The action runner (plan `v0-core-daemon` WP-9): every run is a queue job (Decisions locked —
//! one execution path, no inline runner), claimed under I3 by [`cv_state::StateDb::claim`], with
//! steps executed sequentially on a blocking thread and per-step checkpoints (audit E2).
//!
//! - User-started runs take [`Lane::User`], event/schedule runs [`Lane::Background`] (Crucible
//!   `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/lanes.ts:3-6`).
//! - Replaces Crucible's chain runner (`src/chains.ts`) and command dispatch
//!   (`src/internalCommands.ts`): Obsidian's command model (inventory §3) is dropped — there is no
//!   command registry, no hidden commands and no `mutating` flag; the queue + resource keys are
//!   the only serialization.

mod claim;
mod clock;
mod config_load;
mod events;
mod execute;
mod holds;
mod rpc;
mod schedule;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use cv_actions::{Action, ConfigSet, LaneChoice, Step};
use cv_protocol::Lane;
use cv_protocol::{ActionContext, JobState, JobStatus, Notification};
use cv_state::{BreakerFailure, EnqueueOutcome, JobResult, NewJob, StateDb};
use cv_vault::WriterHandle;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::{Notify, broadcast, watch};

use crate::steps::{
    self, ChildSpawner, DeferCause, StepCtx, StepError, StepOutcome, TRANSIENT_BASE_MS,
    TRANSIENT_CAP_MS, TRANSIENT_MAX_ATTEMPTS,
};
use crate::sync::{lock, read, write};

#[cfg(test)]
use claim::run_job;
use claim::{Pool, claim_loop, failed};
pub use clock::{Clock, ManualClock, SystemClock};
pub use config_load::load_config_dir;
pub use execute::apply_defaults;
use execute::execute_steps;
pub use holds::HoldsView;
use holds::{Running, RunningJob};
pub use rpc::{actions_list, run_action};

/// Maximum `run_action` nesting (pin 10).
pub const MAX_CHAIN_DEPTH: usize = 4;
/// Cycle-guard window (I5, pin 9).
pub const CYCLE_WINDOW: Duration = Duration::from_secs(10);
/// How long a non-durable `run_action` waits for completion.
pub const NON_DURABLE_WAIT: Duration = Duration::from_secs(10);
/// Upper bound on the claim loop's idle sleep (a safety net beside `Notify`).
const IDLE_POLL: Duration = Duration::from_secs(1);
/// Remembered finished runs (for non-durable `open_path` replies).
const FINISHED_CAP: usize = 256;
/// Period of the claim loop's lease-recovery / prune maintenance (plan WP-2).
pub(crate) const MAINTENANCE_EVERY_MS: i64 = 60_000;
/// Terminal-row pruning runs at most this often.
pub(crate) const PRUNE_EVERY_MS: i64 = 3_600_000;

/// When the claim loop last ran each maintenance task (`None` = never).
#[derive(Debug, Default)]
pub(crate) struct Maintenance {
    last_recover_ms: Option<i64>,
    last_prune_ms: Option<i64>,
}

/// Executes one step. Production: [`ProductionExecutor`] (→ [`steps::execute`]); tests inject
/// counting/failing executors.
pub trait StepExecutor: Send + Sync {
    /// Runs `step` (index `ctx.step_index`).
    fn execute(&self, step: &Step, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError>;
}

/// [`steps::execute`].
#[derive(Debug, Default, Clone, Copy)]
pub struct ProductionExecutor;

impl StepExecutor for ProductionExecutor {
    fn execute(&self, step: &Step, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        steps::execute(step, ctx)
    }
}

/// Where the runner gets the writer (the daemon's `Shared`; tests a fixed handle or none).
pub trait WriterSource: Send + Sync {
    /// The writer, or `None` once shutdown began.
    fn writer(&self) -> Option<WriterHandle>;
}

impl WriterSource for Option<WriterHandle> {
    fn writer(&self) -> Option<WriterHandle> {
        self.clone()
    }
}

impl WriterSource for Weak<crate::daemon::Shared> {
    fn writer(&self) -> Option<WriterHandle> {
        self.upgrade().and_then(|s| s.writer())
    }
}

/// What started a run (stored in the job params).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trigger {
    /// `run_action` RPC.
    Command,
    /// A live watcher event.
    Event {
        /// Note path.
        path: String,
        /// `created|modified|deleted|renamed`.
        change: cv_protocol::ChangeKind,
    },
    /// A schedule (or its one catch-up).
    Schedule {
        /// The due time that fired.
        due_ms: i64,
    },
    /// A `run_action` step of another job.
    Child {
        /// Parent job id.
        parent: String,
    },
}

/// Job params of `action:<id>` jobs: `{action_id, params, context, trigger, chain}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobParams {
    /// Action id.
    pub action_id: String,
    /// Run params.
    #[serde(default)]
    pub params: Map<String, Value>,
    /// Context.
    #[serde(default)]
    pub context: ActionContext,
    /// Trigger.
    pub trigger: Trigger,
    /// Ancestor action ids (`run_action` call chain), outermost first.
    #[serde(default)]
    pub chain: Vec<String>,
}

/// A run to enqueue.
#[derive(Debug, Clone)]
pub struct RunRequest {
    /// Params (defaults already applied).
    pub params: Map<String, Value>,
    /// Context.
    pub context: ActionContext,
    /// Trigger.
    pub trigger: Trigger,
    /// Call chain.
    pub chain: Vec<String>,
    /// Lane used when the action's lane is `auto`.
    pub default_lane: Lane,
    /// Un-namespaced dedupe key.
    pub dedupe_key: Option<String>,
}

impl RunRequest {
    /// A `run_action` RPC run (user lane, no dedupe).
    #[must_use]
    pub fn command(params: Map<String, Value>, context: ActionContext) -> Self {
        Self {
            params,
            context,
            trigger: Trigger::Command,
            chain: Vec::new(),
            default_lane: Lane::User,
            dedupe_key: None,
        }
    }

    /// A background run of `action` with its param defaults, deduped on `{action}::{key}`.
    fn background(action: &Action, trigger: Trigger, context: ActionContext, key: &str) -> Self {
        let mut params = Map::new();
        apply_defaults(action, &mut params);
        Self {
            params,
            context,
            trigger,
            chain: Vec::new(),
            default_lane: Lane::Background,
            dedupe_key: Some(format!("{}::{key}", action.id)),
        }
    }

    /// An event-starter run on `path` (deduped per action and path).
    #[must_use]
    pub fn event(action: &Action, path: &str, change: cv_protocol::ChangeKind) -> Self {
        let context = ActionContext {
            active_path: Some(path.to_owned()),
            ..ActionContext::default()
        };
        let trigger = Trigger::Event {
            path: path.to_owned(),
            change,
        };
        Self::background(action, trigger, context, path)
    }

    /// A schedule run due at `due_ms` (deduped per action).
    #[must_use]
    pub fn schedule(action: &Action, due_ms: i64) -> Self {
        let trigger = Trigger::Schedule { due_ms };
        Self::background(action, trigger, ActionContext::default(), "schedule")
    }

    /// A `run_action` step's child run (inherits the parent's context and lane).
    #[must_use]
    pub fn child(
        params: Map<String, Value>,
        context: ActionContext,
        parent: &str,
        chain: Vec<String>,
        lane: Lane,
    ) -> Self {
        Self {
            params,
            context,
            trigger: Trigger::Child {
                parent: parent.to_owned(),
            },
            chain,
            default_lane: lane,
            dedupe_key: None,
        }
    }
}

/// Enqueue refusals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueError {
    /// Resource keys could not be derived (fail closed, never keyless).
    NoResourceKeys(String),
    /// The store failed.
    State(String),
}

impl std::fmt::Display for EnqueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnqueueError::NoResourceKeys(m) => write!(f, "no resource keys: {m}"),
            EnqueueError::State(m) => write!(f, "state.db: {m}"),
        }
    }
}

/// Result of a finished run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSummary {
    /// The last `open_path` any step emitted.
    pub open_path: Option<String>,
    /// Accumulated output paths.
    pub output_paths: Vec<String>,
    /// Why the run deferred (set only when the result is `Deferred`).
    pub defer_cause: Option<DeferCause>,
}

/// Runner construction parameters.
pub struct RunnerDeps {
    /// The daemon's single `StateDb`.
    pub state: Arc<Mutex<StateDb>>,
    /// The writer source.
    pub writer: Arc<dyn WriterSource>,
    /// Notification broadcast.
    pub events: broadcast::Sender<Notification>,
    /// Step executor.
    pub executor: Arc<dyn StepExecutor>,
    /// Clock.
    pub clock: Arc<dyn Clock>,
    /// Concurrent jobs in the general pool (any job; the daemon runs 3).
    pub concurrency: usize,
    /// Permits of the fast pool (plan job-slots-fast-lane WP-2): only jobs of armed `fast`
    /// actions claim them; 0 = single-pool behavior.
    pub fast_slots: usize,
    /// Vault root.
    pub vault_root: PathBuf,
    /// `CV_INDEX_DB` base.
    pub index_db: PathBuf,
    /// `CV_CONFIG_DIR`.
    pub config_dir: PathBuf,
    /// `CV_SEARCH_DB`: opened lazily by the first live search step (plan semsearch-crate WP-3).
    pub search_db: PathBuf,
    /// `Config::data_home` (`XDG_DATA_HOME`): the system trash is `<data_home>/Trash`, the same
    /// root moves to the system trash use; steps read it as `StepCtx::system_trash`.
    pub data_home: Option<PathBuf>,
}

pub(crate) struct Inner {
    state: Arc<Mutex<StateDb>>,
    writer: Arc<dyn WriterSource>,
    events: broadcast::Sender<Notification>,
    executor: Arc<dyn StepExecutor>,
    clock: Arc<dyn Clock>,
    concurrency: usize,
    fast_slots: usize,
    vault_root: PathBuf,
    index_db: PathBuf,
    config_dir: PathBuf,
    system_trash: Option<PathBuf>,
    /// The one shared `semsearch` handle per process (Decision 7).
    search: cv_search::SearchDb,
    config: RwLock<Arc<ConfigSet>>,
    wake: Notify,
    /// Every claimed job in this runner (`job id -> RunningJob`), inserted at claim and removed
    /// at one site, the settle in `claim::run_job`, together with the permit's release, so the
    /// pool record and the note hold clear together. Cancellation and stale-lease recovery read
    /// all keys; permit-wait holders are named from each entry's claim-time `pool` and
    /// `job_type` (plan job-slots-fast-lane WP-2, audit E1), never re-derived from the current
    /// fast set; only jobs whose action holds its note (plan round-17 WP-2, narrowed by plan
    /// round-18 Decision 1 to [`ConfigSet::action_holds_note`]) carry a `hold`. Shared (`Arc`)
    /// so the daemon's writer jobs can refuse client writes to a held note ([`HoldsView`]).
    running: Running,
    finished: Mutex<BTreeMap<String, RunSummary>>,
    cycle: Mutex<HashMap<(String, String), Instant>>,
}

/// The runner handle (cheap to clone).
#[derive(Clone)]
pub struct Runner {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner").finish_non_exhaustive()
    }
}

impl Runner {
    /// A runner with an empty config (call [`Runner::reload_config`] or [`Runner::set_config`]).
    pub fn new(deps: RunnerDeps) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: deps.state,
                writer: deps.writer,
                events: deps.events,
                executor: deps.executor,
                clock: deps.clock,
                concurrency: deps.concurrency.max(1),
                fast_slots: deps.fast_slots,
                vault_root: deps.vault_root,
                index_db: deps.index_db,
                config_dir: deps.config_dir,
                system_trash: deps.data_home.map(|h| h.join("Trash")),
                search: cv_search::SearchDb::new(deps.search_db),
                config: RwLock::new(Arc::new(ConfigSet::new())),
                wake: Notify::new(),
                running: Running::default(),
                finished: Mutex::new(BTreeMap::new()),
                cycle: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The shared search index (the `search` request queries through it, so feed writes are
    /// visible without reopening).
    pub(crate) fn search_db(&self) -> &cv_search::SearchDb {
        &self.inner.search
    }

    /// The current config snapshot.
    pub fn config(&self) -> Arc<ConfigSet> {
        Arc::clone(&read(&self.inner.config))
    }

    /// Replaces the config snapshot (tests; the daemon uses [`Runner::reload_config`]).
    pub fn set_config(&self, c: ConfigSet) {
        *write(&self.inner.config) = Arc::new(c);
    }

    /// Re-reads `CV_CONFIG_DIR` (hot reload, pin 2); returns the current errors.
    pub fn reload_config(&self) -> Vec<String> {
        let mut next = (*self.config()).clone();
        load_config_dir(&self.inner.config_dir, &mut next);
        let errors = next.errors();
        for e in &errors {
            tracing::warn!(error = %e, "config");
        }
        self.set_config(next);
        errors
    }

    /// Current config errors (`cv status` `config_errors`).
    pub fn config_errors(&self) -> Vec<String> {
        let cfg = self.config();
        let mut errors = cfg.errors();
        errors.extend(crate::steps::mail::maildir_write_error(
            &cfg.pipelines().mail,
        ));
        errors
    }

    /// The clock.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.inner.clock
    }

    /// Enqueues a run of `action` (never executes inline). Returns the job id (an existing one on
    /// dedupe).
    pub fn enqueue(&self, action: &Action, req: RunRequest) -> Result<String, EnqueueError> {
        self.inner.enqueue(action, req)
    }

    /// Requests cancellation of a job running in this runner; it stops before its next step.
    /// The request is the durable `cancel_requested` flag ([`cv_state::StateDb::cancel`]).
    pub fn request_cancel(&self, job_id: &str) -> bool {
        lock(&self.inner.running).contains_key(job_id)
            && lock(&self.inner.state)
                .cancel(job_id, self.inner.clock.now_ms())
                .is_ok()
    }

    /// The summary of a finished run (if remembered).
    pub fn finished(&self, job_id: &str) -> Option<RunSummary> {
        lock(&self.inner.finished).get(job_id).cloned()
    }

    /// Spawns the claim loop. It first recovers leases (own, foreign, stale), then claims until
    /// `stop` flips.
    pub fn spawn_claim_loop(&self, stop: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(claim_loop(Arc::clone(&self.inner), stop))
    }

    /// Spawns the event starter (subscribes now: only events after this call start runs, I4).
    pub fn spawn_event_starter(&self, stop: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
        let rx = self.inner.events.subscribe();
        tokio::spawn(events::event_loop(self.clone(), rx, stop))
    }

    /// Spawns the scheduler (its first tick is the one start-time catch-up, I4).
    pub fn spawn_scheduler(&self, stop: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(schedule::schedule_loop(self.clone(), stop))
    }

    /// One scheduler pass at the clock's now; returns the job ids enqueued.
    pub fn schedule_tick(&self) -> Vec<String> {
        schedule::tick(self)
    }

    /// Whether the cycle guard currently blocks `action` on `path` (I5).
    pub fn cycle_blocked(&self, action: &str, path: &str) -> bool {
        let mut m = lock(&self.inner.cycle);
        m.retain(|_, t| t.elapsed() < CYCLE_WINDOW);
        m.contains_key(&(action.to_owned(), path.to_owned()))
    }

    pub(crate) fn inner(&self) -> &Arc<Inner> {
        &self.inner
    }
}

impl Inner {
    fn enqueue(&self, action: &Action, req: RunRequest) -> Result<String, EnqueueError> {
        let job = self.new_job(action, req)?;
        let out = lock(&self.state)
            .enqueue(job, self.clock.now_ms())
            .map_err(|e| EnqueueError::State(e.to_string()))?;
        self.enqueued(out)
    }

    /// The id of the file at `path` (`None`: no file, no writer, or a store failure). An id
    /// already bound is read from `state.db` (the writer persists every binding before its
    /// mutation returns), so an enqueue does not wait behind a busy writer; only a path without
    /// one is minted, on the writer thread. Never call while holding the state mutex.
    fn bind_existing(&self, path: &str) -> Option<cv_protocol::NoteId> {
        match lock(&self.state).note_id_at(path) {
            Ok(Some(id)) => return Some(cv_protocol::NoteId(id)),
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "note id lookup failed; binding on the writer"),
        }
        let w = self.writer.writer()?;
        let p = path.to_owned();
        let r = w.call(move |vw| match vw.fs().hash(&p) {
            Ok(Some(_)) => vw.bind_note(&p),
            _ => None,
        });
        match r {
            Ok(id) => id.map(|i| cv_protocol::NoteId(i.0)),
            Err(e) => {
                tracing::warn!(error = %e, "note id bind failed; job unbound");
                None
            }
        }
    }

    /// The queue row for a run of `action` (resource keys derived, lane resolved).
    fn new_job(&self, action: &Action, mut req: RunRequest) -> Result<NewJob, EnqueueError> {
        // Plan note-identity D5: the job keeps its note's id (the triggering event's, else bound
        // now), persisted in its params so a retry still has it. No id = the job runs unbound.
        if let (Some(path), None) = (&req.context.active_path, req.context.active_id) {
            req.context.active_id = self.bind_existing(path);
        }
        let mut keys = action
            .resources
            .derive(req.context.active_path.as_deref(), &req.params)
            .map_err(EnqueueError::NoResourceKeys)?;
        // Step-intrinsic keys (plan auto-intake-sprint WP-3): never named in user config.
        keys.extend(
            action
                .steps
                .iter()
                .flat_map(|s| s.kind.intrinsic_resource_keys())
                .map(|k| (*k).to_owned()),
        );
        keys.sort();
        keys.dedup();
        if keys.is_empty() && action.mutates_note() {
            return Err(EnqueueError::NoResourceKeys(format!(
                "action {:?} mutates a note but no resource key derives (no active path?)",
                action.id
            )));
        }
        let lane = match action.lane {
            LaneChoice::Auto => req.default_lane,
            LaneChoice::User => Lane::User,
            LaneChoice::Background => Lane::Background,
        };
        let params = JobParams {
            action_id: action.id.clone(),
            params: req.params,
            context: req.context,
            trigger: req.trigger,
            chain: req.chain,
        };
        let params =
            serde_json::to_value(&params).map_err(|e| EnqueueError::State(e.to_string()))?;
        let job = NewJob {
            job_type: format!("action:{}", action.id),
            lane,
            dedupe_key: req.dedupe_key,
            params,
            resource_keys: keys,
            service: None,
        };
        Ok(job)
    }

    /// [`Self::enqueue`] for a scheduled run: the job and `action`'s `schedule_runs` row commit in
    /// one transaction (SR10).
    fn enqueue_scheduled(&self, action: &Action, req: RunRequest) -> Result<String, EnqueueError> {
        let job = self.new_job(action, req)?;
        let out = lock(&self.state)
            .enqueue_scheduled(job, &action.id, self.clock.now_ms())
            .map_err(|e| EnqueueError::State(e.to_string()))?;
        self.enqueued(out)
    }

    /// Publishes and wakes for a fresh enqueue; maps the outcome to the job id.
    fn enqueued(&self, out: EnqueueOutcome) -> Result<String, EnqueueError> {
        let id = match out {
            EnqueueOutcome::Enqueued(id) => {
                self.publish(&id, JobState::Pending, None, Vec::new(), None, None);
                id
            }
            EnqueueOutcome::Duplicate(id) => id,
        };
        self.wake.notify_one();
        Ok(id)
    }

    fn recover(&self) {
        // At startup every other token is dead (single writer); this handle holds no lease yet,
        // so foreign recovery covers every running row. The periodic pass handles stale leases.
        let foreign = lock(&self.state).recover_foreign_leases();
        tracing::info!(?foreign, "runner recovered leases");
    }

    /// Periodic queue maintenance from the claim loop: stale-lease recovery every
    /// [`MAINTENANCE_EVERY_MS`] (never touching this runner's live jobs) and terminal pruning at
    /// most every [`PRUNE_EVERY_MS`] with the configured retention (`0` = keep forever).
    pub(crate) fn maintenance_tick(&self, m: &mut Maintenance) {
        let now = self.clock.now_ms();
        if m.last_recover_ms
            .is_some_and(|t| now - t < MAINTENANCE_EVERY_MS)
        {
            return;
        }
        m.last_recover_ms = Some(now);
        let live: HashSet<String> = lock(&self.running).keys().cloned().collect();
        let retention_days = read(&self.config).pipelines().queue.retention_days;
        let mut db = lock(&self.state);
        match db.recover_stale_leases_except(now, &live) {
            Ok(0) => {}
            Ok(n) => tracing::debug!(recovered = n, "maintenance: stale leases requeued"),
            Err(e) => tracing::warn!(error = %e, "maintenance: stale lease recovery failed"),
        }
        if m.last_prune_ms.is_some_and(|t| now - t < PRUNE_EVERY_MS) {
            return;
        }
        m.last_prune_ms = Some(now);
        db.set_retention_ms(i64::from(retention_days).saturating_mul(86_400_000));
        match db.prune_terminal(now) {
            Ok(0) => {}
            Ok(n) => tracing::debug!(pruned = n, "maintenance: terminal jobs pruned"),
            Err(e) => tracing::warn!(error = %e, "maintenance: prune failed"),
        }
    }

    fn record_writes(&self, action: &str, paths: &[String]) {
        if paths.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut m = lock(&self.cycle);
        for p in paths {
            m.insert((action.to_owned(), p.clone()), now);
        }
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_daemon;
