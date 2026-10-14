//! The cut-over marker (plan `cut-over-now` WP-1, migration v4): a single-row record that the
//! live vault has been handed to this daemon, plus the queue barrier `cv cutover mark` needs
//! (audit B1: no shadow-started job may be recovered or claimed by the live daemon).
//!
//! Crucible has no equivalent — Obsidian owned the vault, so there was nothing to hand over.
//! The marker is never created implicitly: only [`StateDb::set_cutover`] writes it, and only
//! `cv cutover mark` (and tests) call that.

use crate::error::Result;
use crate::job::JobStatus;
use crate::migrate;
use crate::store::{StateDb, StateReader};
use rusqlite::{Connection, OptionalExtension, params};

/// The recorded cut-over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutoverRecord {
    /// Canonical vault path the marker was set for.
    pub vault: String,
    /// When it was set (epoch ms).
    pub created_at_ms: i64,
    /// Who set it (free-form actor string, e.g. `cv cutover mark (user@host)`).
    pub created_by: String,
}

/// One job that has not reached a terminal status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnfinishedJob {
    /// Job id.
    pub id: String,
    /// Job type.
    pub job_type: String,
    /// `queued` or `running`.
    pub status: JobStatus,
    /// Deferred-until time for a queued job.
    pub defer_until_ms: Option<i64>,
}

fn read_marker(conn: &Connection) -> Result<Option<CutoverRecord>> {
    if migrate::current_version(conn)? < 4 {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT vault, created_at_ms, created_by FROM cutover_marker WHERE id = 1",
            [],
            |r| {
                Ok(CutoverRecord {
                    vault: r.get(0)?,
                    created_at_ms: r.get(1)?,
                    created_by: r.get(2)?,
                })
            },
        )
        .optional()?)
}

fn read_unfinished(conn: &Connection) -> Result<Vec<UnfinishedJob>> {
    let mut stmt = conn.prepare(
        "SELECT id, job_type, status, defer_until_ms FROM jobs
         WHERE status IN ('queued', 'running') ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        let status: String = r.get(2)?;
        Ok(UnfinishedJob {
            id: r.get(0)?,
            job_type: r.get(1)?,
            status: if status == "running" {
                JobStatus::Running
            } else {
                JobStatus::Queued
            },
            defer_until_ms: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

impl StateDb {
    /// The cut-over marker, if set.
    pub fn cutover(&self) -> Result<Option<CutoverRecord>> {
        read_marker(self.conn())
    }

    /// Set the marker. Idempotent: if one is already set, it is returned unchanged.
    pub fn set_cutover(
        &mut self,
        vault: &str,
        created_by: &str,
        now_ms: i64,
    ) -> Result<CutoverRecord> {
        let tx = self.conn().unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO cutover_marker (id, vault, created_at_ms, created_by)
             VALUES (1, ?1, ?2, ?3)",
            params![vault, now_ms, created_by],
        )?;
        let rec = read_marker(&tx)?;
        tx.commit()?;
        rec.ok_or_else(|| crate::StateError::Corrupt {
            column: "cutover_marker",
            value: "row missing after insert".to_owned(),
        })
    }

    /// Every queued (deferred included) or running job, any lease age.
    pub fn unfinished_jobs(&self) -> Result<Vec<UnfinishedJob>> {
        read_unfinished(self.conn())
    }

    /// Cancel every unfinished job in one transaction: queued rows with the [`StateDb::cancel`]
    /// statement, running rows set to `cancelled` with their lease cleared. Rows are kept.
    ///
    /// **Precondition:** no daemon holds a lease on this database — `cv cutover mark` proves it
    /// first (nothing accepts connections on the socket). Cancelling a running row under a live
    /// runner would race its `settle`.
    pub fn cancel_unfinished(&mut self, now_ms: i64) -> Result<Vec<UnfinishedJob>> {
        let jobs = read_unfinished(self.conn())?;
        let tx = self.conn().unchecked_transaction()?;
        for j in &jobs {
            if j.status == JobStatus::Queued {
                // Same statement as `StateDb::cancel`, inside this transaction.
                tx.execute(
                    "UPDATE jobs SET status = 'cancelled', settled_at_ms = ?2, defer_until_ms = NULL WHERE id = ?1 AND status = 'queued'",
                    params![j.id, now_ms],
                )?;
            } else {
                tx.execute(
                    "UPDATE jobs SET status = 'cancelled', settled_at_ms = ?2, defer_until_ms = NULL,
                       claim_token = NULL, claimed_at_ms = NULL,
                       notes = CASE WHEN notes IS NULL OR notes = '' THEN 'cancelled at cut-over' ELSE notes || char(10) || 'cancelled at cut-over' END
                     WHERE id = ?1 AND status = 'running'",
                    params![j.id, now_ms],
                )?;
            }
        }
        tx.commit()?;
        Ok(jobs)
    }
}

impl StateReader {
    /// The cut-over marker, if set (absent on a pre-v4 database).
    pub fn cutover(&self) -> Result<Option<CutoverRecord>> {
        read_marker(self.conn())
    }

    /// Every queued or running job.
    pub fn unfinished_jobs(&self) -> Result<Vec<UnfinishedJob>> {
        read_unfinished(self.conn())
    }
}
