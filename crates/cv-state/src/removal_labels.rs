//! Removal labels (plan `intake-removals-sources-rescan` WP-4, migration v20): one row per
//! non-blank line the user labelled while resolving a `[!removed]` fold group (origin `fold`)
//! or trimming a selection (origin `selection`). Training data for the per-publication
//! recurring-line learning (WP-5) and `cv removals export`. The normalized line and its key are
//! computed by the caller (`cv_ingest::boilerplate`); this crate does not depend on `cv-ingest`.

use crate::error::Result;
use crate::mail_boilerplate::{from_db, to_db};
use crate::store::{StateDb, StateReader};
use rusqlite::{Connection, params};
use serde::Serialize;

/// One `removal_labels` row (without its id). Field names match the columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemovalLabel {
    /// Durable note id (`note_ids.id`).
    pub note_id: i64,
    /// The note's `publication` frontmatter value; blank when none.
    pub publication: String,
    /// Span kind (`cv_ingest::enrich::SpanKind::as_str`); blank when unknown.
    pub kind: String,
    /// `delete` | `keep`.
    pub verdict: String,
    /// `fold` | `selection`.
    pub origin: String,
    /// The normalized line.
    pub line: String,
    /// `cv_ingest::boilerplate::line_key` of `line` (stored bit-cast to INTEGER, as
    /// `mail_boilerplate.line_key`).
    pub line_key: u64,
    /// When the label was recorded.
    pub at_ms: i64,
}

impl StateDb {
    /// Inserts every row in one transaction; returns how many were written.
    pub fn insert_removal_labels(&mut self, rows: &[RemovalLabel]) -> Result<usize> {
        let tx = self.conn().unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO removal_labels (note_id, publication, kind, verdict, origin, line,
                     line_key, at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for l in rows {
                stmt.execute(params![
                    l.note_id,
                    l.publication,
                    l.kind,
                    l.verdict,
                    l.origin,
                    l.line,
                    to_db(l.line_key),
                    l.at_ms
                ])?;
            }
        }
        tx.commit()?;
        Ok(rows.len())
    }

    /// One publication's labelled line keys (plan WP-5's label tiers): `(keep, delete, unknown)`
    /// — the distinct keys labelled `keep`, those labelled `delete`, and how many rows carry any
    /// other verdict (ignored, never read as delete). A blank `publication` has no labels.
    pub fn publication_labels(&self, publication: &str) -> Result<(Vec<u64>, Vec<u64>, usize)> {
        let (mut keep, mut delete, mut unknown) = (Vec::new(), Vec::new(), 0);
        if publication.trim().is_empty() {
            return Ok((keep, delete, unknown));
        }
        let mut stmt = self.conn().prepare_cached(
            "SELECT DISTINCT line_key, verdict FROM removal_labels WHERE publication = ?1",
        )?;
        let rows = stmt.query_map(params![publication], |r| {
            Ok((from_db(r.get(0)?), r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (k, v) = row?;
            match v.as_str() {
                "keep" => keep.push(k),
                "delete" => delete.push(k),
                _ => unknown += 1,
            }
        }
        Ok((keep, delete, unknown))
    }

    /// Every removal label, oldest first.
    pub fn removal_labels(&self) -> Result<Vec<RemovalLabel>> {
        read_all(self.conn())
    }
}

impl StateReader {
    /// Every removal label, oldest first (read-only; `cv removals export`).
    pub fn removal_labels(&self) -> Result<Vec<RemovalLabel>> {
        read_all(&self.conn)
    }
}

fn read_all(conn: &Connection) -> Result<Vec<RemovalLabel>> {
    let mut stmt = conn.prepare(
        "SELECT note_id, publication, kind, verdict, origin, line, line_key, at_ms
         FROM removal_labels ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(RemovalLabel {
            note_id: r.get(0)?,
            publication: r.get(1)?,
            kind: r.get(2)?,
            verdict: r.get(3)?,
            origin: r.get(4)?,
            line: r.get(5)?,
            line_key: from_db(r.get(6)?),
            at_ms: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
