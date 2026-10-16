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
use crate::job::{
    EnqueueOutcome, FailureKind, Job, JobResult, JobSeed, JobStatus, JobTypeSpec, Lane, NewJob,
    namespaced_key,
};
use crate::migrate;
use crate::query::{self, ListQuery, ShadowWrite, StatusCount};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Default terminal retention: 30 days.
pub const DEFAULT_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// Default settled re-seed suppression window (Crucible `terminalRetentionMs` default, 60 s).
pub const DEFAULT_RESEED_WINDOW_MS: i64 = 60_000;
/// Default lease timeout for same-token (live) leases.
pub const DEFAULT_STALE_LEASE_MS: i64 = 30 * 60_000;

/// Queue tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueConfig {
    /// Auto-seed suppression window after a key settles.
    pub reseed_window_ms: i64,
    /// Terminal-row retention for [`StateDb::prune_terminal`]; `<= 0` keeps forever.
    pub retention_ms: i64,
    /// Lease timeout for [`StateDb::recover_stale_leases`].
    pub stale_lease_ms: i64,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            reseed_window_ms: DEFAULT_RESEED_WINDOW_MS,
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

/// A write to log in `shadow_writes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewShadowWrite {
    /// Operation.
    pub op: String,
    /// Vault-relative path.
    pub path: String,
    /// Hash before.
    pub before_hash: Option<String>,
    /// Hash after.
    pub after_hash: Option<String>,
    /// Producing job.
    pub job_id: Option<String>,
    /// When.
    pub at_ms: i64,
}

static TOKEN_SEQ: AtomicU64 = AtomicU64::new(0);

