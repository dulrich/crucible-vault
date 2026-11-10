//! Durable note ids (plan note-identity WP-1, D1 and the storage half of D3; schema v18): one
//! `note_ids` row per vault file identity, kept bound to the file's path synchronously on the
//! writer thread by the daemon's id sink.
//!
//! A row is **live** (`retired_ms IS NULL`: `path` is where the file is now; at most one live row
//! per path, enforced by a partial unique index) or a **tombstone** (`path` is its last path,
//! `replaced_by` the absorbing note's id for a replace). Ids come from `AUTOINCREMENT`, so
//! `sqlite_sequence` keeps the high-water mark and an id is never minted twice, tombstone pruning
//! included. Tombstones older than [`NOTE_ID_RETENTION_MS`] are pruned by [`StateDb::note_id_prune`];
//! a pruned id answers [`NoteLocation::Unknown`].

use crate::error::Result;
use crate::store::StateDb;
use cv_protocol::{NoteId, NoteLocation};
use rusqlite::{OptionalExtension, Transaction, params};

/// Tombstone retention: retired ids older than this are pruned (≥ 30 days required by D1).
pub const NOTE_ID_RETENTION_MS: i64 = 90 * 86_400_000;

fn to_id(v: i64) -> NoteId {
    NoteId(u64::try_from(v).unwrap_or(0))
}

fn to_i64(v: NoteId) -> i64 {
    i64::try_from(v.0).unwrap_or(i64::MAX)
}

fn live_at(tx: &Transaction<'_>, path: &str) -> Result<Option<i64>> {
    Ok(tx
        .query_row(
            "SELECT id FROM note_ids WHERE path = ?1 AND retired_ms IS NULL",
            params![path],
            |r| r.get(0),
        )
        .optional()?)
}

fn mint(tx: &Transaction<'_>, path: &str) -> Result<i64> {
    tx.execute("INSERT INTO note_ids (path) VALUES (?1)", params![path])?;
    Ok(tx.last_insert_rowid())
}

fn retire_row(tx: &Transaction<'_>, id: i64, by: Option<i64>, now: i64) -> Result<()> {
    tx.execute(
        "UPDATE note_ids SET retired_ms = ?2, replaced_by = ?3 WHERE id = ?1",
        params![id, now, by],
    )?;
    Ok(())
}

impl StateDb {
    /// The live id at `path`, if any (no mint).
    pub fn note_id_at(&self, path: &str) -> Result<Option<NoteId>> {
        let v: Option<i64> = self
            .conn()
            .query_row(
                "SELECT id FROM note_ids WHERE path = ?1 AND retired_ms IS NULL",
                params![path],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.map(to_id))
    }

    /// Every live row's path (the daemon's startup repair checks each against the vault).
    pub fn note_id_live_paths(&self) -> Result<Vec<String>> {
        let mut st = self
            .conn()
            .prepare("SELECT path FROM note_ids WHERE retired_ms IS NULL")?;
        let rows = st.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    }

    /// The live id at `path`, minting one if the path has none.
    pub fn note_id_bind(&mut self, path: &str) -> Result<NoteId> {
        let tx = self.conn.transaction()?;
        let id = match live_at(&tx, path)? {
            Some(id) => id,
            None => mint(&tx, path)?,
        };
        tx.commit()?;
        Ok(to_id(id))
    }

    /// The file at `from` now lives at `to`: its id (minted if `from` had none) moves to `to`. A
    /// stale live binding already at `to` is retired first (`now` is its retirement time), so a
    /// path never holds two live ids.
    pub fn note_id_move(&mut self, from: &str, to: &str, now: i64) -> Result<NoteId> {
        let tx = self.conn.transaction()?;
        let id = Self::move_in(&tx, from, to, now)?;
        tx.commit()?;
        Ok(to_id(id))
    }

    fn move_in(tx: &Transaction<'_>, from: &str, to: &str, now: i64) -> Result<i64> {
        if from == to {
            return match live_at(tx, to)? {
                Some(id) => Ok(id),
                None => mint(tx, to),
            };
        }
        if let Some(stale) = live_at(tx, to)? {
            retire_row(tx, stale, None, now)?;
        }
        match live_at(tx, from)? {
            Some(id) => {
                tx.execute(
                    "UPDATE note_ids SET path = ?2 WHERE id = ?1",
                    params![id, to],
                )?;
                Ok(id)
            }
            None => mint(tx, to),
        }
    }

