//! Read-side queries shared by [`crate::StateDb`] and [`crate::StateReader`].

use crate::error::Result;
use crate::job::{FailureReason, Job, JobStatus, lane_from_rank, parse_failure_kind};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub(crate) const JOB_COLS: &str = "id, job_type, lane, status, dedupe_key, params, resource_keys, service, created_ms, claim_token, claimed_at_ms, defer_until_ms, settled_at_ms, attempts, error, failure_reason, failure_kind, notes";

/// Untyped row; converted (and validated) by [`raw_to_job`].
pub(crate) struct RawJob {
    id: String,
    job_type: String,
    lane: i64,
    status: String,
    dedupe_key: Option<String>,
    params: String,
    resource_keys: String,
    service: Option<String>,
    created_ms: i64,
    claim_token: Option<String>,
    claimed_at_ms: Option<i64>,
    defer_until_ms: Option<i64>,
    settled_at_ms: Option<i64>,
    attempts: i64,
    error: Option<String>,
    failure_reason: Option<String>,
    failure_kind: Option<String>,
    notes: Option<String>,
}

pub(crate) fn raw_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawJob> {
    Ok(RawJob {
        id: r.get(0)?,
        job_type: r.get(1)?,
        lane: r.get(2)?,
        status: r.get(3)?,
        dedupe_key: r.get(4)?,
        params: r.get(5)?,
        resource_keys: r.get(6)?,
        service: r.get(7)?,
        created_ms: r.get(8)?,
        claim_token: r.get(9)?,
        claimed_at_ms: r.get(10)?,
        defer_until_ms: r.get(11)?,
        settled_at_ms: r.get(12)?,
        attempts: r.get(13)?,
        error: r.get(14)?,
        failure_reason: r.get(15)?,
        failure_kind: r.get(16)?,
        notes: r.get(17)?,
    })
}

/// Corrupt JSON in `params`/`resource_keys` reads as a safe default rather than failing the
/// whole list (Crucible vector: "corrupted params/output_paths JSON is tolerated").
pub(crate) fn raw_to_job(r: RawJob) -> Result<Job> {
    let params = serde_json::from_str(&r.params).unwrap_or(serde_json::Value::Null);
    let resource_keys = serde_json::from_str(&r.resource_keys).unwrap_or_default();
    Ok(Job {
        id: r.id,
        job_type: r.job_type,
        lane: lane_from_rank(r.lane)?,
        status: JobStatus::parse(&r.status)?,
        dedupe_key: r.dedupe_key,
        params,
        resource_keys,
        service: r.service,
        created_ms: r.created_ms,
        claim_token: r.claim_token,
        claimed_at_ms: r.claimed_at_ms,
        defer_until_ms: r.defer_until_ms,
        settled_at_ms: r.settled_at_ms,
        attempts: r.attempts,
        error: r.error,
        failure_reason: r
            .failure_reason
            .as_deref()
            .map(FailureReason::parse)
            .transpose()?,
        failure_kind: r
            .failure_kind
            .as_deref()
            .map(parse_failure_kind)
            .transpose()?,
        notes: r.notes,
    })
}

pub(crate) fn get(conn: &Connection, id: &str) -> Result<Option<Job>> {
    let sql = format!("SELECT {JOB_COLS} FROM jobs WHERE id = ?1");
    conn.query_row(&sql, params![id], raw_row)
        .optional()?
        .map(raw_to_job)
        .transpose()
}

/// Queue listing filter for clients.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListQuery {
    /// Restrict to one status.
    pub status: Option<JobStatus>,
    /// Restrict to one job type.
    pub job_type: Option<String>,
    /// Max rows (0 = unlimited).
    pub limit: u32,
    /// Rows to skip.
    pub offset: u32,
    /// Newest-settled first instead of claim order (useful for terminal buckets).
    pub recent_first: bool,
    /// Deferral filter, evaluated against `now_ms`: `Some(true)` keeps only rows with
    /// `defer_until_ms > now_ms`, `Some(false)` only rows without a future deferral.
    pub deferred: Option<bool>,
    /// Clock for the `deferred` filter.
    pub now_ms: i64,
}

pub(crate) fn list(conn: &Connection, q: &ListQuery) -> Result<Vec<Job>> {
    let order = if q.recent_first {
        "COALESCE(settled_at_ms, created_ms) DESC, id DESC"
    } else {
        "lane ASC, id ASC"
    };
    let limit: i64 = if q.limit == 0 { -1 } else { i64::from(q.limit) };
    let sql = format!(
        "SELECT {JOB_COLS} FROM jobs WHERE (?1 IS NULL OR status = ?1) AND (?2 IS NULL OR job_type = ?2) \
         AND (?5 IS NULL OR (CASE WHEN defer_until_ms IS NOT NULL AND defer_until_ms > ?6 THEN 1 ELSE 0 END) = ?5) \
         ORDER BY {order} LIMIT ?3 OFFSET ?4"
    );
    let mut stmt = conn.prepare(&sql)?;
    let raws = stmt
        .query_map(
            params![
                q.status.map(JobStatus::as_str),
                q.job_type,
                limit,
                i64::from(q.offset),
                q.deferred.map(i64::from),
                q.now_ms
            ],
            raw_row,
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    raws.into_iter().map(raw_to_job).collect()
}

/// Per-(type, status) row count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusCount {
    /// Job type.
    pub job_type: String,
    /// Status.
    pub status: JobStatus,
    /// Count.
    pub count: i64,
}

