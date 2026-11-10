//! Durable path lineage (plan note-path-continuity WP-2, D2; schema v17): one `path_lineage` row
//! per identity transition, appended synchronously on the writer thread by the daemon's lineage
//! sink, and the chain resolution every path holder (editor document, job context) uses to find
//! where the note it bound at a cursor went.
//!
//! A cursor is a `seq`: "the holder's path was valid as of every edge up to and including this
//! number". Resolution follows only edges after the cursor, so a reused path (X moved A→B, then Y
//! created at A) resolves per incarnation. `created` rows start an incarnation and are never
//! followed. Retention is bounded by [`StateDb::lineage_prune`]; a cursor below the pruned floor
//! resolves [`LineageEnd::Stale`] (never guessed).

use crate::error::Result;
use crate::store::StateDb;
use rusqlite::{OptionalExtension, params};

/// Lineage retention: rows older than this are pruned (≥ 30 days required by D2).
pub const LINEAGE_RETENTION_MS: i64 = 90 * 86_400_000;

/// Hop cap for one resolution (cursors strictly increase, so a chain always terminates; the cap
/// only bounds pathological histories).
const MAX_HOPS: usize = 4096;

/// One identity transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageKind {
    /// A new incarnation starts at `from`.
    Created,
    /// The note at `from` now lives at `to`.
    Renamed,
    /// The note at `from` was deleted because `to` absorbed it.
    Replaced,
    /// The note at `from` is gone.
    Deleted,
}

impl LineageKind {
    /// The SQL spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Renamed => "renamed",
            Self::Replaced => "replaced",
            Self::Deleted => "deleted",
        }
    }
}

/// Where a (path, cursor) binding ends after following every later edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineageEnd {
    /// No identity transition left the path after the cursor (including a chain that came back
    /// to the same path): the holder's note is still there.
    Unchanged,
    /// The note now lives at this (different) path; a replaced hop is followed to its absorber.
    Moved(String),
    /// The chain ends in a delete.
    Deleted,
    /// The cursor is below the pruned floor: lineage can no longer vouch for it.
    Stale,
}

/// A resolution plus the lineage head it is valid through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageResolution {
    /// The chain's end.
    pub end: LineageEnd,
    /// The highest assigned `seq` when resolved: the holder's new cursor.
    pub head: u64,
}

fn to_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

impl StateDb {
    /// Appends one transition; returns its `seq` (strictly increasing, never reused).
    pub fn lineage_append(
        &mut self,
        kind: LineageKind,
        from: &str,
        to: Option<&str>,
        at_ms: i64,
    ) -> Result<u64> {
        self.conn().execute(
            "INSERT INTO path_lineage (kind, from_path, to_path, at_ms) VALUES (?1, ?2, ?3, ?4)",
            params![kind.as_str(), from, to, at_ms],
        )?;
        Ok(to_u64(self.conn().last_insert_rowid()))
    }

