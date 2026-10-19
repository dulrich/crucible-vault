//! The read-write store: the daemon's single handle on `state.db`.
//!
//! Contract ported from `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/AGENTS.md:55` rules (1)–(6)
//! and `db/SqliteJobStore.ts` / `DbJobBackend.ts`:
//! (1) the claim is atomic — here one `BEGIN IMMEDIATE` transaction that selects and leases,
//! so two claimers can never win the same row; (2) leases carry a per-process claim token,
//! and a dead token's leases are recovered regardless of age, while live-token leases recover
//! only by timeout; (3) terminal rows are pruned by retention (default 30 days); (4) dedupe
//! keys are type-namespaced `"<type>::<key>"` and matched exactly; (5) every time-dependent
//! call takes an explicit `now_ms`; (6) auto sources supply params, never keys, and a settled
//! key suppresses its own re-seed for a window.
//!
//! Dropped as Obsidian-induced: the drain-loop construction fragility (`AGENTS.md:59`), the
//! markdown queue archive and its notice (`:63`), and the file-backend `JobStore.move`
//! rename/rollback; the `NoteLockManager` is replaced by claim-time resource-key exclusion (I3).

use crate::error::{Result, StateError};
use crate::health::{self, BreakerState, ServiceFailureKind, ServiceHealth};
use crate::id::new_job_id;
use crate::image_descriptions::ImageDescription;
use crate::job::{EnqueueOutcome, FailureKind, Job, JobResult, JobStatus, NewJob, namespaced_key};
use crate::migrate;
use crate::query::{self, ListQuery, StatusCount};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Default terminal retention: 30 days.
pub const DEFAULT_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// Default lease timeout for same-token (live) leases.
pub const DEFAULT_STALE_LEASE_MS: i64 = 30 * 60_000;

/// Queue tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueConfig {
    /// Terminal-row retention for [`StateDb::prune_terminal`]; `<= 0` keeps forever.
    pub retention_ms: i64,
    /// Lease timeout for [`StateDb::recover_stale_leases_except`].
    pub stale_lease_ms: i64,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            retention_ms: DEFAULT_RETENTION_MS,
            stale_lease_ms: DEFAULT_STALE_LEASE_MS,
        }
    }
}

/// Breakdown returned by [`StateDb::requeue_service_failures`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequeueBreakdown {
    /// Service-kind failed rows found.
    pub total: i64,
    /// Found rows per job type.
    pub by_type: BTreeMap<String, i64>,
    /// Rows actually moved back to queued (0 on a dry run).
    pub requeued: i64,
}

/// A service failure the runner classified for one run, applied to the breaker in the same
/// transaction as the job's settle ([`StateDb::settle_with_breaker`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakerFailure {
    pub service: String,
    pub kind: ServiceFailureKind,
    pub reason: String,
    pub retry_after_ms: Option<i64>,
}

static TOKEN_SEQ: AtomicU64 = AtomicU64::new(0);