pub(crate) fn counts(conn: &Connection) -> Result<Vec<StatusCount>> {
    let mut stmt =
        conn.prepare("SELECT job_type, status, COUNT(*) FROM jobs GROUP BY job_type, status ORDER BY job_type, status")?;
    let raws = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    raws.into_iter()
        .map(|(job_type, s, count)| {
            Ok(StatusCount {
                job_type,
                status: JobStatus::parse(&s)?,
                count,
            })
        })
        .collect()
}

/// Exact match of the namespaced key against queued+running rows; never a prefix/LIKE.
pub(crate) fn find_pending(conn: &Connection, namespaced: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT id FROM jobs WHERE dedupe_key = ?1 AND status IN ('queued','running') ORDER BY id LIMIT 1",
            params![namespaced],
            |r| r.get(0),
        )
        .optional()?)
}

// ---- job telemetry (plan job-telemetry WP-1, state.db v16) ------------------------------

/// Why a queued job was not running, as recorded by the claim pass / claim loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitCause {
    /// Waiting out a `defer_until_ms` (deferred settle); the blocker is the deferring service.
    Deferred,
    /// Claimable, but every runner permit was taken; the blocker names the running actions.
    Permit,
    /// A resource key was held by a running job; the blocker is that job's action.
    Key,
    /// The job's service breaker was open (or half-open with its probe taken).
    Breaker,
}

impl WaitCause {
    /// Stored form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deferred => "deferred",
            Self::Permit => "permit",
            Self::Key => "key",
            Self::Breaker => "breaker",
        }
    }

    /// Parse the stored form.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "deferred" => Some(Self::Deferred),
            "permit" => Some(Self::Permit),
            "key" => Some(Self::Key),
            "breaker" => Some(Self::Breaker),
            _ => None,
        }
    }
}

/// One claim of a job (telemetry attempt `attempt`, monotonic per job, never reset).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptTime {
    pub attempt: i64,
    pub claimed_ms: i64,
    /// `None` when the attempt never settled normally (recovered lease; the row is partial).
    pub settled_ms: Option<i64>,
}

/// A wait-cause change: from `at_ms` the job waited for `cause` until the next event, the next
/// claim, or settle (a deferral also ends at `until_ms`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitEvent {
    pub at_ms: i64,
    pub cause: WaitCause,
    pub blocker: Option<String>,
    pub until_ms: Option<i64>,
}

/// One executed step of one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepTime {
    pub attempt: i64,
    pub step_index: i64,
    pub step_kind: String,
    pub start_ms: i64,
    pub end_ms: Option<i64>,
}

/// Every stored telemetry row of one job (input of [`split_buckets`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobTelemetry {
    pub id: String,
    pub job_type: String,
    pub status: JobStatus,
    pub created_ms: i64,
    pub settled_ms: Option<i64>,
    /// Partial-coverage marker (Decision 6): created/claimed before v16, recovered without a
    /// normal settle, or cancelled while queued.
    pub partial: bool,
    /// In claim order.
    pub attempts: Vec<AttemptTime>,
    /// In record order.
    pub waits: Vec<WaitEvent>,
    /// By attempt, then step index.
    pub steps: Vec<StepTime>,
}

/// Run time of one attempt, split per executed step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRun {
    pub attempt: i64,
    pub run_ms: i64,
    /// `(step_index, step_kind, duration_ms)`; a step with no end stamp is omitted.
    pub steps: Vec<(i64, String, i64)>,
}

/// `settled − created` of one job, split so every millisecond lands in exactly one bucket:
/// `deferral + permit + key + breaker + run + residual == total`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketSplit {
    pub total_ms: i64,
    pub deferral_ms: i64,
    pub permit_ms: i64,
    pub key_ms: i64,
    pub breaker_ms: i64,
    pub run_ms: i64,
    /// Non-executing / unobserved residual: time terminal between a failure and its retry,
    /// and queued time no claim pass observed with a cause. Never folded into a measured bucket.
    pub residual_ms: i64,
    /// Measured wait per `(cause, blocker)` (blocker `""` when none was named).
    pub blockers: Vec<(WaitCause, String, i64)>,
    pub attempts: Vec<AttemptRun>,
}

