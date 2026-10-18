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

mod config_load;
mod events;
mod rpc;
mod schedule;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak};
use std::time::{Duration, Instant};

use cv_actions::{Action, ConfigSet, LaneChoice, Step};
use cv_protocol::{ActionContext, JobState, JobStatus, Notification};
use cv_state::{EnqueueOutcome, JobResult, Lane, NewJob, StateDb};
use cv_vault::WriterHandle;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::{Notify, broadcast, watch};

use crate::steps::{
    self, ChildSpawner, DeferCause, StepCtx, StepError, StepOutcome, TRANSIENT_BASE_MS,
    TRANSIENT_CAP_MS, TRANSIENT_MAX_ATTEMPTS,
};

pub use config_load::load_config_dir;
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

/// Time source (tests inject [`ManualClock`]).
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> i64;
    /// Local UTC offset in ms at `now_ms` (daily schedules).
    fn local_offset_ms(&self, now_ms: i64) -> i64;
}

/// The wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        crate::now_ms()
    }
    fn local_offset_ms(&self, now_ms: i64) -> i64 {
        use chrono::{Local, Offset, TimeZone};
        Local
            .timestamp_millis_opt(now_ms)
            .single()
            .map_or(0, |t| i64::from(t.offset().fix().local_minus_utc()) * 1000)
    }
}

/// A settable clock (UTC).
#[derive(Debug, Default)]
pub struct ManualClock(AtomicI64);