/// Read-write handle on `state.db`. One per daemon process (I2's single writer).
/// Called with a table name after a write that readers of that table should see (plan
/// round-5-reading-pickers-intake WP-10: `tracker_items` → view invalidation). Runs while the
/// caller holds the `StateDb`, so it must only signal (e.g. a channel send), never block.
pub type ChangeHook = Box<dyn Fn(&'static str) + Send>;

pub struct StateDb {
    conn: Connection,
    token: String,
    config: QueueConfig,
    change_hook: Option<ChangeHook>,
}

impl std::fmt::Debug for StateDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateDb")
            .field("token", &self.token)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl StateDb {
    /// Open (creating if absent) and migrate. `now_ms` seeds this handle's claim token.
    pub fn open(path: &Path, now_ms: i64, config: QueueConfig) -> Result<Self> {
        let mut conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate::migrate(&mut conn)?;
        let token = format!(
            "{}-{}-{}",
            std::process::id(),
            now_ms,
            TOKEN_SEQ.fetch_add(1, Ordering::Relaxed)
        );
        Ok(Self {
            conn,
            token,
            config,
            change_hook: None,
        })
    }

    /// Installs the daemon-internal change signal (replaces any previous hook).
    pub fn set_change_hook(&mut self, hook: ChangeHook) {
        self.change_hook = Some(hook);
    }

    /// Raises the change signal for `table`.
    pub(crate) fn changed(&self, table: &'static str) {
        if let Some(h) = &self.change_hook {
            h(table);
        }
    }

    /// This handle's claim token.
    #[must_use]
    pub fn claim_token(&self) -> &str {
        &self.token
    }

    /// Active config.
    #[must_use]
    pub fn config(&self) -> QueueConfig {
        self.config
    }

    /// Schema version on disk.
    pub fn schema_version(&self) -> Result<i64> {
        migrate::current_version(&self.conn)
    }

    // ---- enqueue / dedupe -------------------------------------------------

    /// Enqueue a job. Dedupes against queued+running rows with the same key.
    pub fn enqueue(&mut self, job: NewJob, now_ms: i64) -> Result<EnqueueOutcome> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = Self::enqueue_tx(&tx, job, now_ms)?;
        tx.commit()?;
        Ok(out)
    }

    /// A scheduled run: enqueues `job` and records `action_id`'s schedule run at `now_ms` in one
    /// transaction, so a crash leaves both rows or neither (a duplicate enqueue
    /// still records the run). On error nothing is written.
    pub fn enqueue_scheduled(
        &mut self,
        job: NewJob,
        action_id: &str,
        now_ms: i64,
    ) -> Result<EnqueueOutcome> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = Self::enqueue_tx(&tx, job, now_ms)?;
        crate::runner_state::record_schedule_run(&tx, action_id, now_ms)?;
        tx.commit()?;
        Ok(out)
    }

    fn enqueue_tx(
        tx: &rusqlite::Transaction<'_>,
        job: NewJob,
        now_ms: i64,
    ) -> Result<EnqueueOutcome> {
        let key = namespaced_key(&job.job_type, job.dedupe_key.as_deref());
        if let Some(k) = &key
            && let Some(existing) = query::find_pending(tx, k)?
        {
            return Ok(EnqueueOutcome::Duplicate(existing));
        }
        let id = new_job_id(now_ms);
        tx.execute(
            "INSERT INTO jobs (id, job_type, lane, status, dedupe_key, params, resource_keys, service, created_ms)
             VALUES (?1, ?2, ?3, 'queued', ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                job.job_type,
                job.lane.rank(),
                key,
                serde_json::to_string(&job.params)?,
                serde_json::to_string(&job.resource_keys)?,
                job.service,
                now_ms
            ],
        )?;
        Ok(EnqueueOutcome::Enqueued(id))
    }

    // ---- claim --------------------------------------------------------------

    /// Atomically claim the next job: highest lane, then oldest id, skipping deferred rows,
    /// rows whose resource keys overlap any running job's (I3), and rows whose service breaker
    /// is open (half-open admits exactly one probe). Ticks every breaker first.
    pub fn claim(&mut self, now_ms: i64) -> Result<Option<Job>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for mut h in health::load_all(&tx)? {
            let before = h.clone();
            h.tick(now_ms);
            if h != before {
                health::store(&tx, &h)?;
            }
        }
        let mut busy: HashSet<String> = HashSet::new();
        {
            let mut stmt = tx.prepare("SELECT resource_keys FROM jobs WHERE status = 'running'")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            for raw in rows {
                // Fail closed (I3): an unreadable running row's keys cannot be checked for
                // overlap, so no claim proceeds until the row is repaired.
                let keys: Vec<String> = serde_json::from_str(&raw?)?;
                busy.extend(keys);
            }
        }
        let candidates: Vec<(String, String, Option<String>)> = {
            let mut stmt = tx.prepare(
                "SELECT id, resource_keys, service FROM jobs WHERE status = 'queued' AND (defer_until_ms IS NULL OR defer_until_ms <= ?1) ORDER BY lane ASC, id ASC",
            )?;
            stmt.query_map(params![now_ms], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut chosen = None;
        for (id, raw_keys, service) in candidates {
            // Fail closed (I3): a candidate whose resource keys do not parse is never claimed;
            // it stays queued and visible to `list` (which tolerates corrupt JSON).
            let Ok(keys) = serde_json::from_str::<Vec<String>>(&raw_keys) else {
                tracing::warn!(job_id = %id, "unparseable resource_keys; not claimable");
                continue;
            };
            if keys.iter().any(|k| busy.contains(k)) {
                continue;
            }
            if let Some(svc) = &service
                && let Some(mut h) = health::load(&tx, svc)?
            {
                match h.state {
                    BreakerState::Closed => {}
                    BreakerState::Open => continue,
                    BreakerState::HalfOpen => {
                        if !h.try_acquire_probe(now_ms) {
                            continue;
                        }
                        health::store(&tx, &h)?;
                    }
                }
            }
            chosen = Some(id);
            break;
        }
        let Some(id) = chosen else {
            tx.commit()?;
            return Ok(None);
        };
        tx.execute(
            "UPDATE jobs SET status = 'running', claim_token = ?2, claimed_at_ms = ?3, defer_until_ms = NULL, attempts = attempts + 1 WHERE id = ?1 AND status = 'queued'",
            params![id, self.token, now_ms],
        )?;
        let job = query::get(&tx, &id)?;
        tx.commit()?;
        Ok(job)
    }

    /// Earliest `defer_until_ms` among deferred queued rows still in the future (the wake time).
    pub fn next_wake(&self, now_ms: i64) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT MIN(defer_until_ms) FROM jobs WHERE status = 'queued' AND defer_until_ms > ?1",
            params![now_ms],
            |r| r.get(0),
        )?)
    }

    // ---- settle -------------------------------------------------------------

    /// Settle a job this handle holds the lease on.
    ///
    /// `Deferred` moves it back to queued with `defer_until_ms = now + retry_after_ms`;
    /// `Failed` stamps `failure_kind` via [`FailureKind::classify`]. Breaker reporting stays
    /// with the runner (it knows the failure class); the store does not infer it.
    pub fn settle(&mut self, id: &str, result: &JobResult, now_ms: i64) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::settle_tx(&tx, &self.token, id, result, now_ms)?;
        tx.commit()?;
        Ok(())
    }

    /// [`Self::settle`] plus the job's service-breaker update, committed in one transaction (so
    /// a crash never leaves a settled job with a stale breaker, or the reverse). `failure` is
    /// a service failure the runner classified for this run (reported first). Then, for a job
    /// with a `service`: `Done` reports success, and a breaker that was not closed requeues
    /// service failures (their count is returned); any other result returns a held half-open
    /// probe. Unknown services are never recorded on success/release.
    pub fn settle_with_breaker(
        &mut self,
        id: &str,
        result: &JobResult,
        failure: Option<&BreakerFailure>,
        now_ms: i64,
    ) -> Result<Option<i64>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let service = Self::settle_tx(&tx, &self.token, id, result, now_ms)?;
        if let Some(f) = failure {
            let mut h =
                health::load(&tx, &f.service)?.unwrap_or_else(|| ServiceHealth::fresh(&f.service));
            h.report_failure(f.kind, &f.reason, f.retry_after_ms, now_ms);
            health::store(&tx, &h)?;
        }
        let mut requeued = None;
        if let Some(svc) = service
            && let Some(mut h) = health::load(&tx, &svc)?
        {
            if matches!(result, JobResult::Done) {
                let was_closed = h.state == BreakerState::Closed;
                h.report_success();
                health::store(&tx, &h)?;
                if !was_closed {
                    requeued = Some(Self::requeue_tx(&tx, false)?.requeued);
                }
            } else if h.state == BreakerState::HalfOpen && h.probe_in_flight() {
                h.release_probe();
                health::store(&tx, &h)?;
            }
        }
        tx.commit()?;
        Ok(requeued)
    }

    /// The settle write inside `tx`; returns the job's `service` (as claimed).
    fn settle_tx(
        tx: &rusqlite::Transaction<'_>,
        token: &str,
        id: &str,
        result: &JobResult,
        now_ms: i64,
    ) -> Result<Option<String>> {
        let Some(job) = query::get(tx, id)? else {
            return Err(StateError::NoSuchJob(id.to_owned()));
        };
        if job.status != JobStatus::Running || job.claim_token.as_deref() != Some(token) {
            return Err(StateError::WrongStatus {
                id: id.to_owned(),
                status: job.status,
                expected: "running under this claim token",
            });
        }
        // A durable cancel request wins over a deferral (the job never goes back to queued).
        let cancel_requested: bool = tx.query_row(
            "SELECT cancel_requested FROM jobs WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        let result = match result {
            JobResult::Deferred { .. } if cancel_requested => &JobResult::Cancelled,
            r => r,
        };
        const CLEAR: &str = "claim_token = NULL, claimed_at_ms = NULL, cancel_requested = 0";
        match result {
            JobResult::Done => tx.execute(
                &format!("UPDATE jobs SET status = 'done', settled_at_ms = ?2, error = NULL, failure_reason = NULL, failure_kind = NULL, {CLEAR} WHERE id = ?1"),
                params![id, now_ms],
            )?,
            JobResult::Cancelled => tx.execute(
                &format!("UPDATE jobs SET status = 'cancelled', settled_at_ms = ?2, {CLEAR} WHERE id = ?1"),
                params![id, now_ms],
            )?,
            JobResult::Failed { error, reason } => tx.execute(
                &format!("UPDATE jobs SET status = 'failed', settled_at_ms = ?2, error = ?3, failure_reason = ?4, failure_kind = ?5, {CLEAR} WHERE id = ?1"),
                params![id, now_ms, error, reason.map(|r| r.as_str()), FailureKind::classify(*reason).as_str()],
            )?,
            JobResult::Deferred { retry_after_ms, service_unhealthy } => {
                let note = service_unhealthy.as_ref().map(|s| format!("deferred: service {s} unhealthy"));
                // Audit E1: stamp the deferring service so the claim gate consults its breaker.
                tx.execute(
                    &format!("UPDATE jobs SET status = 'queued', defer_until_ms = ?2, error = ?3, service = COALESCE(?4, service), settled_at_ms = NULL, {CLEAR} WHERE id = ?1"),
                    params![id, now_ms.saturating_add((*retry_after_ms).max(0)), note, service_unhealthy],
                )?
            }
        };
        Ok(job.service)
    }

    /// Cancel a job. A queued job settles `cancelled` now; a running (claimed) job gets the
    /// durable `cancel_requested` flag, which its runner reads before each step and settles as
    /// cancelled. Anything else is [`StateError::WrongStatus`].
    pub fn cancel(&mut self, id: &str, now_ms: i64) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE jobs SET status = 'cancelled', settled_at_ms = ?2, defer_until_ms = NULL WHERE id = ?1 AND status = 'queued'",
            params![id, now_ms],
        )?;
        if n > 0 {
            return Ok(());
        }
        let n = self.conn.execute(
            "UPDATE jobs SET cancel_requested = 1 WHERE id = ?1 AND status = 'running'",
            params![id],
        )?;
        if n > 0 {
            return Ok(());
        }
        match query::get(&self.conn, id)? {
            None => Err(StateError::NoSuchJob(id.to_owned())),
            Some(j) => Err(StateError::WrongStatus {
                id: id.to_owned(),
                status: j.status,
                expected: "queued or running",
            }),
        }
    }

    /// Whether a cancel was requested for (running) job `id`.
    pub fn cancel_requested(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT cancel_requested FROM jobs WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    /// Retry one `failed` or `cancelled` job: back to `queued` with a fresh attempt budget
    /// (`attempts = 0`) and every settle/claim/diagnostic column cleared, in one immediate
    /// transaction. Never mints a second pending job for one dedupe key: when a `queued` or
    /// `running` job shares the key, nothing changes and its id is returned as
    /// [`RetryOutcome::Duplicate`]. Missing → [`StateError::NoSuchJob`]; any other status →
    /// [`StateError::WrongStatus`].
    pub fn retry_job(&mut self, id: &str, _now_ms: i64) -> Result<RetryOutcome> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(job) = query::get(&tx, id)? else {
            return Err(StateError::NoSuchJob(id.to_owned()));
        };
        if !matches!(job.status, JobStatus::Failed | JobStatus::Cancelled) {
            return Err(StateError::WrongStatus {
                id: id.to_owned(),
                status: job.status,
                expected: "failed or cancelled",
            });
        }
        if let Some(key) = &job.dedupe_key {
            let dup: Option<String> = tx
                .query_row(
                    "SELECT id FROM jobs WHERE dedupe_key = ?1 AND status IN ('queued','running') AND id != ?2 LIMIT 1",
                    params![key, id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(other) = dup {
                return Ok(RetryOutcome::Duplicate(other));
            }
        }
        tx.execute(
            "UPDATE jobs SET status = 'queued', error = NULL, failure_reason = NULL, failure_kind = NULL, settled_at_ms = NULL, defer_until_ms = NULL, claim_token = NULL, claimed_at_ms = NULL, attempts = 0 WHERE id = ?1",
            params![id],
        )?;
        tx.commit()?;
        Ok(RetryOutcome::Retried)
    }

    // ---- recovery / retention -----------------------------------------------

    /// Requeue every running job leased by any token other than this handle's — at daemon
    /// start, every other token is dead (single writer).
    pub fn recover_foreign_leases(&mut self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE jobs SET status = 'queued', claim_token = NULL, claimed_at_ms = NULL WHERE status = 'running' AND (claim_token IS NULL OR claim_token <> ?1)",
            params![self.token],
        )?)
    }

    /// Timeout recovery: requeues running jobs whose lease is older than `stale_lease_ms`, but never requeues a row whose id is in `live`
    /// (a job this process is still executing is long-running, not stale).
    pub fn recover_stale_leases_except(
        &mut self,
        now_ms: i64,
        live: &HashSet<String>,
    ) -> Result<usize> {
        let cutoff = now_ms - self.config.stale_lease_ms;
        let stale: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM jobs WHERE status = 'running' AND claimed_at_ms < ?1")?;
            stmt.query_map(params![cutoff], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut n = 0;
        for id in stale.iter().filter(|id| !live.contains(*id)) {
            n += self.conn.execute(
                "UPDATE jobs SET status = 'queued', claim_token = NULL, claimed_at_ms = NULL WHERE id = ?1 AND status = 'running' AND claimed_at_ms < ?2",
                params![id, cutoff],
            )?;
        }
        Ok(n)
    }

    /// Set the terminal-row retention used by [`Self::prune_terminal`] (`<= 0` keeps forever).
    pub fn set_retention_ms(&mut self, retention_ms: i64) {
        self.config.retention_ms = retention_ms;
    }

    /// Delete terminal rows settled strictly before `now - retention_ms` (boundary kept).
    /// `retention_ms <= 0` keeps forever.
    pub fn prune_terminal(&mut self, now_ms: i64) -> Result<usize> {
        if self.config.retention_ms <= 0 {
            return Ok(0);
        }
        let cutoff = now_ms - self.config.retention_ms;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // WP-9: a pruned job's step checkpoints go with it.
        tx.execute(
            "DELETE FROM job_step_checkpoints WHERE job_id IN (SELECT id FROM jobs WHERE status IN ('done','failed','cancelled') AND settled_at_ms < ?1)",
            params![cutoff],
        )?;
        let n = tx.execute(
            "DELETE FROM jobs WHERE status IN ('done','failed','cancelled') AND settled_at_ms < ?1",
            params![cutoff],
        )?;
        tx.commit()?;
        Ok(n)
    }

    /// Outage requeue: move `failure_kind = 'service'` failed rows back to queued in one
    /// statement, clearing their diagnostic. Port of `requeueServiceFailures`
    /// (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/failedJobRepair.ts:106-134`); selection reads
    /// the stamped column, never the error text. Rows whose dedupe key already has a pending
    /// job are left failed (deviation: avoids minting a duplicate). Requeued rows get
    /// `attempts = 0` — a fresh transient-retry budget.
    pub fn requeue_service_failures(&mut self, dry_run: bool) -> Result<RequeueBreakdown> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = Self::requeue_tx(&tx, dry_run)?;
        tx.commit()?;
        Ok(out)
    }

    fn requeue_tx(tx: &rusqlite::Transaction<'_>, dry_run: bool) -> Result<RequeueBreakdown> {
        let mut out = RequeueBreakdown::default();
        {
            let mut stmt = tx.prepare(
                "SELECT job_type, COUNT(*) FROM jobs WHERE status = 'failed' AND failure_kind = 'service' GROUP BY job_type",
            )?;
            for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
                let (t, n) = row?;
                out.total += n;
                out.by_type.insert(t, n);
            }
        }
        if !dry_run {
            let n = tx.execute(
                "UPDATE jobs SET status = 'queued', error = NULL, failure_reason = NULL, failure_kind = NULL, settled_at_ms = NULL, defer_until_ms = NULL, attempts = 0
                 WHERE status = 'failed' AND failure_kind = 'service'
                   AND (dedupe_key IS NULL OR NOT EXISTS (SELECT 1 FROM jobs p WHERE p.dedupe_key = jobs.dedupe_key AND p.status IN ('queued','running')))",
                [],
            )?;
            out.requeued = i64::try_from(n).unwrap_or(i64::MAX);
        }
        Ok(out)
    }

    // ---- service health -----------------------------------------------------

    fn with_health<T>(
        &mut self,
        service: &str,
        f: impl FnOnce(&mut ServiceHealth) -> T,
    ) -> Result<T> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut h = health::load(&tx, service)?.unwrap_or_else(|| ServiceHealth::fresh(service));
        let out = f(&mut h);
        health::store(&tx, &h)?;
        tx.commit()?;
        Ok(out)
    }

    /// Report a service failure to its breaker.
    pub fn report_service_failure(
        &mut self,
        service: &str,
        kind: ServiceFailureKind,
        reason: &str,
        retry_after_ms: Option<i64>,
        now_ms: i64,
    ) -> Result<ServiceHealth> {
        self.with_health(service, |h| {
            h.report_failure(kind, reason, retry_after_ms, now_ms);
            h.clone()
        })
    }

    /// Report a service success (closes the breaker). Unknown services are not recorded.
    pub fn report_service_success(&mut self, service: &str) -> Result<()> {
        if health::load(&self.conn, service)?.is_none() {
            return Ok(());
        }
        self.with_health(service, ServiceHealth::report_success)
    }

    /// Return a held probe token without a verdict.
    pub fn release_probe(&mut self, service: &str) -> Result<()> {
        if health::load(&self.conn, service)?.is_none() {
            return Ok(());
        }
        self.with_health(service, ServiceHealth::release_probe)
    }

    /// Breaker snapshot for one service (unknown reads as fresh/closed, unrecorded).
    pub fn service_health(&self, service: &str) -> Result<ServiceHealth> {
        Ok(health::load(&self.conn, service)?.unwrap_or_else(|| ServiceHealth::fresh(service)))
    }

    // ---- reads ----------------------------------------------------------------

    /// One job.
    pub fn job(&self, id: &str) -> Result<Option<Job>> {
        query::get(&self.conn, id)
    }

    /// List jobs.
    pub fn list(&self, q: &ListQuery) -> Result<Vec<Job>> {
        query::list(&self.conn, q)
    }

    /// Counts per (type, status).
    pub fn counts(&self) -> Result<Vec<StatusCount>> {
        query::counts(&self.conn)
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
}

