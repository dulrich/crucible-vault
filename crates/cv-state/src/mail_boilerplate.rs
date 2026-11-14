//! Per-sender learned newsletter boilerplate (plan `round-10-feedback-todos` WP-2, migration
//! v13): for each newsletter `source_key`, how many learned messages contained each normalized
//! line (keyed by `cv_ingest::boilerplate::line_key`), plus the Message-IDs already learned so a
//! message counts once. The caller supplies the line keys; this crate stays free of the
//! normalization rules.

use std::collections::BTreeSet;

use crate::error::Result;
use crate::store::StateDb;
use rusqlite::params;

/// The u64 line key as the stored signed integer (bit-preserving).
fn to_db(k: u64) -> i64 {
    i64::from_ne_bytes(k.to_ne_bytes())
}

fn from_db(k: i64) -> u64 {
    u64::from_ne_bytes(k.to_ne_bytes())
}

impl StateDb {
    /// Every `(line_key, count)` learned for `source_key`.
    pub fn mail_boilerplate_lines(&self, source_key: &str) -> Result<Vec<(u64, u32)>> {
        let mut stmt = self
            .conn()
            .prepare_cached("SELECT line_key, count FROM mail_boilerplate WHERE source_key = ?1")?;
        let rows = stmt.query_map(params![source_key], |r| {
            let k: i64 = r.get(0)?;
            let c: i64 = r.get(1)?;
            Ok((from_db(k), u32::try_from(c).unwrap_or(u32::MAX)))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Whether `message_id` was already learned for `source_key`.
    pub fn mail_boilerplate_seen(&self, source_key: &str, message_id: &str) -> Result<bool> {
        let n: i64 = self.conn().query_row(
            "SELECT count(*) FROM mail_boilerplate_seen WHERE source_key = ?1 AND message_id = ?2",
            params![source_key, message_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Learns one message in one transaction: each distinct key of `line_keys` gains one count.
    /// Idempotent by `message_id` — returns `false` (and writes nothing) when it was learned before.
    pub fn learn_mail_boilerplate(
        &mut self,
        source_key: &str,
        message_id: &str,
        line_keys: &[u64],
    ) -> Result<bool> {
        let tx = self.conn().unchecked_transaction()?;
        let fresh = tx.execute(
            "INSERT OR IGNORE INTO mail_boilerplate_seen (source_key, message_id) VALUES (?1, ?2)",
            params![source_key, message_id],
        )? > 0;
        if fresh {
            let keys: BTreeSet<u64> = line_keys.iter().copied().collect();
            let mut stmt = tx.prepare_cached(
                "INSERT INTO mail_boilerplate (source_key, line_key, count) VALUES (?1, ?2, 1)
                 ON CONFLICT(source_key, line_key) DO UPDATE SET count = count + 1",
            )?;
            for k in keys {
                stmt.execute(params![source_key, to_db(k)])?;
            }
        }
        tx.commit()?;
        Ok(fresh)
    }
    /// Moves everything learned for `old_key` to `new_key` in one transaction (a fold renamed a
    /// mail `source_key`; plan sources-publication-model WP-2): line counts add onto `new_key`'s,
    /// seen Message-IDs union (so a message is still learned once), and `old_key`'s rows are
    /// deleted. Idempotent: nothing under `old_key` (or equal keys) is a no-op returning `false`.
    pub fn rekey_mail_boilerplate(&mut self, old_key: &str, new_key: &str) -> Result<bool> {
        if old_key == new_key {
            return Ok(false);
        }
        let tx = self.conn().unchecked_transaction()?;
        let lines = tx.execute(
            "INSERT INTO mail_boilerplate (source_key, line_key, count)
             SELECT ?2, line_key, count FROM mail_boilerplate WHERE source_key = ?1
             ON CONFLICT(source_key, line_key) DO UPDATE SET count = count + excluded.count",
            params![old_key, new_key],
        )?;
        let seen = tx.execute(
            "INSERT OR IGNORE INTO mail_boilerplate_seen (source_key, message_id)
             SELECT ?2, message_id FROM mail_boilerplate_seen WHERE source_key = ?1",
            params![old_key, new_key],
        )?;
        let gone = tx.execute(
            "DELETE FROM mail_boilerplate WHERE source_key = ?1",
            params![old_key],
        )? + tx.execute(
            "DELETE FROM mail_boilerplate_seen WHERE source_key = ?1",
            params![old_key],
        )?;
        tx.commit()?;
        Ok(lines + seen + gone > 0)
    }
}

#[cfg(test)]
mod tests {
    use crate::*;

    #[test]
    fn learns_once_per_message_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = StateDb::open(&dir.path().join("s.db"), 1, QueueConfig::default()).unwrap();
        let big = u64::MAX - 3;
        assert!(s.learn_mail_boilerplate("nl", "<a>", &[1, big, 1]).unwrap());
        assert!(!s.learn_mail_boilerplate("nl", "<a>", &[1, big]).unwrap());
        assert!(s.learn_mail_boilerplate("nl", "<b>", &[1]).unwrap());
        assert!(s.learn_mail_boilerplate("other", "<a>", &[1]).unwrap());
        let mut got = s.mail_boilerplate_lines("nl").unwrap();
        got.sort_unstable();
        assert_eq!(got, [(1, 2), (big, 1)]);
        assert!(s.mail_boilerplate_seen("nl", "<b>").unwrap());
        assert!(!s.mail_boilerplate_seen("nl", "<c>").unwrap());
    }

    #[test]
    fn rekey_moves_merges_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = StateDb::open(&dir.path().join("s.db"), 1, QueueConfig::default()).unwrap();
        // Fresh move: nothing under the new key.
        assert!(s.learn_mail_boilerplate("old", "<a>", &[1, 2]).unwrap());
        assert!(s.rekey_mail_boilerplate("old", "new").unwrap());
        assert!(s.mail_boilerplate_lines("old").unwrap().is_empty());
        assert!(!s.mail_boilerplate_seen("old", "<a>").unwrap());
        let mut got = s.mail_boilerplate_lines("new").unwrap();
        got.sort_unstable();
        assert_eq!(got, [(1, 1), (2, 1)]);
        assert!(s.mail_boilerplate_seen("new", "<a>").unwrap());
        // Idempotent re-run: the old key is absent.
        assert!(!s.rekey_mail_boilerplate("old", "new").unwrap());
        assert_eq!(s.mail_boilerplate_lines("new").unwrap().len(), 2);
        // Collision: counts add, seen ids union, the moved message is never learned again.
        assert!(s.learn_mail_boilerplate("old", "<b>", &[2, 3]).unwrap());
        assert!(s.rekey_mail_boilerplate("old", "new").unwrap());
        let mut got = s.mail_boilerplate_lines("new").unwrap();
        got.sort_unstable();
        assert_eq!(got, [(1, 1), (2, 2), (3, 1)]);
        assert!(s.mail_boilerplate_seen("new", "<a>").unwrap());
        assert!(s.mail_boilerplate_seen("new", "<b>").unwrap());
        assert!(!s.learn_mail_boilerplate("new", "<b>", &[9]).unwrap());
        assert!(s.mail_boilerplate_lines("old").unwrap().is_empty());
    }
}
