//! The trash ledger (plan tidiness-trash-orphans WP-1, migration v15): one `trash_entries` row
//! per file cv moved into a trash (or, WP-2, a pre-existing trash file it adopted). Every
//! mutating accessor raises the `trash_entries` change signal (audit E3), so views refresh
//! without daemon-side signal calls.
//!
//! `location`/`reason` are the protocol enums ([`TrashLocation`], [`TrashReason`]); this module
//! owns their SQL text form (`as_str` in, `parse` out). The table's CHECKs still refuse any
//! other spelling (schema v15 unchanged; plan tn-review-2026-10-26-remediation WP-R6).

use crate::error::Result;
use crate::store::StateDb;
use cv_protocol::{TrashLocation, TrashReason};
use rusqlite::types::Type;
use rusqlite::{OptionalExtension, Row, params};
use std::path::{Component, Path};

/// One ledger row without its id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTrashEntry {
    /// Which trash.
    pub location: TrashLocation,
    /// Vault-relative `.trash/…` path (vault) or the name inside `Trash/files/` (system).
    pub trash_path: String,
    /// The vault path the file had.
    pub original_path: String,
    /// When it was trashed (ms since the epoch; `seeded` rows: when first seen).
    pub trashed_ms: i64,
    /// Size at move time.
    pub bytes: i64,
    /// Why it was trashed.
    pub reason: TrashReason,
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

/// Column `i` parsed by `parse` (an unknown spelling is a conversion error, never coerced).
fn text<T>(r: &Row<'_>, i: usize, parse: fn(&str) -> Option<T>) -> rusqlite::Result<T> {
    let s: String = r.get(i)?;
    parse(&s).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            i,
            Type::Text,
            format!("unknown ledger spelling {s:?}").into(),
        )
    })
}

fn row(r: &Row<'_>) -> rusqlite::Result<TrashEntry> {
    Ok(TrashEntry {
        id: r.get(0)?,
        entry: NewTrashEntry {
            location: text(r, 1, TrashLocation::parse)?,
            trash_path: r.get(2)?,
            original_path: r.get(3)?,
            trashed_ms: r.get(4)?,
            bytes: r.get(5)?,
            reason: text(r, 6, TrashReason::parse)?,
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
                e.location.as_str(),
                e.trash_path,
                e.original_path,
                e.trashed_ms,
                e.bytes,
                e.reason.as_str(),
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
        location: TrashLocation,
        trash_path: &str,
    ) -> Result<Option<TrashEntry>> {
        Ok(self
            .conn()
            .query_row(
                &format!(
                    "SELECT {COLS} FROM trash_entries WHERE location = ?1 AND trash_path = ?2
                     ORDER BY id DESC LIMIT 1"
                ),
                params![location.as_str(), trash_path],
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

    /// Deletes every row whose trashed file is verifiably absent (plan round-17-todos-ui-sweep
    /// WP-1): `vault` rows resolve `trash_path` under `vault_root`, `system` rows under
    /// `system_root/files/` (`None`: system rows are kept). Only `NotFound` counts as absent -- any
    /// other I/O error, or a `trash_path` that is not a plain relative path, keeps the row.
    /// Returns the number deleted; signals `trash_entries` once when any row went.
    pub fn prune_missing_trash(
        &mut self,
        vault_root: &Path,
        system_root: Option<&Path>,
    ) -> Result<usize> {
        let mut gone = Vec::new();
        for r in self.trash_entries()? {
            let e = &r.entry;
            let Some(file) = (match e.location {
                TrashLocation::Vault => {
                    plain_relative(&e.trash_path).then(|| vault_root.join(&e.trash_path))
                }
                TrashLocation::System => system_root
                    .filter(|_| plain_relative(&e.trash_path))
                    .map(|root| root.join("files").join(&e.trash_path)),
            }) else {
                continue;
            };
            if matches!(std::fs::symlink_metadata(&file), Err(err) if err.kind() == std::io::ErrorKind::NotFound)
            {
                gone.push(r.id);
            }
        }
        for id in &gone {
            self.conn()
                .execute("DELETE FROM trash_entries WHERE id = ?1", params![id])?;
        }
        if !gone.is_empty() {
            self.changed("trash_entries");
        }
        Ok(gone.len())
    }
}

/// A non-empty relative path of plain components only (no `..`, root or prefix).
fn plain_relative(s: &str) -> bool {
    !s.is_empty()
        && !s.contains('\0')
        && Path::new(s)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}
