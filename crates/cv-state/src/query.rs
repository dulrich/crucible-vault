//! Read-side queries shared by [`crate::StateDb`] and [`crate::StateReader`].

use crate::error::Result;
use crate::job::{FailureKind, FailureReason, Job, JobStatus, Lane};
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
        lane: Lane::from_rank(r.lane)?,
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
            .map(FailureKind::parse)
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
}

pub(crate) fn list(conn: &Connection, q: &ListQuery) -> Result<Vec<Job>> {
    let order = if q.recent_first {
        "COALESCE(settled_at_ms, created_ms) DESC, id DESC"
    } else {
        "lane ASC, id ASC"
    };
    let limit: i64 = if q.limit == 0 { -1 } else { i64::from(q.limit) };
    let sql = format!(
        "SELECT {JOB_COLS} FROM jobs WHERE (?1 IS NULL OR status = ?1) AND (?2 IS NULL OR job_type = ?2) ORDER BY {order} LIMIT ?3 OFFSET ?4"
    );
    let mut stmt = conn.prepare(&sql)?;
    let raws = stmt
        .query_map(
            params![
                q.status.map(JobStatus::as_str),
                q.job_type,
                limit,
                i64::from(q.offset)
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

/// One logged would-be (shadow-mode) or real write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowWrite {
    /// Insertion sequence.
    pub seq: i64,
    /// Operation (e.g. `create`, `modify`, `rename`, `delete`).
    pub op: String,
    /// Vault-relative path.
    pub path: String,
    /// Content hash before (absent for create).
    pub before_hash: Option<String>,
    /// Content hash after (absent for delete).
    pub after_hash: Option<String>,
    /// Job that produced it.
    pub job_id: Option<String>,
    /// When.
    pub at_ms: i64,
}

pub(crate) fn shadow_writes(
    conn: &Connection,
    since_seq: i64,
    limit: u32,
) -> Result<Vec<ShadowWrite>> {
    let limit: i64 = if limit == 0 { -1 } else { i64::from(limit) };
    let mut stmt = conn.prepare(
        "SELECT seq, op, path, before_hash, after_hash, job_id, at_ms FROM shadow_writes WHERE seq > ?1 ORDER BY seq LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![since_seq, limit], |r| {
            Ok(ShadowWrite {
                seq: r.get(0)?,
                op: r.get(1)?,
                path: r.get(2)?,
                before_hash: r.get(3)?,
                after_hash: r.get(4)?,
                job_id: r.get(5)?,
                at_ms: r.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
