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
mod rematch;
mod rpc;
mod schedule;
mod stage_guard;

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
    self, Admission, ChildSpawner, DeferCause, StageAdmitter, StepCtx, StepError, StepOutcome,
    TRANSIENT_BASE_MS, TRANSIENT_CAP_MS, TRANSIENT_MAX_ATTEMPTS,
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
pub use stage_guard::ModelGuard;

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
    /// Admitted through the stage seam ([`Runner::admit_stage`], plan intake-stage-jobs WP-1).
    /// This trigger is the durable seam marker: the guard, the no-terminal-outage rule and the
    /// reload re-tag find a seam job by it after a restart.
    Stage {
        /// The note's bound id (also its `note-id::` resource key).
        note: cv_protocol::NoteId,
    },
}

impl JobParams {
    /// Whether the job was enqueued through the stage seam.
    #[must_use]
    pub fn is_stage(&self) -> bool {
        matches!(self.trigger, Trigger::Stage { .. })
    }
}

/// The resource key every seam job of one note carries (follows the note across a rename,
/// unlike a path key).
#[must_use]
pub fn note_resource_key(id: cv_protocol::NoteId) -> String {
    format!("note-id::{}", id.0)
}

/// Whether a stage is still runnable for a note, asked on a seam job's first claim (no
/// checkpoint yet). The daemon installs the stage model's [`ModelGuard`] through
/// [`Runner::set_stage_guard`]; a bare runner (tests) keeps [`AlwaysRun`].
pub trait StageGuard: Send + Sync {
    /// `stage` (an [`cv_actions::STAGE_NAMES`] name) for the job's note; `false` settles the
    /// job Done with no attempt.
    fn runnable(&self, stage: &str, context: &ActionContext, config: &ConfigSet) -> bool;
}

/// The stub guard: every stage is runnable.
#[derive(Debug, Default, Clone, Copy)]
pub struct AlwaysRun;

impl StageGuard for AlwaysRun {
    fn runnable(&self, _: &str, _: &ActionContext, _: &ConfigSet) -> bool {
        true
    }
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
    /// A stage admission was refused (not armed, no stage marker, no bound note id, no key).
    Admission(String),
    /// The store failed.
    State(String),
}

