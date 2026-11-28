//! Stage outcomes and learned publication hosts (plan `intake-foundations` WP-1, schema v22).
//!
//! `stage_outcomes`: one row per (note id, stage) for what the note itself does not show — a
//! skip (with the fingerprint of the facts it was judged on), a failure (with its attempt count),
//! or an unfinished run (an Ok that left work, with its attempt count; plan `intake-stage-jobs`
//! WP-2, D40). The stage and outcome names are `cv_ingest::stage`'s strings, stored as written; this
//! crate does not depend on `cv-ingest`. Rows go when the note id is retired
//! (`note_ids::retire_row`, same transaction); a reset is [`StateDb::clear_stage_outcomes`]. The
//! `outcome` column is text, so `unfinished` needed no schema change. The stage steps write them
//! (the `cv` binary's `steps::stage_record`, plan `intake-stage-jobs` WP-3).
//!
//! `learned_hosts`: a publication host proved by a tracker observation, owned by the first
//! publication that proved it (the host is the key). Minimal accessors; plan `intake-foundations`
//! WP-3 extends them.
//!
//! The read-only [`StateReader`] accessors treat a database without these tables (one not yet
//! migrated to v22) as holding no rows.

use crate::error::Result;
use crate::store::{StateDb, StateReader};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

/// One `stage_outcomes` row. Field names match the columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StageOutcomeRow {
    /// Durable note id (`note_ids.id`).
    pub note_id: i64,
    /// `cv_ingest::stage::Stage::as_str`.
    pub stage: String,
    /// `cv_ingest::stage::OutcomeKind::as_str` (`skipped` | `failed` | `unfinished`).
    pub outcome: String,
    /// Why (blank when none).
    pub reason: String,
    /// Attempts so far.
    pub attempts: u32,
    /// `cv_ingest::stage::fingerprint` of the facts judged on.
    pub fingerprint: String,
    /// When the row was first written.
    pub first_ms: i64,
    /// When it was last written.
    pub updated_ms: i64,
}

/// One `learned_hosts` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LearnedHost {
    /// The host, in canonical form (the caller canonicalizes).
    pub host: String,
    /// The publication that first proved it.
    pub publication: String,
    /// When it was first proved.
    pub first_ms: i64,
    /// The kind of evidence (e.g. `rss-link`, `mail-url`).
    pub evidence: String,
}

const COLS: &str = "note_id, stage, outcome, reason, attempts, fingerprint, first_ms, updated_ms";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<StageOutcomeRow> {
    Ok(StageOutcomeRow {
        note_id: r.get(0)?,
        stage: r.get(1)?,
        outcome: r.get(2)?,
        reason: r.get(3)?,
        attempts: u32::try_from(r.get::<_, i64>(4)?).unwrap_or(0),
        fingerprint: r.get(5)?,
        first_ms: r.get(6)?,
        updated_ms: r.get(7)?,
    })
}

fn has_table(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn outcomes_for(conn: &Connection, ids: &[i64]) -> Result<Vec<StageOutcomeRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM stage_outcomes WHERE note_id = ?1 ORDER BY stage"
    ))?;
    let mut out = Vec::new();
    for id in ids {
        for r in stmt.query_map([id], row)? {
            out.push(r?);
        }
    }
    Ok(out)
}

