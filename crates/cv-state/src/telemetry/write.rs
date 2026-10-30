//! Job telemetry writes inside the store's transactions (plan job-telemetry WP-1, state.db v16).
//!
//! The write side of [`super::split_buckets`]: claim inserts an attempt row (keyed by the
//! monotonic `jobs.tel_attempt`, never `attempts`, which retry and service requeue reset), settle
//! stamps its end, and a wait-cause *change* appends one `job_wait_events` row. A queued job's
//! current cause is its latest event recorded at or after its last claim, so a requeue without an
//! event (retry, recovery, service requeue) starts with no cause: that time is the unobserved
//! residual until a claim pass observes it.

use super::split::WaitCause;
use crate::error::Result;
use rusqlite::{OptionalExtension, Transaction, params};

/// Records that queued job `id` waits for `cause` (`blocker`) as of `now_ms`, unless that is
/// already its current cause. At most one row write per call.
pub(crate) fn note_wait(
    tx: &Transaction<'_>,
    id: &str,
    cause: WaitCause,
    blocker: Option<&str>,
    now_ms: i64,
) -> Result<()> {
    let current: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT e.cause, e.blocker FROM job_wait_events e JOIN jobs j ON j.id = e.job_id
             WHERE e.job_id = ?1 AND e.at_ms >= COALESCE(j.last_claimed_ms, 0)
             ORDER BY e.seq DESC LIMIT 1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((c, b)) = &current
        && c == cause.as_str()
        && b.as_deref() == blocker
    {
        return Ok(());
    }
    insert_wait(tx, id, cause, blocker, None, now_ms)
}

/// Appends a wait event unconditionally.
pub(crate) fn insert_wait(
    tx: &Transaction<'_>,
    id: &str,
    cause: WaitCause,
    blocker: Option<&str>,
    until_ms: Option<i64>,
    now_ms: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO job_wait_events (job_id, at_ms, cause, blocker, until_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, now_ms, cause.as_str(), blocker, until_ms],
    )?;
    Ok(())
}

/// The attempt row of a claim the caller just stamped (`tel_attempt` already incremented).
pub(crate) fn record_claim(tx: &Transaction<'_>, id: &str, now_ms: i64) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO job_attempt_times (job_id, attempt, claimed_ms, settled_ms)
         SELECT id, tel_attempt, ?2, NULL FROM jobs WHERE id = ?1",
        params![id, now_ms],
    )?;
    Ok(())
}

/// Stamps the settle of `id`'s current attempt (any result, a deferral included).
pub(crate) fn record_settle(tx: &Transaction<'_>, id: &str, now_ms: i64) -> Result<()> {
    tx.execute(
        "UPDATE job_attempt_times SET settled_ms = ?2
         WHERE job_id = ?1 AND attempt = (SELECT tel_attempt FROM jobs WHERE id = ?1) AND settled_ms IS NULL",
        params![id, now_ms],
    )?;
    Ok(())
}