/// Read-write handle on `state.db`. One per daemon process (I2's single writer).
pub struct StateDb {
    conn: Connection,
    token: String,
    config: QueueConfig,
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
        })
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

    /// Enqueue an explicit job. Dedupes against queued+running rows with the same key.
    /// Explicit (user) enqueues are never re-seed-suppressed.
    pub fn enqueue(&mut self, job: NewJob, now_ms: i64) -> Result<EnqueueOutcome> {
        self.enqueue_inner(job, now_ms, false)
    }

    /// Auto-source enqueue: the key is derived by the type's own spec, never supplied by the
    /// source, and a key settled within `reseed_window_ms` is suppressed
    /// (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/AGENTS.md:75`).
    pub fn enqueue_seed(
        &mut self,
        spec: &dyn JobTypeSpec,
        seed: JobSeed,
        now_ms: i64,
    ) -> Result<EnqueueOutcome> {
        let job = NewJob {
            job_type: spec.job_type().to_owned(),
            lane: seed.lane.unwrap_or(Lane::Background),
            dedupe_key: spec.dedupe_key(&seed.params),
            resource_keys: spec.resource_keys(&seed.params),
            service: spec.service(),
            params: seed.params,
        };
        self.enqueue_inner(job, now_ms, true)
    }

    fn enqueue_inner(
        &mut self,
        job: NewJob,
        now_ms: i64,
        suppress: bool,
    ) -> Result<EnqueueOutcome> {
        let key = namespaced_key(&job.job_type, job.dedupe_key.as_deref());
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(k) = &key {
            if let Some(existing) = query::find_pending(&tx, k)? {
                return Ok(EnqueueOutcome::Duplicate(existing));
            }
            if suppress {
                let cutoff = now_ms - self.config.reseed_window_ms;
                let settled: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM jobs WHERE dedupe_key = ?1 AND settled_at_ms IS NOT NULL AND settled_at_ms >= ?2",
                    params![k, cutoff],
                    |r| r.get(0),
                )?;
                if settled > 0 {
                    return Ok(EnqueueOutcome::Suppressed);
                }
            }
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
        tx.commit()?;
        Ok(EnqueueOutcome::Enqueued(id))
    }

    /// Exact dedupe probe: is a job with `"<job_type>::<key>"` queued or running?
    pub fn has_pending(&self, job_type: &str, key: &str) -> Result<bool> {
        match namespaced_key(job_type, Some(key)) {
            Some(k) => Ok(query::find_pending(&self.conn, &k)?.is_some()),
            None => Ok(false),
        }
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
        let Some(job) = query::get(&tx, id)? else {
            return Err(StateError::NoSuchJob(id.to_owned()));
        };
        if job.status != JobStatus::Running
            || job.claim_token.as_deref() != Some(self.token.as_str())
        {
            return Err(StateError::WrongStatus {
                id: id.to_owned(),
                status: job.status.as_str().to_owned(),
                expected: "running under this claim token",
            });
        }
        const CLEAR: &str = "claim_token = NULL, claimed_at_ms = NULL";
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
        tx.commit()?;
        Ok(())
    }

    /// Cancel a queued job. Running jobs are cancelled by their runner via `settle`.
    pub fn cancel(&mut self, id: &str, now_ms: i64) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE jobs SET status = 'cancelled', settled_at_ms = ?2, defer_until_ms = NULL WHERE id = ?1 AND status = 'queued'",
            params![id, now_ms],
        )?;
        if n == 0 {
            return match query::get(&self.conn, id)? {
                None => Err(StateError::NoSuchJob(id.to_owned())),
                Some(j) => Err(StateError::WrongStatus {
                    id: id.to_owned(),
                    status: j.status.as_str().to_owned(),
                    expected: "queued",
                }),
            };
        }
        Ok(())
    }

    /// Bulk-cancel every queued row in one statement; returns the count.
    pub fn clear_queued(&mut self, now_ms: i64) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE jobs SET status = 'cancelled', settled_at_ms = ?1, defer_until_ms = NULL,
               notes = CASE WHEN notes IS NULL OR notes = '' THEN 'cleared' ELSE notes || char(10) || 'cleared' END
             WHERE status = 'queued'",
            params![now_ms],
        )?)
    }

    /// Change a queued job's lane.
    pub fn promote(&mut self, id: &str, lane: Lane) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE jobs SET lane = ?2 WHERE id = ?1 AND status = 'queued'",
            params![id, lane.rank()],
        )? > 0)
    }

    /// Append run notes (newline-joined).
    pub fn append_notes(&mut self, id: &str, text: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE jobs SET notes = CASE WHEN notes IS NULL OR notes = '' THEN ?2 ELSE notes || char(10) || ?2 END WHERE id = ?1",
            params![id, text],
        )?;
        if n == 0 {
            return Err(StateError::NoSuchJob(id.to_owned()));
        }
        Ok(())
    }

    // ---- recovery / retention -----------------------------------------------

    /// Requeue every running job leased by `token`, regardless of age (a dead process's token).
    pub fn recover_own_leases(&mut self, token: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE jobs SET status = 'queued', claim_token = NULL, claimed_at_ms = NULL WHERE status = 'running' AND claim_token = ?1",
            params![token],
        )?)
    }

    /// Requeue every running job leased by any token other than this handle's — at daemon
    /// start, every other token is dead (single writer).
    pub fn recover_foreign_leases(&mut self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE jobs SET status = 'queued', claim_token = NULL, claimed_at_ms = NULL WHERE status = 'running' AND (claim_token IS NULL OR claim_token <> ?1)",
            params![self.token],
        )?)
    }

    /// Timeout recovery: requeue running jobs whose lease is older than `stale_lease_ms`.
    pub fn recover_stale_leases(&mut self, now_ms: i64) -> Result<usize> {
        let cutoff = now_ms - self.config.stale_lease_ms;
        Ok(self.conn.execute(
            "UPDATE jobs SET status = 'queued', claim_token = NULL, claimed_at_ms = NULL WHERE status = 'running' AND claimed_at_ms < ?1",
            params![cutoff],
        )?)
    }

    /// Like [`Self::recover_stale_leases`], but never requeues a row whose id is in `live`
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
        tx.commit()?;
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

    // ---- shadow writes ------------------------------------------------------

    /// Append a `shadow_writes` row; returns its sequence number.
    pub fn append_shadow_write(&mut self, w: &NewShadowWrite) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO shadow_writes (op, path, before_hash, after_hash, job_id, at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![w.op, w.path, w.before_hash, w.after_hash, w.job_id, w.at_ms],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Shadow writes with `seq > since_seq`, oldest first (`limit` 0 = unlimited).
    pub fn shadow_writes(&self, since_seq: i64, limit: u32) -> Result<Vec<ShadowWrite>> {
        query::shadow_writes(&self.conn, since_seq, limit)
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

    /// Shadow writes after `since_seq`.
    pub fn shadow_writes(&self, since_seq: i64, limit: u32) -> Result<Vec<ShadowWrite>> {
        query::shadow_writes(&self.conn, since_seq, limit)
    }

    /// Every stored image description among `md5s`, ordered by md5 (unknown keys skipped).
    pub fn image_descriptions_for(&self, md5s: &[String]) -> Result<Vec<ImageDescription>> {
        crate::image_descriptions::descriptions_for(&self.conn, md5s)
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