/// Split a settled, fully instrumented job's `settled − created` into wait buckets (deferral,
/// permit, key, breaker, each with its blockers), run time (per attempt and per step) and the
/// labelled non-executing / unobserved residual (`total − measured`). Pure over the stored rows
/// ([`crate::StateDb::job_telemetry`] loads them).
///
/// Returns `None` for a job that has not settled or carries the partial-coverage marker: its
/// intervals are unavailable, never reported as a fabricated zero or cause.
///
/// A wait event spans from its `at_ms` to the earliest of the next event, the next claim at or
/// after it, and settle; a deferral additionally ends at its `until_ms` (the time after it until
/// the claim is unobserved). An attempt runs from its claim to its settle.
#[must_use]
pub fn split_buckets(t: &JobTelemetry) -> Option<BucketSplit> {
    let settled = t.settled_ms?;
    if t.partial {
        return None;
    }
    let clamp = |v: i64| v.clamp(t.created_ms, settled);
    let mut out = BucketSplit {
        total_ms: settled - t.created_ms,
        ..BucketSplit::default()
    };
    let mut claims: Vec<i64> = t.attempts.iter().map(|a| a.claimed_ms).collect();
    claims.sort_unstable();
    for (i, a) in t.attempts.iter().enumerate() {
        let next_claim = t.attempts.get(i + 1).map_or(settled, |n| n.claimed_ms);
        let end = clamp(a.settled_ms.unwrap_or(next_claim));
        let run = (end - clamp(a.claimed_ms)).max(0);
        out.run_ms += run;
        let steps = t
            .steps
            .iter()
            .filter(|s| s.attempt == a.attempt)
            .filter_map(|s| {
                s.end_ms
                    .map(|e| (s.step_index, s.step_kind.clone(), (e - s.start_ms).max(0)))
            })
            .collect();
        out.attempts.push(AttemptRun {
            attempt: a.attempt,
            run_ms: run,
            steps,
        });
    }
    let mut blockers: std::collections::BTreeMap<(WaitCause, String), i64> =
        std::collections::BTreeMap::new();
    for (i, w) in t.waits.iter().enumerate() {
        let next_event = t.waits.get(i + 1).map_or(settled, |n| n.at_ms);
        let next_claim = claims
            .iter()
            .copied()
            .find(|c| *c >= w.at_ms)
            .unwrap_or(settled);
        let mut end = next_event.min(next_claim);
        if w.cause == WaitCause::Deferred
            && let Some(u) = w.until_ms
        {
            end = end.min(u);
        }
        let ms = (clamp(end) - clamp(w.at_ms)).max(0);
        match w.cause {
            WaitCause::Deferred => out.deferral_ms += ms,
            WaitCause::Permit => out.permit_ms += ms,
            WaitCause::Key => out.key_ms += ms,
            WaitCause::Breaker => out.breaker_ms += ms,
        }
        *blockers
            .entry((w.cause, w.blocker.clone().unwrap_or_default()))
            .or_insert(0) += ms;
    }
    out.blockers = blockers
        .into_iter()
        .map(|((c, b), ms)| (c, b, ms))
        .collect();
    out.residual_ms =
        out.total_ms - out.deferral_ms - out.permit_ms - out.key_ms - out.breaker_ms - out.run_ms;
    Some(out)
}

/// Every telemetry row of job `id`; `None` when the job does not exist.
pub(crate) fn telemetry(conn: &Connection, id: &str) -> Result<Option<JobTelemetry>> {
    let Some(job) = get(conn, id)? else {
        return Ok(None);
    };
    let partial: bool = conn.query_row(
        "SELECT tel_partial FROM jobs WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )?;
    let attempts = conn
        .prepare("SELECT attempt, claimed_ms, settled_ms FROM job_attempt_times WHERE job_id = ?1 ORDER BY attempt")?
        .query_map(params![id], |r| {
            Ok(AttemptTime {
                attempt: r.get(0)?,
                claimed_ms: r.get(1)?,
                settled_ms: r.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let raw_waits = conn
        .prepare("SELECT at_ms, cause, blocker, until_ms FROM job_wait_events WHERE job_id = ?1 ORDER BY seq")?
        .query_map(params![id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let waits = raw_waits
        .into_iter()
        .filter_map(|(at_ms, cause, blocker, until_ms)| {
            WaitCause::parse(&cause).map(|cause| WaitEvent {
                at_ms,
                cause,
                blocker,
                until_ms,
            })
        })
        .collect();
    let steps = conn
        .prepare("SELECT attempt, step_index, step_kind, start_ms, end_ms FROM job_step_times WHERE job_id = ?1 ORDER BY attempt, step_index")?
        .query_map(params![id], |r| {
            Ok(StepTime {
                attempt: r.get(0)?,
                step_index: r.get(1)?,
                step_kind: r.get(2)?,
                start_ms: r.get(3)?,
                end_ms: r.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Some(JobTelemetry {
        id: job.id,
        job_type: job.job_type,
        status: job.status,
        created_ms: job.created_ms,
        settled_ms: job.settled_at_ms,
        partial,
        attempts,
        waits,
        steps,
    }))
}
