//! The trash ledger (plan tidiness-trash-orphans WP-1, migration v15): one `trash_entries` row
//! per file cv moved into a trash (or, WP-2, a pre-existing trash file it adopted). Every
//! mutating accessor raises the `trash_entries` change signal (audit E3), so views refresh
//! without daemon-side signal calls.
//!
//! Strings are the ledger spelling: `location` ∈ `vault`/`system`, `reason` ∈
//! `deleted`/`replaced`/`note-attachment`/`orphan`/`seeded` (the table's CHECKs refuse others).

use crate::error::Result;
use crate::store::StateDb;
use rusqlite::{OptionalExtension, Row, params};

/// One ledger row without its id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTrashEntry {
    /// `vault` | `system`.
    pub location: String,
    /// Vault-relative `.trash/…` path (vault) or the name inside `Trash/files/` (system).
    pub trash_path: String,
    /// The vault path the file had.
    pub original_path: String,
    /// When it was trashed (ms since the epoch; `seeded` rows: when first seen).
    pub trashed_ms: i64,
    /// Size at move time.
    pub bytes: i64,
    /// `deleted` | `replaced` | `note-attachment` | `orphan` | `seeded`.
    pub reason: String,
    /// The file's mtime (ms) at move time (identity, audit E1).
    pub mtime_ms: Option<i64>,
    /// The file's inode at move time (identity; rename preserves it).
    pub inode: Option<i64>,
    /// System rows: the `info/` file name (`<name>.trashinfo`).
    pub info_name: Option<String>,
}

/// One ledger row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashEntry {
    /// Row id.
    pub id: i64,
    /// The row's fields.
    pub entry: NewTrashEntry,
}

const COLS: &str = "id, location, trash_path, original_path, trashed_ms, bytes, reason, mtime_ms, inode, info_name";

fn row(r: &Row<'_>) -> rusqlite::Result<TrashEntry> {
    Ok(TrashEntry {
        id: r.get(0)?,
        entry: NewTrashEntry {
            location: r.get(1)?,
            trash_path: r.get(2)?,
            original_path: r.get(3)?,
            trashed_ms: r.get(4)?,
            bytes: r.get(5)?,
            reason: r.get(6)?,
            mtime_ms: r.get(7)?,
            inode: r.get(8)?,
            info_name: r.get(9)?,
        },
    })
}

impl StateDb {
    /// Inserts one ledger row; returns its id. Signals `trash_entries`.
    pub fn insert_trash_entry(&mut self, e: &NewTrashEntry) -> Result<i64> {
        self.conn().execute(
            "INSERT INTO trash_entries (location, trash_path, original_path, trashed_ms, bytes,
                 reason, mtime_ms, inode, info_name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                e.location,
                e.trash_path,
                e.original_path,
                e.trashed_ms,
                e.bytes,
                e.reason,
                e.mtime_ms,
                e.inode,
                e.info_name
            ],
        )?;
        let id = self.conn().last_insert_rowid();
        self.changed("trash_entries");
        Ok(id)
    }

    /// Every ledger row, oldest first (`trashed_ms`, then id).
    pub fn trash_entries(&self) -> Result<Vec<TrashEntry>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {COLS} FROM trash_entries ORDER BY trashed_ms, id"
        ))?;
        let rows = stmt.query_map([], row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The newest row for `location` + `trash_path` (seed/adopt de-duplication), if any.
    pub fn trash_entry_by_path(
        &self,
        location: &str,
        trash_path: &str,
    ) -> Result<Option<TrashEntry>> {
        Ok(self
            .conn()
            .query_row(
                &format!(
                    "SELECT {COLS} FROM trash_entries WHERE location = ?1 AND trash_path = ?2
                     ORDER BY id DESC LIMIT 1"
                ),
                params![location, trash_path],
                row,
            )
            .optional()?)
    }

    /// Deletes row `id`; `true` when it existed. Signals `trash_entries` only then.
    pub fn delete_trash_entry(&mut self, id: i64) -> Result<bool> {
        let n = self
            .conn()
            .execute("DELETE FROM trash_entries WHERE id = ?1", params![id])?;
        if n > 0 {
            self.changed("trash_entries");
        }
        Ok(n > 0)
    }
}