/// Result of [`StateDb::retry_job`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryOutcome {
    /// The job is queued again.
    Retried,
    /// A pending job with the same dedupe key exists (its id); nothing changed.
    Duplicate(String),
}

/// Read-only client handle (plan B editor, A2 dashboard, `cv status`). Opened with
/// `SQLITE_OPEN_READ_ONLY`; it never migrates, so a not-yet-created or newer DB is an error.
pub struct StateReader {
    conn: Connection,
}

impl std::fmt::Debug for StateReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateReader").finish_non_exhaustive()
    }
}

impl StateReader {
    /// Open read-only.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let found = migrate::current_version(&conn)?;
        let supported = migrate::latest_version();
        if found > supported {
            return Err(StateError::NewerSchema { found, supported });
        }
        Ok(Self { conn })
    }

    /// One job.
    pub fn job(&self, id: &str) -> Result<Option<Job>> {
        query::get(&self.conn, id)
    }

    /// Queue list.
    pub fn list(&self, q: &ListQuery) -> Result<Vec<Job>> {
        query::list(&self.conn, q)
    }

    /// Queue status: counts per (type, status).
    pub fn counts(&self) -> Result<Vec<StatusCount>> {
        query::counts(&self.conn)
    }

    /// All breaker entries.
    pub fn service_health(&self) -> Result<Vec<ServiceHealth>> {
        health::load_all(&self.conn)
    }

    /// Every stored image description among `md5s`, ordered by md5 (unknown keys skipped).
    pub fn image_descriptions_for(&self, md5s: &[String]) -> Result<Vec<ImageDescription>> {
        crate::image_descriptions::descriptions_for(&self.conn, md5s)
    }

    /// Tracker intake rows matching `q`, newest first.
    pub fn tracker_items(
        &self,
        q: &crate::tracker::IntakeQuery,
    ) -> Result<Vec<crate::tracker::TrackerItem>> {
        crate::tracker::list(&self.conn, q)
    }

    /// Number of `tracker_items` rows.
    pub fn tracker_item_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM tracker_items", [], |r| r.get(0))?)
    }

    /// Number of `link_records` rows.
    pub fn link_record_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM link_records", [], |r| r.get(0))?)
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
impl StateReader {
    pub(crate) fn conn_for_test(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod wp11_tests {
    use crate::*;
    use serde_json::json;

    fn new_job(key: &str) -> NewJob {
        NewJob {
            job_type: "t".into(),
            lane: Lane::Background,
            dedupe_key: Some(key.into()),
            params: json!({}),
            resource_keys: vec![],
            service: None,
        }
    }

    fn enq(s: &mut StateDb, key: &str, now: i64) -> String {
        match s.enqueue(new_job(key), now).unwrap() {
            EnqueueOutcome::Enqueued(id) | EnqueueOutcome::Duplicate(id) => id,
        }
    }

    #[test]
    fn retry_job_transitions_and_refusals() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = StateDb::open(&dir.path().join("state.db"), 1, QueueConfig::default()).unwrap();
        let a = enq(&mut s, "a", 1);
        assert!(matches!(
            s.retry_job("nope", 2),
            Err(StateError::NoSuchJob(_))
        ));
        assert!(matches!(
            s.retry_job(&a, 2),
            Err(StateError::WrongStatus { .. })
        ));
        let claimed = s.claim(3).unwrap().unwrap();
        assert_eq!(claimed.id, a);
        assert!(
            matches!(s.retry_job(&a, 4), Err(StateError::WrongStatus { .. })),
            "running refused"
        );
        s.settle(
            &a,
            &JobResult::Failed {
                error: "boom".into(),
                reason: None,
            },
            5,
        )
        .unwrap();
        assert_eq!(s.retry_job(&a, 6).unwrap(), RetryOutcome::Retried);
        let j = s.job(&a).unwrap().unwrap();
        assert_eq!(j.status, JobStatus::Queued);
        assert_eq!(j.attempts, 0);
        assert!(j.error.is_none() && j.settled_at_ms.is_none() && j.claim_token.is_none());
        // Cancelled → retried; a pending sibling with the same key → Duplicate, nothing changes.
        s.cancel(&a, 7).unwrap();
        let b = enq(&mut s, "a", 8);
        assert_ne!(a, b);
        assert_eq!(s.retry_job(&a, 9).unwrap(), RetryOutcome::Duplicate(b));
        assert_eq!(s.job(&a).unwrap().unwrap().status, JobStatus::Cancelled);
    }

    fn item(kind: &str, id: &str, seen: i64, day: &str, outcome: TrackerOutcome) -> TrackerItem {
        TrackerItem {
            source_kind: kind.into(),
            source_key: "src".into(),
            item_id: id.into(),
            title: "T".into(),
            url: "u".into(),
            first_seen_ms: seen,
            day: day.into(),
            outcome,
            score: None,
            full_body: None,
            note_path: None,
            source_name: None,
            duration_s: None,
            word_count: None,
        }
    }

    #[test]
    fn list_filters_deferred_and_reader_intake() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.db");
        let mut s = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
        let a = enq(&mut s, "a", 1);
        let _b = enq(&mut s, "b", 2);
        assert_eq!(s.claim(3).unwrap().unwrap().id, a);
        s.settle(
            &a,
            &JobResult::Deferred {
                retry_after_ms: 1_000,
                service_unhealthy: None,
            },
            4,
        )
        .unwrap();
        let q = |deferred| ListQuery {
            status: Some(JobStatus::Queued),
            deferred,
            now_ms: 10,
            ..ListQuery::default()
        };
        let deferred = s.list(&q(Some(true))).unwrap();
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].id, a);
        assert_eq!(s.list(&q(Some(false))).unwrap().len(), 1);
        assert_eq!(s.list(&q(None)).unwrap().len(), 2);
        s.insert_tracker_item(&item(
            "youtube",
            "v1",
            5,
            "2026-10-15",
            TrackerOutcome::Line,
        ))
        .unwrap();
        s.insert_tracker_item(&item(
            "blog",
            "p0",
            6,
            "2026-10-14",
            TrackerOutcome::Ignored,
        ))
        .unwrap();
        s.insert_tracker_item(&item("blog", "p1", 7, "2026-10-15", TrackerOutcome::Note))
            .unwrap();
        let r = StateReader::open(&p).unwrap();
        assert_eq!(r.tracker_item_count().unwrap(), 3);
        assert_eq!(r.link_record_count().unwrap(), 0);
        let ids = |q: IntakeQuery| {
            r.tracker_items(&q)
                .unwrap()
                .into_iter()
                .map(|i| i.item_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(IntakeQuery::default()), ["p1", "p0", "v1"]);
        assert_eq!(
            ids(IntakeQuery {
                day: Some("2026-10-15".into()),
                ..IntakeQuery::default()
            }),
            ["p1", "v1"]
        );
        assert_eq!(
            ids(IntakeQuery {
                source_kind: Some("blog".into()),
                outcome: Some(TrackerOutcome::Ignored),
                ..IntakeQuery::default()
            }),
            ["p0"]
        );
        assert_eq!(
            ids(IntakeQuery {
                limit: 1,
                offset: 1,
                ..IntakeQuery::default()
            }),
            ["p0"]
        );
    }
}
