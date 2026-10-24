//! Duplicate-resolution labels (plan `round-12-feedback-todos` WP-7, migration v14): one row per
//! user verdict on a candidate pair, with the scores at resolve time, as training data for the
//! duplicate thresholds. Paths are vault-relative.

use crate::error::Result;
use crate::store::StateDb;
use rusqlite::params;

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
