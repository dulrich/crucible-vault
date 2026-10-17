//! Tracker bookkeeping (migration v5, plan `v0-automation-pipelines`): `tracker_items`,
//! `ignored_ids` and `api_usage`. WP-1 ships minimal insert/get/list accessors; the trackers'
//! policy (seen sets, automatable windows, quota checks) belongs to WP-4/7/8/9.

use crate::error::Result;
use crate::store::StateDb;
use rusqlite::{OptionalExtension, params};

/// What a tracker did with an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackerOutcome {
    /// A daily intake line was appended.
    Line,
    /// A captured note was created (automatable blog post).
    Note,
    /// Filtered by `ignored_ids` / Ignore Shorts.
    Ignored,
    /// Seen but not surfaced (already in the vault, skipped row, …).
    Skipped,
}

impl TrackerOutcome {
    /// The stored string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TrackerOutcome::Line => "line",
            TrackerOutcome::Note => "note",
            TrackerOutcome::Ignored => "ignored",
            TrackerOutcome::Skipped => "skipped",
        }
    }

    /// Parses a stored string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "line" => TrackerOutcome::Line,
            "note" => TrackerOutcome::Note,
            "ignored" => TrackerOutcome::Ignored,
            "skipped" => TrackerOutcome::Skipped,
            _ => return None,
        })
    }
}

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
}

const COLS: &str = "source_kind, source_key, item_id, title, url, first_seen_ms, day, outcome, \
                    score, full_body, note_path, source_name, duration_s, word_count";

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
    })
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
                "INSERT INTO tracker_items ({COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
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

    /// Marks one item done (`done_ms` = now) or not done (`done_ms` = NULL). Returns whether a
    /// row matched; raises the `tracker_items` change signal when one did.
    pub fn set_tracker_item_done(
        &mut self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
        done: bool,
    ) -> Result<bool> {
        let done_ms = done.then(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        });
        let n = self.conn().execute(
            "UPDATE tracker_items SET done_ms = ?4
             WHERE source_kind = ?1 AND source_key = ?2 AND item_id = ?3",
            params![source_kind, source_key, item_id, done_ms],
        )?;
        if n > 0 {
            self.changed("tracker_items");
        }
        Ok(n > 0)
    }

    /// Whether one item is marked done (`None` when no such row).
    pub fn tracker_item_done(
        &self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
    ) -> Result<Option<bool>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT done_ms IS NOT NULL FROM tracker_items
                 WHERE source_kind = ?1 AND source_key = ?2 AND item_id = ?3",
                params![source_kind, source_key, item_id],
                |r| r.get(0),
            )
            .optional()?)
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
