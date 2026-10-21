//! Tracker bookkeeping (migration v5, plan `v0-automation-pipelines`): `tracker_items`,
//! `ignored_ids` and `api_usage`. WP-1 ships minimal insert/get/list accessors; the trackers'
//! policy (seen sets, automatable windows, quota checks) belongs to WP-4/7/8/9.

use crate::error::Result;
use crate::store::StateDb;
use cv_protocol::TrackerOutcome;
use rusqlite::{OptionalExtension, params};

/// One `tracker_items` row. Key: (`source_kind`, `source_key`, `item_id`).
#[derive(Debug, Clone, PartialEq)]
pub struct TrackerItem {
    /// `youtube` | `blog`.
    pub source_kind: String,
    /// Channel id / feed URL.
    pub source_key: String,
    /// Video id / post id.
    pub item_id: String,
    /// Item title.
    pub title: String,
    /// Item URL.
    pub url: String,
    /// When the tracker first saw it.
    pub first_seen_ms: i64,
    /// Local intake day `YYYY-MM-DD`.
    pub day: String,
    /// What happened to it.
    pub outcome: TrackerOutcome,
    /// Importance score (WP-10), if scored.
    pub score: Option<f64>,
    /// Automatable evidence: whether the fetched item carried a full body (blogs only).
    pub full_body: Option<bool>,
    /// Vault-relative path of the captured note (`Note` outcomes; migration v6).
    pub note_path: Option<String>,
    /// Display name of the source at record time (migration v7).
    pub source_name: Option<String>,
    /// Item duration in seconds (YouTube; migration v7).
    pub duration_s: Option<i64>,
    /// Body word count (blogs with a full body; migration v7).
    pub word_count: Option<i64>,
    /// Probed access tier (`free` | `paid` | `founding` | `unknown`; migration v12).
    pub access: Option<String>,
    /// Whether the delivered body is the complete post (`None` = unknown; migration v12).
    pub complete: Option<bool>,
    /// Full post word count reported by the probe (migration v12).
    pub full_word_count: Option<i64>,
}

const COLS: &str = "source_kind, source_key, item_id, title, url, first_seen_ms, day, outcome, \
                    score, full_body, note_path, source_name, duration_s, word_count, access, complete, \
                    full_word_count";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TrackerItem> {
    let outcome: String = r.get(7)?;
    Ok(TrackerItem {
        source_kind: r.get(0)?,
        source_key: r.get(1)?,
        item_id: r.get(2)?,
        title: r.get(3)?,
        url: r.get(4)?,
        first_seen_ms: r.get(5)?,
        day: r.get(6)?,
        outcome: TrackerOutcome::parse(&outcome).unwrap_or(TrackerOutcome::Skipped),
        score: r.get(8)?,
        full_body: r.get(9)?,
        note_path: r.get(10)?,
        source_name: r.get(11)?,
        duration_s: r.get(12)?,
        word_count: r.get(13)?,
        access: r.get(14)?,
        complete: r.get(15)?,
        full_word_count: r.get(16)?,
    })
}

/// What [`StateDb::rekey_tracker_item`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rekey {
    /// No row at the new key: the old row's key was updated in place.
    Moved,
    /// A row already sat at the new key: it absorbed the old row's NULL fields; the old row is gone.
    Merged,
    /// No row at the old key (already moved) or the keys are equal: nothing changed.
    Absent,
}

/// `tracker_items` listing filter (every field optional; `limit` 0 = unlimited).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntakeQuery {
    /// `youtube` | `blog`.
    pub source_kind: Option<String>,
    /// Channel id / feed URL.
    pub source_key: Option<String>,
    /// Intake day `YYYY-MM-DD`.
    pub day: Option<String>,
    /// Outcome filter.
    pub outcome: Option<TrackerOutcome>,
    /// Max rows (0 = unlimited).
    pub limit: u32,
    /// Rows to skip.
    pub offset: u32,
}