impl ManualClock {
    /// Starts at `ms`.
    pub fn new(ms: i64) -> Self {
        Self(AtomicI64::new(ms))
    }
    /// Sets the time.
    pub fn set(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn local_offset_ms(&self, _: i64) -> i64 {
        0
    }
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
        change: String,
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
    /// Concurrent jobs (default 2).
    pub concurrency: usize,
    /// Vault root.
    pub vault_root: PathBuf,
    /// `CV_INDEX_DB` base.
    pub index_db: PathBuf,
    /// `CV_CONFIG_DIR`.
    pub config_dir: PathBuf,
    /// `CV_SEARCH_DB`: opened lazily by the first live search step (plan semsearch-crate WP-3).
    pub search_db: PathBuf,
}

pub(crate) struct Inner {
    state: Arc<Mutex<StateDb>>,
    writer: Arc<dyn WriterSource>,
    events: broadcast::Sender<Notification>,
    executor: Arc<dyn StepExecutor>,
    clock: Arc<dyn Clock>,
    concurrency: usize,
    vault_root: PathBuf,
    index_db: PathBuf,
    config_dir: PathBuf,
    /// The one shared `semsearch` handle per process (Decision 7).
    search: cv_search::SearchDb,
    config: RwLock<Arc<ConfigSet>>,
    wake: Notify,
    running: Mutex<HashSet<String>>,
    cancel_requested: Mutex<HashSet<String>>,
    finished: Mutex<BTreeMap<String, RunSummary>>,
    cycle: Mutex<HashMap<(String, String), Instant>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
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
                vault_root: deps.vault_root,
                index_db: deps.index_db,
                config_dir: deps.config_dir,
                search: cv_search::SearchDb::new(deps.search_db),
                config: RwLock::new(Arc::new(ConfigSet::new())),
                wake: Notify::new(),
                running: Mutex::new(HashSet::new()),
                cancel_requested: Mutex::new(HashSet::new()),
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
        Arc::clone(
            &self
                .inner
                .config
                .read()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Replaces the config snapshot (tests; the daemon uses [`Runner::reload_config`]).
    pub fn set_config(&self, c: ConfigSet) {
        *self
            .inner
            .config
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::new(c);
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
        self.config().errors()
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
    pub fn request_cancel(&self, job_id: &str) -> bool {
        if lock(&self.inner.running).contains(job_id) {
            lock(&self.inner.cancel_requested).insert(job_id.to_owned());
            true
        } else {
            false
        }
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
        let keys = action
            .resources
            .derive(req.context.active_path.as_deref(), &req.params)
            .map_err(EnqueueError::NoResourceKeys)?;
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
        let out = lock(&self.state)
            .enqueue(job, self.clock.now_ms())
            .map_err(|e| EnqueueError::State(e.to_string()))?;
        let id = match out {
            EnqueueOutcome::Enqueued(id) => {
                self.publish_status(&id, JobState::Pending, None, Vec::new(), None);
                id
            }
            EnqueueOutcome::Duplicate(id) => id,
            EnqueueOutcome::Suppressed => {
                return Err(EnqueueError::State("enqueue suppressed".into()));
            }
        };
        self.wake.notify_one();
        Ok(id)
    }

    fn publish_status(
        &self,
        id: &str,
        status: JobState,
        message: Option<String>,
        output_paths: Vec<String>,
        open_path: Option<String>,
    ) {
        let _ = self.events.send(Notification::JobUpdated(JobStatus {
            job_id: id.to_owned(),
            status,
            message,
            output_paths,
            open_path,
        }));
    }

    fn recover(&self) {
        let mut db = lock(&self.state);
        let now = self.clock.now_ms();
        let token = db.claim_token().to_owned();
        let own = db.recover_own_leases(&token);
        let foreign = db.recover_foreign_leases();
        let stale = db.recover_stale_leases(now);
        tracing::info!(?own, ?foreign, ?stale, "runner recovered leases");
    }

    /// Breaker feeding + bounded transient retry (plan WP-2), run after the steps and before
    /// `settle`. Takes the `state` lock only for the short breaker/queue calls.
    pub(crate) fn apply_service_policy(
        &self,
        job: &cv_state::Job,
        result: JobResult,
        cause: Option<DeferCause>,
    ) -> JobResult {
        let now = self.clock.now_ms();
        let mut db = lock(&self.state);
        let log = |e: cv_state::StateError| tracing::warn!(job_id = %job.id, error = %e, "service health update failed");
        let result = match (result, cause) {
            (
                JobResult::Deferred {
                    retry_after_ms,
                    service_unhealthy: Some(svc),
                },
                Some(DeferCause::Transient(kind)),
            ) => {
                // Transient: the breaker computes its own doubling window (no retry_after).
                let reason = format!("transient {kind:?} (attempt {})", job.attempts);
                if let Err(e) = db.report_service_failure(&svc, kind, &reason, None, now) {
                    log(e);
                }
                if job.attempts >= TRANSIENT_MAX_ATTEMPTS {
                    JobResult::Failed {
                        error: format!(
                            "service {svc} still failing after {} attempts",
                            job.attempts
                        ),
                        reason: Some(cv_state::FailureReason::ServiceOutage),
                    }
                } else {
                    let shift = u32::try_from((job.attempts - 1).clamp(0, 20)).unwrap_or(20);
                    let backoff = TRANSIENT_BASE_MS
                        .saturating_mul(1_i64 << shift)
                        .min(TRANSIENT_CAP_MS);
                    JobResult::Deferred {
                        retry_after_ms: retry_after_ms.max(backoff),
                        service_unhealthy: Some(svc),
                    }
                }
            }
            (
                JobResult::Deferred {
                    retry_after_ms,
                    service_unhealthy: Some(svc),
                },
                Some(DeferCause::Quota),
            ) => {
                if let Err(e) = db.report_service_failure(
                    &svc,
                    cv_state::ServiceFailureKind::RateLimited,
                    "quota exhausted",
                    Some(retry_after_ms),
                    now,
                ) {
                    log(e);
                }
                JobResult::Deferred {
                    retry_after_ms,
                    service_unhealthy: Some(svc),
                }
            }
            (r, _) => r,
        };
        let Some(svc) = job.service.as_deref() else {
            return result;
        };
        match &result {
            JobResult::Done => {
                let before = db.service_health(svc).map(|h| h.state);
                if let Err(e) = db.report_service_success(svc) {
                    log(e);
                } else if matches!(before, Ok(s) if s != cv_state::BreakerState::Closed) {
                    match db.requeue_service_failures(false) {
                        Ok(b) => tracing::info!(
                            service = svc,
                            requeued = b.requeued,
                            "breaker closed; requeued service failures"
                        ),
                        Err(e) => log(e),
                    }
                }
            }
            _ => {
                // A job that held the half-open probe and ended without a verdict returns it
                // (a transient failure above already reopened the breaker, clearing it).
                if let Ok(h) = db.service_health(svc)
                    && h.state == cv_state::BreakerState::HalfOpen
                    && h.probe_in_flight()
                    && let Err(e) = db.release_probe(svc)
                {
                    log(e);
                }
            }
        }
        result
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
        let live = lock(&self.running).clone();
        let retention_days = self
            .config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .pipelines()
            .queue
            .retention_days;
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

async fn claim_loop(inner: Arc<Inner>, mut stop: watch::Receiver<bool>) {
    inner.recover();
    let mut maintenance = Maintenance::default();
    let permits = Arc::new(tokio::sync::Semaphore::new(inner.concurrency));
    loop {
        if *stop.borrow() {
            return;
        }
        inner.maintenance_tick(&mut maintenance);
        while let Ok(permit) = Arc::clone(&permits).try_acquire_owned() {
            let claimed = lock(&inner.state).claim(inner.clock.now_ms());
            match claimed {
                Ok(Some(job)) => {
                    lock(&inner.running).insert(job.id.clone());
                    let inner2 = Arc::clone(&inner);
                    tokio::spawn(async move {
                        run_job(Arc::clone(&inner2), job).await;
                        drop(permit);
                        inner2.wake.notify_one();
                    });
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::error!(error = %e, "claim failed");
                    break;
                }
            }
        }
        let now = inner.clock.now_ms();
        let wake_in = lock(&inner.state)
            .next_wake(now)
            .ok()
            .flatten()
            .map(|t| Duration::from_millis(u64::try_from(t - now).unwrap_or(0)))
            .unwrap_or(IDLE_POLL)
            .min(IDLE_POLL);
        tokio::select! {
            _ = stop.changed() => return,
            () = inner.wake.notified() => {}
            () = tokio::time::sleep(wake_in) => {}
        }
    }
}

/// Executes one claimed job end to end and settles it.
async fn run_job(inner: Arc<Inner>, job: cv_state::Job) {
    let id = job.id.clone();
    let config = Arc::clone(&inner.config.read().unwrap_or_else(PoisonError::into_inner));
    let parsed: Result<JobParams, _> = serde_json::from_value(job.params.clone());
    let (result, summary) = match parsed {
        Err(e) => (
            JobResult::Failed {
                error: format!("bad job params: {e}"),
                reason: None,
            },
            RunSummary::default(),
        ),
        Ok(jp) => match config.armed(&jp.action_id).cloned() {
            None => (
                JobResult::Failed {
                    error: format!(
                        "action {:?} is not armed: {}",
                        jp.action_id,
                        config
                            .disarm_reason(&jp.action_id)
                            .unwrap_or("unknown action")
                    ),
                    reason: None,
                },
                RunSummary::default(),
            ),
            Some(action) => {
                inner.publish_status(&id, JobState::Running, None, Vec::new(), None);
                let inner2 = Arc::clone(&inner);
                let job_id = id.clone();
                let lane = job.lane;
                match tokio::task::spawn_blocking(move || {
                    execute_steps(&inner2, &job_id, lane, &action, &jp, &config)
                })
                .await
                {
                    Ok(r) => r,
                    Err(e) => (
                        JobResult::Failed {
                            error: format!("step panicked: {e}"),
                            reason: None,
                        },
                        RunSummary::default(),
                    ),
                }
            }
        },
    };
    let result = inner.apply_service_policy(&job, result, summary.defer_cause);
    let settled = lock(&inner.state).settle(&id, &result, inner.clock.now_ms());
    if let Err(e) = settled {
        tracing::error!(job_id = %id, error = %e, "settle failed");
    }
    lock(&inner.running).remove(&id);
    lock(&inner.cancel_requested).remove(&id);
    let (state, message) = match &result {
        JobResult::Done => (JobState::Done, None),
        JobResult::Failed { error, .. } => (JobState::Failed, Some(error.clone())),
        JobResult::Deferred { .. } => (JobState::Deferred, None),
        JobResult::Cancelled => (JobState::Cancelled, None),
    };
    {
        let mut f = lock(&inner.finished);
        f.insert(id.clone(), summary.clone());
        while f.len() > FINISHED_CAP {
            let Some(k) = f.keys().next().cloned() else {
                break;
            };
            f.remove(&k);
        }
    }
    inner.publish_status(&id, state, message, summary.output_paths, summary.open_path);
}

struct Spawner<'a> {
    inner: &'a Inner,
    parent_job: &'a str,
    parent: &'a JobParams,
    lane: Lane,
    config: &'a ConfigSet,
}

impl ChildSpawner for Spawner<'_> {
    /// `run_action` step (pin 10): enqueue the target as a child job in the same lane.
    fn spawn_child(&self, action_id: &str, params: Map<String, Value>) -> Result<String, String> {
        let mut chain = self.parent.chain.clone();
        chain.push(self.parent.action_id.clone());
        if chain.iter().any(|a| a == action_id) {
            return Err(format!(
                "run_action cycle: {} -> {action_id}",
                chain.join(" -> ")
            ));
        }
        if chain.len() >= MAX_CHAIN_DEPTH {
            return Err(format!("run_action depth limit {MAX_CHAIN_DEPTH} reached"));
        }
        let target = self.config.armed(action_id).ok_or_else(|| {
            format!(
                "run_action target {action_id:?} is not armed: {}",
                self.config.disarm_reason(action_id).unwrap_or("unknown")
            )
        })?;
        let mut merged = params;
        apply_defaults(target, &mut merged);
        let target = Action {
            lane: match self.lane {
                Lane::User => LaneChoice::User,
                Lane::Background => LaneChoice::Background,
            },
            ..target.clone()
        };
        self.inner
            .enqueue(
                &target,
                RunRequest {
                    params: merged,
                    context: self.parent.context.clone(),
                    trigger: Trigger::Child {
                        parent: self.parent_job.to_owned(),
                    },
                    chain,
                    default_lane: self.lane,
                    dedupe_key: None,
                },
            )
            .map_err(|e| e.to_string())
    }
}

/// Fills absent params from their `ParamSpec` defaults.
pub fn apply_defaults(action: &Action, params: &mut Map<String, Value>) {
    for p in &action.params {
        if !params.contains_key(&p.name)
            && let Some(d) = &p.default
        {
            params.insert(p.name.clone(), Value::String(d.clone()));
        }
    }
}

fn execute_steps(
    inner: &Inner,
    job_id: &str,
    lane: Lane,
    action: &Action,
    jp: &JobParams,
    config: &ConfigSet,
) -> (JobResult, RunSummary) {
    let mut summary = RunSummary::default();
    let mut vars = Map::new();
    let checkpoints = match lock(&inner.state).checkpoints(job_id) {
        Ok(c) => c,
        Err(e) => {
            return (
                JobResult::Failed {
                    error: format!("reading checkpoints: {e}"),
                    reason: None,
                },
                summary,
            );
        }
    };
    let mut done: BTreeMap<usize, StepOutcome> = BTreeMap::new();
    for cp in checkpoints {
        // Fail closed: a checkpoint that does not parse means "applied, outcome unknown" — the
        // step is never re-run, so the job fails rather than guessing.
        let parsed = usize::try_from(cp.step_index)
            .ok()
            .zip(serde_json::from_str::<StepOutcome>(&cp.outcome).ok());
        let Some((i, o)) = parsed else {
            return (
                JobResult::Failed {
                    error: format!("corrupt checkpoint for step {}", cp.step_index),
                    reason: None,
                },
                summary,
            );
        };
        done.insert(i, o);
    }
    let spawner = Spawner {
        inner,
        parent_job: job_id,
        parent: jp,
        lane,
        config,
    };
    let mut seen_outputs = BTreeSet::new();
    // The run's own context: `active_path` follows a `move` of the active note (WP-8).
    let mut context = jp.context.clone();
    let mut absorb = |o: &StepOutcome, summary: &mut RunSummary, vars: &mut Map<String, Value>| {
        if o.open_path.is_some() {
            summary.open_path.clone_from(&o.open_path);
        }
        for p in &o.output_paths {
            if seen_outputs.insert(p.clone()) {
                summary.output_paths.push(p.clone());
            }
        }
        for (k, v) in &o.vars {
            vars.insert(k.clone(), v.clone());
        }
    };
    for (i, step) in action.steps.iter().enumerate() {
        if let Some(o) = done.get(&i) {
            absorb(o, &mut summary, &mut vars);
            steps::follow_move(&mut context, o);
            if o.stop {
                break;
            }
            continue;
        }
        if lock(&inner.cancel_requested).contains(job_id) {
            return (JobResult::Cancelled, summary);
        }
        let mut ctx = StepCtx {
            job_id,
            action,
            step_index: i,
            params: &jp.params,
            context: &context,
            vars: &vars,
            config,
            vault_root: &inner.vault_root,
            index_db: &inner.index_db,
            search: Some(&inner.search),
            state: &inner.state,
            writer: inner.writer.writer(),
            children: &spawner,
            written: Vec::new(),
        };
        let res = inner.executor.execute(step, &mut ctx);
        let written = std::mem::take(&mut ctx.written);
        inner.record_writes(&action.id, &written);
        match res {
            Ok(o) => {
                let text = match serde_json::to_string(&o) {
                    Ok(t) => t,
                    Err(e) => {
                        return (
                            JobResult::Failed {
                                error: format!("step {} outcome not serializable: {e}", i + 1),
                                reason: None,
                            },
                            summary,
                        );
                    }
                };
                let idx = i64::try_from(i).unwrap_or(i64::MAX);
                let put =
                    lock(&inner.state).put_checkpoint(job_id, idx, &text, inner.clock.now_ms());
                if let Err(e) = put {
                    return (
                        JobResult::Failed {
                            error: format!("committing checkpoint of step {}: {e}", i + 1),
                            reason: None,
                        },
                        summary,
                    );
                }
                absorb(&o, &mut summary, &mut vars);
                steps::follow_move(&mut context, &o);
                if o.stop {
                    break;
                }
            }
            Err(StepError::Failed { message, reason }) => {
                return (
                    JobResult::Failed {
                        error: format!("step {} ({}): {message}", i + 1, step.kind().as_str()),
                        reason,
                    },
                    summary,
                );
            }
            Err(StepError::Deferred {
                retry_after_ms,
                service,
                cause,
            }) => {
                summary.defer_cause = Some(cause);
                return (
                    JobResult::Deferred {
                        retry_after_ms,
                        service_unhealthy: service,
                    },
                    summary,
                );
            }
        }
    }
    (JobResult::Done, summary)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_daemon;
