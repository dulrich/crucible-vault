//! The cut-over marker (plan `cut-over-now` WP-1, migration v4): a single-row record that the
//! live vault has been handed to this daemon.
//!
//! Crucible has no equivalent — Obsidian owned the vault, so there was nothing to hand over.
//! The marker is never created implicitly: only [`StateDb::set_cutover`] writes it (the
//! `cv cutover mark` CLI that called it was retired in plan tn-review-baseline-remediation WP-R5).

use crate::error::Result;
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
}

impl StateReader {
    /// The cut-over marker, if set (absent on a pre-v4 database).
    pub fn cutover(&self) -> Result<Option<CutoverRecord>> {
        read_marker(self.conn())
    }
}