    /// The highest `seq` ever assigned (0 = none), pruned rows included.
    pub fn lineage_head(&self) -> Result<u64> {
        let v: Option<i64> = self
            .conn()
            .query_row(
                "SELECT seq FROM sqlite_sequence WHERE name = 'path_lineage'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(to_u64(v.unwrap_or(0)))
    }

    /// The highest pruned `seq` (0 = nothing pruned).
    pub fn lineage_floor(&self) -> Result<u64> {
        let v: i64 = self.conn().query_row(
            "SELECT pruned FROM path_lineage_floor WHERE id = 1",
            [],
            |r| r.get(0),
        )?;
        Ok(to_u64(v))
    }

    /// Follows every identity transition that left `path` after `since`, hop by hop.
    pub fn lineage_resolve(&self, path: &str, since: u64) -> Result<LineageResolution> {
        let head = self.lineage_head()?;
        if since < self.lineage_floor()? {
            return Ok(LineageResolution {
                end: LineageEnd::Stale,
                head,
            });
        }
        let mut stmt = self.conn().prepare_cached(
            "SELECT seq, kind, to_path FROM path_lineage
             WHERE from_path = ?1 AND seq > ?2 AND kind != 'created'
             ORDER BY seq LIMIT 1",
        )?;
        let mut cur = path.to_owned();
        let mut cursor = to_i64(since);
        for _ in 0..MAX_HOPS {
            let next: Option<(i64, String, Option<String>)> = stmt
                .query_row(params![cur, cursor], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?;
            let Some((seq, kind, to)) = next else {
                break;
            };
            match (kind.as_str(), to) {
                ("renamed" | "replaced", Some(to)) => {
                    cur = to;
                    cursor = seq;
                }
                _ => {
                    return Ok(LineageResolution {
                        end: LineageEnd::Deleted,
                        head,
                    });
                }
            }
        }
        let end = if cur == path {
            LineageEnd::Unchanged
        } else {
            LineageEnd::Moved(cur)
        };
        Ok(LineageResolution { end, head })
    }

    /// Prunes rows older than `before_ms` (and every row before them, so the retained rows are a
    /// suffix) and raises the floor; returns how many rows went.
    pub fn lineage_prune(&mut self, before_ms: i64) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let max: Option<i64> = tx.query_row(
            "SELECT MAX(seq) FROM path_lineage WHERE at_ms < ?1",
            params![before_ms],
            |r| r.get(0),
        )?;
        let Some(max) = max else {
            return Ok(0);
        };
        let n = tx.execute("DELETE FROM path_lineage WHERE seq <= ?1", params![max])?;
        tx.execute(
            "UPDATE path_lineage_floor SET pruned = MAX(pruned, ?1) WHERE id = 1",
            params![max],
        )?;
        tx.commit()?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QueueConfig;

    fn db() -> (tempfile::TempDir, StateDb) {
        let dir = tempfile::tempdir().unwrap();
        let s = StateDb::open(&dir.path().join("state.db"), 1, QueueConfig::default()).unwrap();
        (dir, s)
    }

    fn add(s: &mut StateDb, kind: LineageKind, from: &str, to: Option<&str>) -> u64 {
        s.lineage_append(kind, from, to, 1_000).unwrap()
    }

    fn end(s: &StateDb, path: &str, since: u64) -> LineageEnd {
        s.lineage_resolve(path, since).unwrap().end
    }

    #[test]
    fn chain_resolution_follows_every_hop_after_the_cursor() {
        use LineageKind::*;
        let (_d, mut s) = db();
        // Unknown path / empty lineage: unchanged at head 0.
        assert_eq!(
            s.lineage_resolve("nope.md", 0).unwrap(),
            LineageResolution {
                end: LineageEnd::Unchanged,
                head: 0
            }
        );
        // Multi-hop A→B→C with a replaced hop C→D.
        let c0 = add(&mut s, Created, "a.md", None);
        let r1 = add(&mut s, Renamed, "a.md", Some("b.md"));
        add(&mut s, Renamed, "b.md", Some("c.md"));
        assert_eq!(end(&s, "a.md", c0), LineageEnd::Moved("c.md".into()));
        assert_eq!(end(&s, "b.md", r1), LineageEnd::Moved("c.md".into()));
        let rep = add(&mut s, Replaced, "c.md", Some("d.md"));
        assert_eq!(end(&s, "a.md", c0), LineageEnd::Moved("d.md".into()));
        assert_eq!(s.lineage_head().unwrap(), rep);
        // A delete at the end of the chain.
        add(&mut s, Deleted, "d.md", None);
        assert_eq!(end(&s, "a.md", c0), LineageEnd::Deleted);
        // Path reuse: X at P moves P→Q, then Y is created at P.
        let x = add(&mut s, Created, "p.md", None);
        add(&mut s, Renamed, "p.md", Some("q.md"));
        let y = add(&mut s, Created, "p.md", None);
        assert_eq!(end(&s, "p.md", x), LineageEnd::Moved("q.md".into()));
        assert_eq!(end(&s, "p.md", y), LineageEnd::Unchanged);
        // A cycle back to a live path: M→N→M.
        let m = add(&mut s, Created, "m.md", None);
        add(&mut s, Renamed, "m.md", Some("n.md"));
        add(&mut s, Renamed, "n.md", Some("m.md"));
        assert_eq!(end(&s, "m.md", m), LineageEnd::Unchanged);
        assert_eq!(end(&s, "n.md", m), LineageEnd::Moved("m.md".into()));
        // An unknown path with lineage present: unchanged (the caller checks existence).
        assert_eq!(end(&s, "never.md", 0), LineageEnd::Unchanged);
        // Seq strictly increases.
        assert!(m > y && y > x && x > rep);
    }

    #[test]
    fn baseline_database_opens_with_an_empty_lineage() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.db");
        {
            let c = rusqlite::Connection::open(&p).unwrap();
            c.execute_batch(crate::migrate::BASELINE).unwrap();
            c.pragma_update(None, "user_version", crate::migrate::BASELINE_VERSION)
                .unwrap();
        }
        let mut s = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
        assert_eq!(
            s.schema_version().unwrap(),
            crate::migrate::latest_version()
        );
        assert_eq!(
            (s.lineage_head().unwrap(), s.lineage_floor().unwrap()),
            (0, 0)
        );
        assert_eq!(
            s.lineage_append(LineageKind::Created, "a.md", None, 1)
                .unwrap(),
            1
        );
    }

    #[test]
    fn pruning_keeps_the_recent_suffix_and_refuses_stale_cursors() {
        use LineageKind::*;
        let (_d, mut s) = db();
        let old = s.lineage_append(Created, "a.md", None, 10).unwrap();
        let mv = s.lineage_append(Renamed, "a.md", Some("b.md"), 20).unwrap();
        let new = s
            .lineage_append(Renamed, "b.md", Some("c.md"), 10_000)
            .unwrap();
        assert_eq!(s.lineage_prune(100).unwrap(), 2);
        assert_eq!(s.lineage_floor().unwrap(), mv);
        assert_eq!(end(&s, "a.md", old), LineageEnd::Stale);
        assert_eq!(end(&s, "b.md", mv), LineageEnd::Moved("c.md".into()));
        assert_eq!(s.lineage_prune(100).unwrap(), 0);
        // Numbers are never reused after a prune.
        s.lineage_prune(i64::MAX).unwrap();
        assert_eq!(s.lineage_head().unwrap(), new);
        assert!(s.lineage_append(Created, "z.md", None, 1).unwrap() > new);
    }
}
