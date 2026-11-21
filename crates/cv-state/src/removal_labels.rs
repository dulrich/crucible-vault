//! Removal labels (plan `intake-removals-sources-rescan` WP-4, migration v20): one row per
//! non-blank line the user labelled while resolving a `[!removed]` fold group (origin `fold`)
//! or trimming a selection (origin `selection`). Training data for the per-publication
//! recurring-line learning (WP-5) and `cv removals export`. The normalized line and its key are
//! computed by the caller (`cv_ingest::boilerplate`); this crate does not depend on `cv-ingest`.

use crate::error::Result;
use crate::mail_boilerplate::{from_db, to_db};
use crate::store::{StateDb, StateReader};
use cv_protocol::{RemovalOrigin, RemovalVerdict};
use rusqlite::types::{FromSqlError, ValueRef};
use rusqlite::{Connection, params};
use serde::Serialize;

/// One publication's labelled lines, by verdict (plan `abstract-removal-confidence-deletion`
/// WP-4 label contract): the stored `line` text, which the caller keys with
/// `cv_ingest::boilerplate::Labels::from_lines` (the masked tier key). The stored exact
/// `line_key` column is not used for lookup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublicationLabels {
    /// Distinct lines labelled `keep`, sorted.
    pub keep: Vec<String>,
    /// Distinct lines labelled `delete`, sorted.
    pub delete: Vec<String>,
}

/// One `removal_labels` row (without its id). Field names match the columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemovalLabel {
    /// Durable note id (`note_ids.id`).
    pub note_id: i64,
    /// The note's `publication` frontmatter value; blank when none.
    pub publication: String,
    /// Span kind: `cv_ingest::enrich::SpanKind`'s serialised form, blank for none. Open-ended
    /// (stored and exported kinds include ones the enrich model does not emit, e.g. `banner`).
    pub kind: String,
    /// Stored and exported as `delete` | `keep`.
    pub verdict: RemovalVerdict,
    /// Stored and exported as `fold` | `selection`.
    pub origin: RemovalOrigin,
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
                    l.verdict.as_str(),
                    l.origin.as_str(),
                    l.line,
                    to_db(l.line_key),
                    l.at_ms
                ])?;
            }
        }
        tx.commit()?;
        Ok(rows.len())
    }

    /// One publication's labelled lines (the tiers' labels): the distinct stored lines labelled
    /// `keep` and those labelled `delete`. A blank `publication` has no labels.
    pub fn publication_labels(&self, publication: &str) -> Result<PublicationLabels> {
        let mut out = PublicationLabels::default();
        if publication.trim().is_empty() {
            return Ok(out);
        }
        let mut stmt = self.conn().prepare_cached(
            "SELECT DISTINCT line, verdict FROM removal_labels WHERE publication = ?1
             ORDER BY line, verdict",
        )?;
        let rows = stmt.query_map(params![publication], |r| {
            Ok((r.get::<_, String>(0)?, verdict_at(r, 1)?))
        })?;
        for row in rows {
            let (k, v) = row?;
            match v {
                RemovalVerdict::Keep => out.keep.push(k),
                RemovalVerdict::Delete => out.delete.push(k),
            }
        }
        Ok(out)
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
            verdict: verdict_at(r, 3)?,
            origin: origin_at(r, 4)?,
            line: r.get(5)?,
            line_key: from_db(r.get(6)?),
            at_ms: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Column `i` as a verdict (the schema's CHECK admits only the two strings).
fn verdict_at(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<RemovalVerdict> {
    match r.get_ref(i)? {
        ValueRef::Text(b"delete") => Ok(RemovalVerdict::Delete),
        ValueRef::Text(b"keep") => Ok(RemovalVerdict::Keep),
        v => Err(invalid(i, v)),
    }
}

/// Column `i` as an origin (the schema's CHECK admits only the two strings).
fn origin_at(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<RemovalOrigin> {
    match r.get_ref(i)? {
        ValueRef::Text(b"fold") => Ok(RemovalOrigin::Fold),
        ValueRef::Text(b"selection") => Ok(RemovalOrigin::Selection),
        v => Err(invalid(i, v)),
    }
}

fn invalid(i: usize, v: ValueRef<'_>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(i, v.data_type(), Box::new(FromSqlError::InvalidType))
}