    /// The file at `path` is gone (deleted, or absorbed by the note with id `replaced_by`): its id
    /// (minted first if the path had none, so the retirement is still nameable) becomes a
    /// tombstone. Returns the retired id.
    pub fn note_id_retire(
        &mut self,
        path: &str,
        replaced_by: Option<NoteId>,
        now: i64,
    ) -> Result<NoteId> {
        let tx = self.conn.transaction()?;
        let id = match live_at(&tx, path)? {
            Some(id) => id,
            None => mint(&tx, path)?,
        };
        retire_row(&tx, id, replaced_by.map(to_i64), now)?;
        tx.commit()?;
        Ok(to_id(id))
    }

    /// Makes the retired `id` live again at `path` (D3: a write by id to a deleted note keeps the
    /// id). `true` if `id` is now live at `path` (already live there included); `false` if `id`
    /// is unknown (never minted or pruned), live elsewhere, or `path` holds another live id.
    pub fn note_id_revive(&mut self, id: NoteId, path: &str) -> Result<bool> {
        let id = to_i64(id);
        let tx = self.conn.transaction()?;
        let row: Option<(String, Option<i64>)> = tx
            .query_row(
                "SELECT path, retired_ms FROM note_ids WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let ok = match row {
            None => false,
            Some((p, None)) => p == path,
            Some((_, Some(_))) => {
                if live_at(&tx, path)?.is_some() {
                    false
                } else {
                    tx.execute(
                        "UPDATE note_ids SET path = ?2, retired_ms = NULL, replaced_by = NULL
                         WHERE id = ?1",
                        params![id, path],
                    )?;
                    true
                }
            }
        };
        tx.commit()?;
        Ok(ok)
    }

