//! Duplicate-resolution labels (plan `round-12-feedback-todos` WP-7, migration v14): one row per
//! user verdict on a candidate pair, with the scores at resolve time, as training data for the
//! duplicate thresholds. Paths are vault-relative.

use crate::error::Result;
use crate::store::StateDb;
use rusqlite::{OptionalExtension, params};

/// One `dup_labels` row (without its id).
#[derive(Debug, Clone, PartialEq)]
pub struct DupLabel {
    /// The kept (or first) note.
    pub keep_path: String,
    /// The other note (trashed on a `keep` verdict).
    pub other_path: String,
    /// `keep_path`'s identity at resolve time.
    pub keep_identity: Option<String>,
    /// `other_path`'s identity at resolve time.
    pub other_identity: Option<String>,
    /// |keep ∩ other| / |keep|.
    pub containment_keep_in_other: f64,
    /// |keep ∩ other| / |other|.
    pub containment_other_in_keep: f64,
    /// Both identities present and equal.
    pub identity_match: bool,
    /// `not-duplicates` | `keep`.
    pub verdict: String,
    /// When the verdict was recorded.
    pub at_ms: i64,
}

impl StateDb {
    /// Inserts one label row; returns its id.
    pub fn insert_dup_label(&mut self, l: &DupLabel) -> Result<i64> {
        self.conn().execute(
            "INSERT INTO dup_labels (keep_path, other_path, keep_identity, other_identity,
                 containment_keep_in_other, containment_other_in_keep, identity_match, verdict, at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                l.keep_path,
                l.other_path,
                l.keep_identity,
                l.other_identity,
                l.containment_keep_in_other,
                l.containment_other_in_keep,
                l.identity_match,
                l.verdict,
                l.at_ms
            ],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    /// Records one verdict atomically (plan tn-review-2026-10-26-remediation WP-R6, D4): in one
    /// transaction, reuses the row already recorded for (`keep_path`, `other_path`, `verdict`)
    /// or inserts `l`, and with `repoint` moves every tracker row whose `note_path` is
    /// `other_path` onto `keep_path`. A retry of the same verdict therefore never adds a second
    /// label row. Returns the label id and the number of re-pointed tracker rows; raises the
    /// `tracker_items` change signal when any moved.
    pub fn record_dup_verdict(&mut self, l: &DupLabel, repoint: bool) -> Result<(i64, usize)> {
        let tx = self.conn().unchecked_transaction()?;
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM dup_labels WHERE keep_path = ?1 AND other_path = ?2
                     AND verdict = ?3 ORDER BY id LIMIT 1",
                params![l.keep_path, l.other_path, l.verdict],
                |r| r.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => id,
            None => {
                tx.execute(
                    "INSERT INTO dup_labels (keep_path, other_path, keep_identity, other_identity,
                         containment_keep_in_other, containment_other_in_keep, identity_match,
                         verdict, at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        l.keep_path,
                        l.other_path,
                        l.keep_identity,
                        l.other_identity,
                        l.containment_keep_in_other,
                        l.containment_other_in_keep,
                        l.identity_match,
                        l.verdict,
                        l.at_ms
                    ],
                )?;
                tx.last_insert_rowid()
            }
        };
        let repointed = if repoint {
            tx.execute(
                "UPDATE tracker_items SET note_path = ?2 WHERE note_path = ?1",
                params![l.other_path, l.keep_path],
            )?
        } else {
            0
        };
        tx.commit()?;
        if repointed > 0 {
            self.changed("tracker_items");
        }
        Ok((id, repointed))
    }

    /// Every label row, oldest first.
    pub fn dup_labels(&self) -> Result<Vec<DupLabel>> {
        let mut stmt = self.conn().prepare(
            "SELECT keep_path, other_path, keep_identity, other_identity,
                    containment_keep_in_other, containment_other_in_keep, identity_match, verdict, at_ms
             FROM dup_labels ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(DupLabel {
                keep_path: r.get(0)?,
                other_path: r.get(1)?,
                keep_identity: r.get(2)?,
                other_identity: r.get(3)?,
                containment_keep_in_other: r.get(4)?,
                containment_other_in_keep: r.get(5)?,
                identity_match: r.get(6)?,
                verdict: r.get(7)?,
                at_ms: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}