/// Lists rows matching `q`, ordered `first_seen_ms DESC, item_id`.
pub(crate) fn list(conn: &rusqlite::Connection, q: &IntakeQuery) -> Result<Vec<TrackerItem>> {
    let limit: i64 = if q.limit == 0 { -1 } else { i64::from(q.limit) };
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM tracker_items
         WHERE (?1 IS NULL OR source_kind = ?1) AND (?2 IS NULL OR source_key = ?2)
           AND (?3 IS NULL OR day = ?3) AND (?4 IS NULL OR outcome = ?4)
         ORDER BY first_seen_ms DESC, item_id ASC LIMIT ?5 OFFSET ?6"
    ))?;
    let rows = stmt.query_map(
        params![
            q.source_kind,
            q.source_key,
            q.day,
            q.outcome.map(TrackerOutcome::as_str),
            limit,
            i64::from(q.offset)
        ],
        row,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

impl StateDb {
    /// Inserts `item` unless its key exists (first sighting wins). Returns `true` when inserted.
    pub fn insert_tracker_item(&mut self, item: &TrackerItem) -> Result<bool> {
        let n = self.conn().execute(
            &format!(
                "INSERT INTO tracker_items ({COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
                 ON CONFLICT DO NOTHING"
            ),
            params![
                item.source_kind,
                item.source_key,
                item.item_id,
                item.title,
                item.url,
                item.first_seen_ms,
                item.day,
                item.outcome.as_str(),
                item.score,
                item.full_body,
                item.note_path,
                item.source_name,
                item.duration_s,
                item.word_count,
                item.access,
                item.complete,
                item.full_word_count,
            ],
        )?;
        if n > 0 {
            self.changed("tracker_items");
        }
        Ok(n > 0)
    }

    /// Sets `source_name` on one source's rows where it is NULL (the name self-heal for rows
    /// recorded before migration v7). Returns rows changed; raises the `tracker_items` change
    /// signal only when some changed.
    pub fn fill_tracker_source_name(
        &mut self,
        source_kind: &str,
        source_key: &str,
        source_name: &str,
    ) -> Result<usize> {
        let n = self.conn().execute(
            "UPDATE tracker_items SET source_name = ?3
             WHERE source_kind = ?1 AND source_key = ?2 AND source_name IS NULL",
            params![source_kind, source_key, source_name],
        )?;
        if n > 0 {
            self.changed("tracker_items");
        }
        Ok(n)
    }

    /// Sets `word_count` on one `line`/`note` row where it is NULL (the length backfill for rows
    /// recorded before migration v7). Returns rows changed; raises the `tracker_items` change
    /// signal only when some changed.
    pub fn fill_tracker_word_count(
        &mut self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
        word_count: i64,
    ) -> Result<usize> {
        self.fill_tracker_length("word_count", source_kind, source_key, item_id, word_count)
    }

    /// Sets `duration_s` on one `line`/`note` row where it is NULL (`0` = the video is no longer
    /// available, rendered blank). Returns rows changed; raises the `tracker_items` change signal
    /// only when some changed.
    pub fn fill_tracker_duration(
        &mut self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
        duration_s: i64,
    ) -> Result<usize> {
        self.fill_tracker_length("duration_s", source_kind, source_key, item_id, duration_s)
    }

    fn fill_tracker_length(
        &mut self,
        column: &'static str,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
        value: i64,
    ) -> Result<usize> {
        let n = self.conn().execute(
            &format!(
                "UPDATE tracker_items SET {column} = ?4
                 WHERE source_kind = ?1 AND source_key = ?2 AND item_id = ?3
                   AND {column} IS NULL AND outcome IN ('line', 'note')"
            ),
            params![source_kind, source_key, item_id, value],
        )?;
        if n > 0 {
            self.changed("tracker_items");
        }
        Ok(n)
    }

    /// `(source_key, item_id)` of up to `limit` `line`/`note` rows of `source_kind` whose
    /// `duration_s` is NULL (the YouTube duration backfill), oldest first.
    pub fn tracker_items_missing_duration(
        &self,
        source_kind: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        let mut st = self.conn().prepare(
            "SELECT source_key, item_id FROM tracker_items
             WHERE source_kind = ?1 AND outcome IN ('line', 'note') AND duration_s IS NULL
             ORDER BY first_seen_ms, item_id LIMIT ?2",
        )?;
        let rows = st
            .query_map(
                params![source_kind, i64::try_from(limit).unwrap_or(i64::MAX)],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Sets the access/completeness evidence on one row (plan auto-intake-sprint WP-2; for the
    /// re-probe and backfill paths). Returns whether a row matched; raises the `tracker_items`
    /// change signal when one did.
    pub fn set_tracker_completeness(
        &mut self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
        access: Option<&str>,
        complete: Option<bool>,
        full_word_count: Option<i64>,
    ) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE tracker_items SET access = ?4, complete = ?5, full_word_count = ?6
             WHERE source_kind = ?1 AND source_key = ?2 AND item_id = ?3",
            params![
                source_kind,
                source_key,
                item_id,
                access,
                complete,
                full_word_count
            ],
        )?;
        if n > 0 {
            self.changed("tracker_items");
        }
        Ok(n > 0)
    }

    /// Sets one item's task status (`None` = open). Returns whether a row matched; raises the
    /// `tracker_items` change signal when one did.
    pub fn set_tracker_item_status(
        &mut self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
        status: Option<char>,
    ) -> Result<bool> {
        let status = status.map(String::from);
        let n = self.conn().execute(
            "UPDATE tracker_items SET status = ?4
             WHERE source_kind = ?1 AND source_key = ?2 AND item_id = ?3",
            params![source_kind, source_key, item_id, status],
        )?;
        if n > 0 {
            self.changed("tracker_items");
        }
        Ok(n > 0)
    }

    /// Distinct `(source_key, source_name)` pairs recorded for `source_kind` (round-8 WP-1: the
    /// retroactive Ignore Shorts pass matches sources with the tracker's own predicate).
    pub fn tracker_sources(&self, source_kind: &str) -> Result<Vec<(String, Option<String>)>> {
        let mut st = self.conn().prepare(
            "SELECT DISTINCT source_key, source_name FROM tracker_items
             WHERE source_kind = ?1 ORDER BY source_key, source_name",
        )?;
        let rows = st
            .query_map(params![source_kind], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// `(item_id, duration_s)` of up to `limit` rows of one source eligible for the retroactive
    /// Ignore Shorts pass (round-8 WP-1): `outcome = 'line'`, NULL status, a known duration
    /// (`> 0`; `0` is the backfill's "unavailable" sentinel) and no `ignored_ids` row for the
    /// item, with `duration_s < below` (the Ignore Shorts cap). The caller applies the channel
    /// predicate. Oldest first.
    pub fn tracker_items_cancel_candidates(
        &self,
        source_kind: &str,
        source_key: &str,
        below: u32,
        limit: usize,
    ) -> Result<Vec<(String, i64)>> {
        let mut st = self.conn().prepare(
            "SELECT t.item_id, t.duration_s FROM tracker_items t
             WHERE t.source_kind = ?1 AND t.source_key = ?2 AND t.outcome = 'line'
               AND t.status IS NULL AND t.duration_s > 0 AND t.duration_s < ?4
               AND NOT EXISTS (SELECT 1 FROM ignored_ids i
                               WHERE i.kind = t.source_kind AND i.id = t.item_id)
             ORDER BY t.first_seen_ms, t.item_id LIMIT ?3",
        )?;
        let rows = st
            .query_map(
                params![
                    source_kind,
                    source_key,
                    i64::try_from(limit).unwrap_or(i64::MAX),
                    i64::from(below)
                ],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Cancels one still-eligible `line` row (round-8 WP-1): `outcome = 'ignored'`, status `-`,
    /// plus an `ignored_ids` row (`reason`), in one transaction. A row whose status is non-NULL
    /// or that is no longer a `line` is left alone. Returns whether the row changed.
    pub fn cancel_tracker_item(
        &mut self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
        reason: &str,
        at_ms: i64,
    ) -> Result<bool> {
        let tx = self.conn().unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE tracker_items SET outcome = 'ignored', status = '-'
             WHERE source_kind = ?1 AND source_key = ?2 AND item_id = ?3
               AND outcome = 'line' AND status IS NULL",
            params![source_kind, source_key, item_id],
        )?;
        if n > 0 {
            tx.execute(
                "INSERT INTO ignored_ids (kind, id, reason, at_ms) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT DO NOTHING",
                params![source_kind, item_id, reason, at_ms],
            )?;
        }
        tx.commit()?;
        if n > 0 {
            self.changed("tracker_items");
        }
        Ok(n > 0)
    }

    /// Sets status `-` on every row of `(source_kind, item_id)` (any source; the manual
    /// `youtube-ignore-video` step, round-8 WP-1). Outcome is unchanged. Returns rows changed.
    pub fn cancel_tracker_items_by_id(
        &mut self,
        source_kind: &str,
        item_id: &str,
    ) -> Result<usize> {
        let n = self.conn().execute(
            "UPDATE tracker_items SET status = '-'
             WHERE source_kind = ?1 AND item_id = ?2 AND status IS NOT '-'",
            params![source_kind, item_id],
        )?;
        if n > 0 {
            self.changed("tracker_items");
        }
        Ok(n)
    }

    /// Legacy alias of [`Self::set_tracker_item_status`]: `true` → `'x'`, `false` → open.
    pub fn set_tracker_item_done(
        &mut self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
        done: bool,
    ) -> Result<bool> {
        self.set_tracker_item_status(source_kind, source_key, item_id, done.then_some('x'))
    }

    /// One item's task status (`Ok(None)` when no such row; `Some(None)` = open).
    pub fn tracker_item_status(
        &self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
    ) -> Result<Option<Option<char>>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT status FROM tracker_items
                 WHERE source_kind = ?1 AND source_key = ?2 AND item_id = ?3",
                params![source_kind, source_key, item_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .map(|s| s.and_then(|s| s.chars().next())))
    }

    /// Whether one item is done (status `x`/`X`; `None` when no such row).
    pub fn tracker_item_done(
        &self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
    ) -> Result<Option<bool>> {
        Ok(self
            .tracker_item_status(source_kind, source_key, item_id)?
            .map(|s| matches!(s, Some('x' | 'X'))))
    }

    /// The row for one key.
    pub fn tracker_item(
        &self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
    ) -> Result<Option<TrackerItem>> {
        Ok(self
            .conn()
            .query_row(
                &format!(
                    "SELECT {COLS} FROM tracker_items
                     WHERE source_kind = ?1 AND source_key = ?2 AND item_id = ?3"
                ),
                params![source_kind, source_key, item_id],
                row,
            )
            .optional()?)
    }

    /// Rows whose `note_path` is `path` (plan auto-intake-sprint WP-3), oldest first.
    pub fn tracker_items_for_note(&self, path: &str) -> Result<Vec<TrackerItem>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {COLS} FROM tracker_items WHERE note_path = ?1
             ORDER BY first_seen_ms, source_kind, source_key, item_id"
        ))?;
        let rows = stmt.query_map(params![path], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Sets one row's `note_path` (plan auto-intake-sprint WP-6 reconcile: a plain UPDATE, no
    /// schema change). Returns whether a row matched; raises the `tracker_items` change signal
    /// when one did.
    pub fn set_tracker_note_path(
        &mut self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
        note_path: &str,
    ) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE tracker_items SET note_path = ?4
             WHERE source_kind = ?1 AND source_key = ?2 AND item_id = ?3",
            params![source_kind, source_key, item_id, note_path],
        )?;
        if n > 0 {
            self.changed("tracker_items");
        }
        Ok(n > 0)
    }

    /// The `note`-outcome rows reconcile looks at (plan auto-intake-sprint WP-6): those with no
    /// `note_path` or whose status is not done (`x`/`X`), each with its status, oldest first.
    pub fn tracker_reconcile_rows(&self) -> Result<Vec<(TrackerItem, Option<char>)>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {COLS}, status FROM tracker_items
             WHERE outcome = 'note'
               AND (note_path IS NULL OR status IS NULL OR status NOT IN ('x', 'X'))
             ORDER BY first_seen_ms, source_kind, source_key, item_id"
        ))?;
        let rows = stmt.query_map([], |r| {
            let status: Option<String> = r.get(17)?;
            Ok((row(r)?, status.and_then(|s| s.chars().next())))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every open row (`status IS NULL`) of `source_kind`, any outcome, oldest first (plan
    /// intake-followups-mail-youtube WP-5: reconcile finalizes YouTube rows that never reached a
    /// `note` outcome).
    pub fn tracker_open_rows(&self, source_kind: &str) -> Result<Vec<TrackerItem>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {COLS} FROM tracker_items WHERE source_kind = ?1 AND status IS NULL
             ORDER BY first_seen_ms, source_key, item_id"
        ))?;
        let rows = stmt.query_map(params![source_kind], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every row of one source, any outcome and status, oldest first (plan
    /// intake-followups-mail-youtube WP-5: the `unknown-sender` mail re-match).
    pub fn tracker_rows_of_source(
        &self,
        source_kind: &str,
        source_key: &str,
    ) -> Result<Vec<TrackerItem>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {COLS} FROM tracker_items WHERE source_kind = ?1 AND source_key = ?2
             ORDER BY first_seen_ms, item_id"
        ))?;
        let rows = stmt.query_map(params![source_kind, source_key], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Moves one row from `old_key` to `new_key` (and sets its `source_name`) in one transaction
    /// (plan intake-followups-mail-youtube WP-5, audit E2).
    ///
    /// No row at `new_key`: the key is updated in place (status, `note_path`, evidence kept).
    /// A row already at `new_key` (collision): that row is kept, its NULL `note_path` / `access`
    /// / `complete` / `full_word_count` / `status` are filled from the old row, and the old row
    /// is deleted. No row at `old_key` (already moved: an interrupted run's re-run) or equal keys
    /// is [`Rekey::Absent`], a no-op.
    pub fn rekey_tracker_item(
        &mut self,
        source_kind: &str,
        old_key: &str,
        item_id: &str,
        new_key: &str,
        new_source_name: &str,
    ) -> Result<Rekey> {
        if old_key == new_key {
            return Ok(Rekey::Absent);
        }
        let tx = self.conn().unchecked_transaction()?;
        let exists = |key: &str| -> rusqlite::Result<bool> {
            tx.query_row(
                "SELECT 1 FROM tracker_items
                 WHERE source_kind = ?1 AND source_key = ?2 AND item_id = ?3",
                params![source_kind, key, item_id],
                |_| Ok(()),
            )
            .optional()
            .map(|r| r.is_some())
        };
        if !exists(old_key)? {
            return Ok(Rekey::Absent);
        }
        let out = if exists(new_key)? {
            let fill = |col: &str| {
                format!(
                    "{col} = COALESCE({col}, (SELECT {col} FROM tracker_items
                       WHERE source_kind = ?1 AND source_key = ?2 AND item_id = ?3))"
                )
            };
            let set = [
                "note_path",
                "access",
                "complete",
                "full_word_count",
                "status",
            ]
            .map(fill)
            .join(", ");
            tx.execute(
                &format!(
                    "UPDATE tracker_items SET {set}
                     WHERE source_kind = ?1 AND source_key = ?4 AND item_id = ?3"
                ),
                params![source_kind, old_key, item_id, new_key],
            )?;
            tx.execute(
                "DELETE FROM tracker_items
                 WHERE source_kind = ?1 AND source_key = ?2 AND item_id = ?3",
                params![source_kind, old_key, item_id],
            )?;
            Rekey::Merged
        } else {
            tx.execute(
                "UPDATE tracker_items SET source_key = ?4, source_name = ?5
                 WHERE source_kind = ?1 AND source_key = ?2 AND item_id = ?3",
                params![source_kind, old_key, item_id, new_key, new_source_name],
            )?;
            Rekey::Moved
        };
        tx.commit()?;
        self.changed("tracker_items");
        Ok(out)
    }

    /// Rows whose `url` is exactly `url` (plan auto-intake-sprint WP-3), oldest first.
    pub fn tracker_items_for_url(&self, url: &str) -> Result<Vec<TrackerItem>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {COLS} FROM tracker_items WHERE url = ?1
             ORDER BY first_seen_ms, source_kind, source_key, item_id"
        ))?;
        let rows = stmt.query_map(params![url], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Rows of `source_kind` whose `item_id` is `item_id`, any source key (plan
    /// intake-followups-mail-youtube WP-4: a YouTube clip resolves its rows by video id), oldest
    /// first.
    pub fn tracker_items_for_item(
        &self,
        source_kind: &str,
        item_id: &str,
    ) -> Result<Vec<TrackerItem>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {COLS} FROM tracker_items WHERE source_kind = ?1 AND item_id = ?2
             ORDER BY first_seen_ms, source_kind, source_key, item_id"
        ))?;
        let rows = stmt.query_map(params![source_kind, item_id], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// One source's rows, newest first, at most `limit`.
    pub fn tracker_items_for_source(
        &self,
        source_kind: &str,
        source_key: &str,
        limit: usize,
    ) -> Result<Vec<TrackerItem>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {COLS} FROM tracker_items WHERE source_kind = ?1 AND source_key = ?2
             ORDER BY first_seen_ms DESC, item_id LIMIT ?3"
        ))?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map(params![source_kind, source_key, limit], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every `item_id` recorded for `source_kind` (any source, any outcome), sorted — the
    /// tracker seen-set's state part (WP-7).
    pub fn tracker_item_ids(&self, source_kind: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn().prepare(
            "SELECT DISTINCT item_id FROM tracker_items WHERE source_kind = ?1 ORDER BY item_id",
        )?;
        let rows = stmt.query_map(params![source_kind], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Whether `source_kind` has any `tracker_items` row (first-run seeding test, WP-7).
    pub fn has_tracker_items(&self, source_kind: &str) -> Result<bool> {
        Ok(self
            .conn()
            .query_row(
                "SELECT 1 FROM tracker_items WHERE source_kind = ?1 LIMIT 1",
                params![source_kind],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Records `(kind, id)` as ignored. Returns `true` when newly inserted.
    pub fn insert_ignored_id(
        &mut self,
        kind: &str,
        id: &str,
        reason: &str,
        at_ms: i64,
    ) -> Result<bool> {
        let n = self.conn().execute(
            "INSERT INTO ignored_ids (kind, id, reason, at_ms) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT DO NOTHING",
            params![kind, id, reason, at_ms],
        )?;
        Ok(n > 0)
    }

    /// Whether `(kind, id)` is ignored.
    pub fn is_ignored(&self, kind: &str, id: &str) -> Result<bool> {
        Ok(self
            .conn()
            .query_row(
                "SELECT 1 FROM ignored_ids WHERE kind = ?1 AND id = ?2",
                params![kind, id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Every ignored id of `kind`, sorted.
    pub fn ignored_ids(&self, kind: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn()
            .prepare("SELECT id FROM ignored_ids WHERE kind = ?1 ORDER BY id")?;
        let rows = stmt.query_map(params![kind], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Adds `units` to `service`'s usage on `day`; returns the new total.
    pub fn add_api_usage(&mut self, service: &str, day: &str, units: i64) -> Result<i64> {
        Ok(self.conn().query_row(
            "INSERT INTO api_usage (service, day, units) VALUES (?1, ?2, ?3)
             ON CONFLICT(service, day) DO UPDATE SET units = units + excluded.units
             RETURNING units",
            params![service, day, units],
            |r| r.get(0),
        )?)
    }

    /// `service`'s units on `day` (0 when none).
    pub fn api_usage(&self, service: &str, day: &str) -> Result<i64> {
        Ok(self
            .conn()
            .query_row(
                "SELECT units FROM api_usage WHERE service = ?1 AND day = ?2",
                params![service, day],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }
}