fn learned(conn: &Connection) -> Result<Vec<LearnedHost>> {
    let mut stmt = conn
        .prepare("SELECT host, publication, first_ms, evidence FROM learned_hosts ORDER BY host")?;
    let rows = stmt.query_map([], |r| {
        Ok(LearnedHost {
            host: r.get(0)?,
            publication: r.get(1)?,
            first_ms: r.get(2)?,
            evidence: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

impl StateDb {
    /// The outcome rows of `note_id`, by stage.
    pub fn stage_outcomes(&self, note_id: i64) -> Result<Vec<StageOutcomeRow>> {
        outcomes_for(self.conn(), &[note_id])
    }

    /// The outcome rows of every id in `ids` (in `ids` order, then by stage).
    pub fn stage_outcomes_for(&self, ids: &[i64]) -> Result<Vec<StageOutcomeRow>> {
        outcomes_for(self.conn(), ids)
    }

    /// Inserts or replaces the row for `(r.note_id, r.stage)`; an existing row keeps its
    /// `first_ms`.
    pub fn upsert_stage_outcome(&mut self, r: &StageOutcomeRow) -> Result<()> {
        self.conn().execute(
            &format!(
                "INSERT INTO stage_outcomes ({COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (note_id, stage) DO UPDATE SET outcome = excluded.outcome,
                     reason = excluded.reason, attempts = excluded.attempts,
                     fingerprint = excluded.fingerprint, updated_ms = excluded.updated_ms"
            ),
            params![
                r.note_id,
                r.stage,
                r.outcome,
                r.reason,
                i64::from(r.attempts),
                r.fingerprint,
                r.first_ms,
                r.updated_ms
            ],
        )?;
        Ok(())
    }

    /// Deletes `note_id`'s row for `stage`, or every row of the note when `stage` is `None`;
    /// returns how many went.
    pub fn clear_stage_outcomes(&mut self, note_id: i64, stage: Option<&str>) -> Result<usize> {
        Ok(self.conn().execute(
            "DELETE FROM stage_outcomes WHERE note_id = ?1 AND (?2 IS NULL OR stage = ?2)",
            params![note_id, stage],
        )?)
    }

    /// Every learned host, by host.
    pub fn learned_hosts(&self) -> Result<Vec<LearnedHost>> {
        learned(self.conn())
    }

    /// Records `h` unless its host is already learned (first proof wins); returns whether it was
    /// inserted.
    pub fn insert_learned_host(&mut self, h: &LearnedHost) -> Result<bool> {
        Ok(self.conn().execute(
            "INSERT INTO learned_hosts (host, publication, first_ms, evidence)
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT (host) DO NOTHING",
            params![h.host, h.publication, h.first_ms, h.evidence],
        )? == 1)
    }

    /// Deletes the learned `host`; returns whether it existed.
    pub fn delete_learned_host(&mut self, host: &str) -> Result<bool> {
        Ok(self
            .conn()
            .execute("DELETE FROM learned_hosts WHERE host = ?1", [host])?
            == 1)
    }

    /// Every `(source_key, item_id, url)` of `source_kind`'s tracker rows, ordered by source key
    /// and item id (plan `intake-foundations` WP-3: the learned-host seed reads them).
    pub fn tracker_url_rows(&self, source_kind: &str) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn().prepare(
            "SELECT source_key, item_id, url FROM tracker_items WHERE source_kind = ?1
             ORDER BY source_key, item_id",
        )?;
        let rows = stmt.query_map([source_kind], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

impl StateReader {
    /// The outcome rows of every id in `ids`; none when the database predates v22.
    pub fn stage_outcomes_for(&self, ids: &[i64]) -> Result<Vec<StageOutcomeRow>> {
        if !has_table(&self.conn, "stage_outcomes")? {
            return Ok(Vec::new());
        }
        outcomes_for(&self.conn, ids)
    }

    /// Every learned host; none when the database predates v22.
    pub fn learned_hosts(&self) -> Result<Vec<LearnedHost>> {
        if !has_table(&self.conn, "learned_hosts")? {
            return Ok(Vec::new());
        }
        learned(&self.conn)
    }

    /// The live note id at `path`, if any; `None` when the database has no `note_ids` table.
    pub fn note_id_at(&self, path: &str) -> Result<Option<i64>> {
        if !has_table(&self.conn, "note_ids")? {
            return Ok(None);
        }
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM note_ids WHERE path = ?1 AND retired_ms IS NULL",
                [path],
                |r| r.get(0),
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QueueConfig;

    fn row(note_id: i64, stage: &str, outcome: &str, at: i64) -> StageOutcomeRow {
        StageOutcomeRow {
            note_id,
            stage: stage.into(),
            outcome: outcome.into(),
            reason: "r".into(),
            attempts: 1,
            fingerprint: "v1:0".into(),
            first_ms: at,
            updated_ms: at,
        }
    }

    #[test]
    fn upsert_read_clear_and_retirement_drops_rows() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = StateDb::open(&dir.path().join("state.db"), 1, QueueConfig::default()).unwrap();
        let a = i64::try_from(s.note_id_bind("a.md").unwrap().0).unwrap();
        let b = i64::try_from(s.note_id_bind("b.md").unwrap().0).unwrap();
        s.upsert_stage_outcome(&row(a, "enrich", "skipped", 5))
            .unwrap();
        let mut again = row(a, "enrich", "failed", 9);
        again.attempts = 2;
        s.upsert_stage_outcome(&again).unwrap();
        s.upsert_stage_outcome(&row(a, "localize", "failed", 6))
            .unwrap();
        s.upsert_stage_outcome(&row(b, "index", "failed", 7))
            .unwrap();
        let got = s.stage_outcomes(a).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(
            (
                got[0].outcome.as_str(),
                got[0].attempts,
                got[0].first_ms,
                got[0].updated_ms
            ),
            ("failed", 2, 5, 9),
            "first_ms survives an upsert"
        );
        assert_eq!(s.stage_outcomes_for(&[b, a]).unwrap().len(), 3);
        assert_eq!(s.clear_stage_outcomes(a, Some("localize")).unwrap(), 1);
        assert_eq!(s.stage_outcomes(a).unwrap().len(), 1);
        // Retirement (delete, and replace) drops the note's rows in the same transaction.
        s.note_id_retire("a.md", None, 10).unwrap();
        assert!(s.stage_outcomes(a).unwrap().is_empty());
        s.note_id_retire("b.md", None, 11).unwrap();
        assert!(s.stage_outcomes(b).unwrap().is_empty());
        // A stale binding retired by a move loses its rows too.
        let c = i64::try_from(s.note_id_bind("c.md").unwrap().0).unwrap();
        s.upsert_stage_outcome(&row(c, "enrich", "failed", 12))
            .unwrap();
        s.note_id_bind("d.md").unwrap();
        s.note_id_move("d.md", "c.md", 13).unwrap();
        assert!(s.stage_outcomes(c).unwrap().is_empty());
    }

    /// `unfinished` (plan intake-stage-jobs WP-2) is stored and read back as written, with its
    /// attempts, through both the daemon handle and the read-only reader; a reset clears it.
    #[test]
    fn unfinished_rows_carry_attempts_and_reset_clears_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut s = StateDb::open(&path, 1, QueueConfig::default()).unwrap();
        let a = i64::try_from(s.note_id_bind("a.md").unwrap().0).unwrap();
        let mut r = row(a, "enrich", "unfinished", 5);
        r.attempts = 2;
        s.upsert_stage_outcome(&r).unwrap();
        s.upsert_stage_outcome(&row(a, "localize", "failed", 6))
            .unwrap();
        let got = StateReader::open(&path)
            .unwrap()
            .stage_outcomes_for(&[a])
            .unwrap();
        let un = got.iter().find(|g| g.stage == "enrich").unwrap();
        assert_eq!((un.outcome.as_str(), un.attempts), ("unfinished", 2));
        assert_eq!(s.clear_stage_outcomes(a, None).unwrap(), 2);
        assert!(s.stage_outcomes(a).unwrap().is_empty());
    }

    #[test]
    fn learned_hosts_first_proof_wins() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = StateDb::open(&dir.path().join("state.db"), 1, QueueConfig::default()).unwrap();
        let h = |p: &str| LearnedHost {
            host: "example.org".into(),
            publication: p.into(),
            first_ms: 1,
            evidence: "rss-link".into(),
        };
        assert!(s.insert_learned_host(&h("A")).unwrap());
        assert!(!s.insert_learned_host(&h("B")).unwrap());
        assert_eq!(s.learned_hosts().unwrap(), vec![h("A")]);
        assert!(s.delete_learned_host("example.org").unwrap());
        assert!(!s.delete_learned_host("example.org").unwrap());
    }

    #[test]
    fn reader_on_a_v21_database_sees_no_rows() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.db");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(crate::migrate::BASELINE).unwrap();
            for m in &crate::migrate::MIGRATIONS[..4] {
                c.execute_batch(m).unwrap();
            }
            c.pragma_update(None, "user_version", 21).unwrap();
        }
        let r = StateReader::open(&p).unwrap();
        assert!(r.stage_outcomes_for(&[1]).unwrap().is_empty());
        assert!(r.learned_hosts().unwrap().is_empty());
    }
}