impl std::fmt::Display for EnqueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnqueueError::NoResourceKeys(m) => write!(f, "no resource keys: {m}"),
            EnqueueError::Admission(m) => write!(f, "stage admission refused: {m}"),
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
    /// A step run in this claim reported a real provider attempt (replayed checkpoints never
    /// count; plan intake-stage-jobs WP-1).
    pub attempted: bool,
    /// The step that failed the run is a stage body of the action's stage, so it recorded the
    /// failure itself (plan intake-stage-jobs WP-3; the settle site does not record it again).
    pub stage_recorded: bool,
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
    /// The stage-job guard (the daemon installs [`ModelGuard`]; [`AlwaysRun`] otherwise).
    stage_guard: RwLock<Arc<dyn StageGuard>>,
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
                stage_guard: RwLock::new(Arc::new(AlwaysRun)),
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

    /// Replaces the config snapshot (tests; the daemon uses [`Runner::reload_config`]). Queued
    /// stage jobs are re-tagged against it.
    pub fn set_config(&self, c: ConfigSet) {
        *write(&self.inner.config) = Arc::new(c);
        self.inner.retag_queued_stage_jobs();
    }

    /// Installs the stage-job guard (the daemon installs [`ModelGuard`]; tests inject their own).
    pub fn set_stage_guard(&self, g: Arc<dyn StageGuard>) {
        *write(&self.inner.stage_guard) = g;
    }

    /// Admits the stage action `action_id` for the note at `path` through the stage seam.
    pub fn admit_stage(&self, action_id: &str, path: &str) -> Result<Admission, EnqueueError> {
        let config = self.config();
        self.inner.admit_stage(&config, action_id, path)
    }

    /// Re-reads `CV_CONFIG_DIR` (hot reload, pin 2); returns the current errors.
    pub fn reload_config(&self) -> Vec<String> {
        self.reload().0
    }

    /// [`Self::reload_config`] for an observed change (the config watcher): when the reload
    /// changed the typed `sources.toml`, enqueues the sources-only re-match (plan
    /// sources-publication-model WP-4). Start-up uses [`Self::reload_config`] and never enqueues.
    pub fn reload_config_observed(&self) -> Vec<String> {
        let (errors, sources_changed) = self.reload();
        if sources_changed {
            self.prune_learned_hosts();
            self.enqueue_sources_rematch();
        }
        errors
    }

    /// The reload; also whether the typed sources differ from the previous snapshot's.
    fn reload(&self) -> (Vec<String>, bool) {
        let before = self.config();
        let mut next = (*before).clone();
        load_config_dir(&self.inner.config_dir, &mut next);
        let sources_changed = before.sources() != next.sources();
        let errors = next.errors();
        for e in &errors {
            tracing::warn!(error = %e, "config");
        }
        for n in next.notices() {
            tracing::info!(notice = %n, "config");
        }
        self.set_config(next);
        (errors, sources_changed)
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

    /// The id of the file at `path` (`Ok(None)`: no file or no writer; `Err`: the id store
    /// failed). An id already bound is read from `state.db` (the writer persists every binding
    /// before its mutation returns), so an enqueue does not wait behind a busy writer; only a path
    /// without one is minted, on the writer thread. Never call while holding the state mutex.
    pub(super) fn bind_existing(&self, path: &str) -> Result<Option<cv_protocol::NoteId>, String> {
        match lock(&self.state).note_id_at(path) {
            Ok(Some(id)) => return Ok(Some(id)),
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "note id lookup failed; binding on the writer"),
        }
        let Some(w) = self.writer.writer() else {
            return Ok(None);
        };
        let p = path.to_owned();
        w.call(move |vw| match vw.fs().hash(&p) {
            Ok(Some(_)) => vw.bind_note(&p).map(Some).map_err(|e| e.to_string()),
            _ => Ok(None),
        })
        .map_err(|e| e.to_string())?
    }

    /// The queue row for a run of `action` (resource keys derived, lane resolved).
    fn new_job(&self, action: &Action, mut req: RunRequest) -> Result<NewJob, EnqueueError> {
        // Plan note-identity D5: the job keeps its note's id (the triggering event's, else bound
        // now), persisted in its params so a retry still has it. A path naming no file yet (or a
        // store failure) stays unbound here; the runner's `refresh` binds it before each step.
        if let (Some(path), None) = (&req.context.active_path, req.context.active_id) {
            req.context.active_id = self.bind_existing(path).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "note id bind failed; bound before the first step");
                None
            });
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

    /// The stage seam (plan intake-stage-jobs WP-1): one chain-free background job of a
    /// stage-marked `action_id` on the note at `path`, deduped on `{action}::{note id}`, its
    /// `service` set at enqueue ([`steps::action_service_key`]) and keyed by the note id.
    pub(crate) fn admit_stage(
        &self,
        config: &ConfigSet,
        action_id: &str,
        path: &str,
    ) -> Result<Admission, EnqueueError> {
        let refuse = |m: String| Err(EnqueueError::Admission(m));
        let Some(action) = config.armed(action_id) else {
            return refuse(format!(
                "action {action_id:?} is not armed: {}",
                config.disarm_reason(action_id).unwrap_or("unknown action")
            ));
        };
        if action.stage.is_none() {
            return refuse(format!("action {action_id:?} has no stage marker"));
        }
        let id = match lock(&self.state).note_id_at(path) {
            Ok(Some(id)) => id,
            Ok(None) => return refuse("the note has no bound id; bind it and retry".into()),
            Err(e) => return Err(EnqueueError::State(e.to_string())),
        };
        let service = steps::action_service_key(action, config).map_err(EnqueueError::Admission)?;
        let context = ActionContext {
            active_path: Some(path.to_owned()),
            active_id: Some(id),
            ..ActionContext::default()
        };
        let req = RunRequest::background(
            action,
            Trigger::Stage { note: id },
            context,
            &id.0.to_string(),
        );
        let mut job = self.new_job(action, req)?;
        job.resource_keys.push(note_resource_key(id));
        job.resource_keys.sort();
        job.resource_keys.dedup();
        job.service = service;
        let out = lock(&self.state)
            .enqueue(job, self.clock.now_ms())
            .map_err(|e| EnqueueError::State(e.to_string()))?;
        let admission = match &out {
            EnqueueOutcome::Enqueued(id) => Admission::Inserted(id.clone()),
            EnqueueOutcome::Duplicate(id) => Admission::Duplicate(id.clone()),
        };
        self.enqueued(out)?;
        Ok(admission)
    }

    /// The tag a seam job of `jp` should carry under `config` (`Err`: the action is gone,
    /// unmarked or unresolvable; the job is left as it is and fails at claim as today).
    pub(crate) fn stage_tag(config: &ConfigSet, jp: &JobParams) -> Result<Option<String>, String> {
        let action = config
            .armed(&jp.action_id)
            .ok_or_else(|| format!("action {:?} is not armed", jp.action_id))?;
        steps::action_service_key(action, config)
    }

    /// Reload re-tag: every queued seam job's `service` is re-resolved under the current config.
    fn retag_queued_stage_jobs(&self) {
        let config = Arc::clone(&read(&self.config));
        let mut db = lock(&self.state);
        let queued = match db.queued_jobs() {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!(error = %e, "re-tag: listing queued jobs failed");
                return;
            }
        };
        for job in queued {
            let Ok(jp) = serde_json::from_value::<JobParams>(job.params.clone()) else {
                continue;
            };
            if !jp.is_stage() {
                continue;
            }
            let Ok(tag) = Self::stage_tag(&config, &jp) else {
                continue;
            };
            if tag != job.service
                && let Err(e) = db.retag_queued(&job.id, tag.as_deref())
            {
                tracing::warn!(job_id = %job.id, error = %e, "re-tag failed");
            }
        }
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
#[cfg(test)]
mod tests_stage;