    /// Where `id` is now.
    pub fn note_id_locate(&self, id: NoteId) -> Result<NoteLocation> {
        let row: Option<(String, Option<i64>, Option<i64>)> = self
            .conn()
            .query_row(
                "SELECT path, retired_ms, replaced_by FROM note_ids WHERE id = ?1",
                params![to_i64(id)],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(match row {
            None => NoteLocation::Unknown,
            Some((path, None, _)) => NoteLocation::Live { path },
            Some((_, Some(_), by)) => NoteLocation::Gone {
                replaced_by: by.map(to_id),
            },
        })
    }

    /// Prunes tombstones retired before `before_ms`; returns how many went. Live rows are never
    /// pruned, and the `AUTOINCREMENT` high-water mark keeps a pruned id from being minted again.
    pub fn note_id_prune(&mut self, before_ms: i64) -> Result<usize> {
        Ok(self.conn().execute(
            "DELETE FROM note_ids WHERE retired_ms IS NOT NULL AND retired_ms < ?1",
            params![before_ms],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QueueConfig;
    use NoteLocation::{Gone, Live, Unknown};

    fn db() -> (tempfile::TempDir, StateDb) {
        let dir = tempfile::tempdir().unwrap();
        let s = StateDb::open(&dir.path().join("state.db"), 1, QueueConfig::default()).unwrap();
        (dir, s)
    }

    fn live(p: &str) -> NoteLocation {
        Live { path: p.into() }
    }

    #[test]
    fn rename_chain_keeps_one_id() {
        let (_d, mut s) = db();
        let a = s.note_id_bind("a.md").unwrap();
        assert_eq!(s.note_id_bind("a.md").unwrap(), a, "bind is mint-or-get");
        assert_eq!(s.note_id_move("a.md", "b.md", 1).unwrap(), a);
        assert_eq!(s.note_id_move("b.md", "c/d.md", 2).unwrap(), a);
        assert_eq!(s.note_id_locate(a).unwrap(), live("c/d.md"));
        assert_eq!(s.note_id_at("a.md").unwrap(), None);
        assert_eq!(s.note_id_at("b.md").unwrap(), None);
        // An unbound source mints at the target.
        let x = s.note_id_move("never.md", "x.md", 3).unwrap();
        assert!(x > a);
        assert_eq!(s.note_id_locate(x).unwrap(), live("x.md"));
    }

    #[test]
    fn replace_and_delete_leave_tombstones() {
        let (_d, mut s) = db();
        let a = s.note_id_bind("a.md").unwrap();
        let b = s.note_id_bind("b.md").unwrap();
        assert_eq!(s.note_id_retire("a.md", Some(b), 5).unwrap(), a);
        assert_eq!(
            s.note_id_locate(a).unwrap(),
            Gone {
                replaced_by: Some(b)
            }
        );
        assert_eq!(s.note_id_retire("b.md", None, 6).unwrap(), b);
        assert_eq!(s.note_id_locate(b).unwrap(), Gone { replaced_by: None });
        assert_eq!(s.note_id_locate(NoteId(9999)).unwrap(), Unknown);
    }

    #[test]
    fn path_reuse_gets_a_new_id_and_revive_restores_the_old_one() {
        let (_d, mut s) = db();
        let old = s.note_id_bind("p.md").unwrap();
        s.note_id_retire("p.md", None, 1).unwrap();
        let new = s.note_id_bind("p.md").unwrap();
        assert_ne!(old, new);
        assert_eq!(s.note_id_locate(old).unwrap(), Gone { replaced_by: None });
        assert_eq!(s.note_id_locate(new).unwrap(), live("p.md"));
        // Revive refuses a path held by another live id, then succeeds once it is free.
        assert!(!s.note_id_revive(old, "p.md").unwrap());
        assert!(s.note_id_revive(old, "q.md").unwrap());
        assert_eq!(s.note_id_locate(old).unwrap(), live("q.md"));
        assert!(s.note_id_revive(old, "q.md").unwrap(), "already live there");
        assert!(!s.note_id_revive(old, "r.md").unwrap(), "live elsewhere");
        assert!(!s.note_id_revive(NoteId(4242), "z.md").unwrap(), "unknown");
    }

    #[test]
    fn move_onto_a_stale_binding_retires_it() {
        let (_d, mut s) = db();
        let a = s.note_id_bind("a.md").unwrap();
        let stale = s.note_id_bind("b.md").unwrap();
        s.note_id_move("a.md", "b.md", 7).unwrap();
        assert_eq!(s.note_id_locate(a).unwrap(), live("b.md"));
        assert_eq!(s.note_id_locate(stale).unwrap(), Gone { replaced_by: None });
    }

    #[test]
    fn pruning_leaves_unknown_and_never_remints_a_pruned_id() {
        let (_d, mut s) = db();
        let a = s.note_id_bind("a.md").unwrap();
        let b = s.note_id_bind("b.md").unwrap();
        s.note_id_retire("a.md", None, 10).unwrap();
        s.note_id_retire("b.md", None, 10_000).unwrap();
        assert_eq!(s.note_id_prune(100).unwrap(), 1);
        assert_eq!(s.note_id_locate(a).unwrap(), Unknown);
        assert_eq!(s.note_id_locate(b).unwrap(), Gone { replaced_by: None });
        // Prune everything, including the highest id: the next mint is still above it.
        assert_eq!(s.note_id_prune(i64::MAX).unwrap(), 1);
        let c = s.note_id_bind("a.md").unwrap();
        assert!(c > b, "pruned ids are never minted again");
    }

    #[test]
    fn v17_database_migrates_to_v19_and_bindings_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.db");
        {
            let c = rusqlite::Connection::open(&p).unwrap();
            c.execute_batch(crate::migrate::BASELINE).unwrap();
            c.pragma_update(None, "user_version", crate::migrate::BASELINE_VERSION)
                .unwrap();
        }
        let a = {
            let mut s = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
            assert_eq!(s.schema_version().unwrap(), 19);
            s.note_id_bind("a.md").unwrap()
        };
        let mut s = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
        assert_eq!(s.note_id_bind("a.md").unwrap(), a, "same id after reopen");
        assert_eq!(s.note_id_locate(a).unwrap(), live("a.md"));
    }
}
