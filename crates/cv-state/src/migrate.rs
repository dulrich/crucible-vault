//! Forward-only, versioned migrations for `state.db`.
//!
//! The ladder is keyed on SQLite's `PRAGMA user_version`: step `i` (0-based) moves the
//! database from version `i` to `i + 1`, each inside its own transaction. state.db is
//! primary data (D14), so there is no down-migration and no "drop and rebuild" path;
//! a version newer than [`MIGRATIONS`] knows is refused with [`StateError::NewerSchema`].
//!
//! Crucible's store self-migrates via a meta-table version cookie
//! (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/AGENTS.md:55`, `db/SqliteJobStore.ts`); we use
//! `user_version` instead, which needs no table. Nothing Obsidian-induced is involved.

use crate::error::{Result, StateError};
use rusqlite::Connection;

/// Ordered migration steps. Later WPs append (never edit) entries.
pub const MIGRATIONS: &[&str] = &[
    // v1: jobs, service health, shadow writes.
    r"
CREATE TABLE jobs (
    id              TEXT PRIMARY KEY,
    job_type        TEXT NOT NULL,
    lane            INTEGER NOT NULL,
    status          TEXT NOT NULL,
    dedupe_key      TEXT,
    params          TEXT NOT NULL,
    resource_keys   TEXT NOT NULL,
    service         TEXT,
    created_ms      INTEGER NOT NULL,
    claim_token     TEXT,
    claimed_at_ms   INTEGER,
    defer_until_ms  INTEGER,
    settled_at_ms   INTEGER,
    attempts        INTEGER NOT NULL DEFAULT 0,
    error           TEXT,
    failure_reason  TEXT,
    failure_kind    TEXT,
    notes           TEXT
);
CREATE INDEX jobs_claim ON jobs(status, lane, id);
CREATE INDEX jobs_dedupe ON jobs(dedupe_key) WHERE dedupe_key IS NOT NULL;
CREATE INDEX jobs_settled ON jobs(settled_at_ms) WHERE settled_at_ms IS NOT NULL;

CREATE TABLE service_health (
    service               TEXT PRIMARY KEY,
    state                 TEXT NOT NULL,
    failure_score         INTEGER NOT NULL,
    open_count            INTEGER NOT NULL,
    last_kind             TEXT,
    last_reason           TEXT,
    opened_at_ms          INTEGER,
    retry_at_ms           INTEGER,
    probe_acquired_at_ms  INTEGER
);

CREATE TABLE shadow_writes (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    op           TEXT NOT NULL,
    path         TEXT NOT NULL,
    before_hash  TEXT,
    after_hash   TEXT,
    job_id       TEXT,
    at_ms        INTEGER NOT NULL
);
CREATE INDEX shadow_writes_at ON shadow_writes(at_ms);
",
    // v2 (WP-9): per-step checkpoints (audit E2) and schedule last-run bookkeeping.
    r"
CREATE TABLE job_step_checkpoints (
    job_id      TEXT NOT NULL,
    step_index  INTEGER NOT NULL,
    outcome     TEXT NOT NULL,
    at_ms       INTEGER NOT NULL,
    PRIMARY KEY (job_id, step_index)
);

CREATE TABLE schedule_runs (
    action_id    TEXT PRIMARY KEY,
    last_run_ms  INTEGER NOT NULL
);
",
    // v3 (WP-13): image descriptions the search feed folds into chunk content and contentHash
    // (Crucible `imageDescriptionStore.ts`; key = content md5; raw = the full record JSON).
    r"
CREATE TABLE image_descriptions (
    md5               TEXT PRIMARY KEY,
    kind              TEXT NOT NULL,
    narrative         TEXT NOT NULL,
    extraction        TEXT NOT NULL,
    description_hash  TEXT NOT NULL,
    described_at      TEXT NOT NULL,
    raw               TEXT NOT NULL
);
",
    // v4 (plan cut-over-now WP-1): the single-row cut-over marker (its writer, `cv cutover mark`, was retired in WP-R5).
    r"
CREATE TABLE cutover_marker (
    id             INTEGER PRIMARY KEY CHECK (id = 1),
    vault          TEXT NOT NULL,
    created_at_ms  INTEGER NOT NULL,
    created_by     TEXT NOT NULL
);
",
    // v5 (plan v0-automation-pipelines WP-1): every A2 table.
    // - link_records: Crucible's columns verbatim (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/db/
    //   sqlite.ts:147-166`; `source_notes` JSON array of `[[path]]`, `tracked_source` JSON scalar),
    //   plus a `yt_video_id` index next to Crucible's `x_status_id` one.
    // - tracker_items: one row per tracked item ever seen (outcome line|note|ignored|skipped);
    //   `full_body` is the per-item automatable evidence (1/0, NULL = not a blog / unknown) the
    //   blog tracker reads over a source's last N rows by `first_seen_ms`.
    // - ignored_ids: ids the trackers must never surface (Ignore Shorts, user ignores).
    // - api_usage: quota units per service per local day.
    r"
CREATE TABLE link_records (
    canonical_url        TEXT PRIMARY KEY,
    url                  TEXT NOT NULL,
    domain               TEXT NOT NULL DEFAULT '',
    source_notes         TEXT NOT NULL DEFAULT '[]',
    first_seen           TEXT,
    last_seen            TEXT,
    state                TEXT NOT NULL DEFAULT 'pending',
    discovery_method     TEXT NOT NULL DEFAULT 'scan',
    tracked_source       TEXT NOT NULL DEFAULT 'false',
    tracked_source_type  TEXT,
    tracked_source_note  TEXT,
    referred_material    TEXT,
    decision_reason      TEXT,
    yt_video_id          TEXT,
    x_status_id          TEXT
);
CREATE INDEX link_records_x_status_id ON link_records(x_status_id);
CREATE INDEX link_records_yt_video_id ON link_records(yt_video_id);

CREATE TABLE tracker_items (
    source_kind    TEXT NOT NULL,
    source_key     TEXT NOT NULL,
    item_id        TEXT NOT NULL,
    title          TEXT NOT NULL DEFAULT '',
    url            TEXT NOT NULL DEFAULT '',
    first_seen_ms  INTEGER NOT NULL,
    day            TEXT NOT NULL,
    outcome        TEXT NOT NULL CHECK (outcome IN ('line', 'note', 'ignored', 'skipped')),
    score          REAL,
    full_body      INTEGER,
    PRIMARY KEY (source_kind, source_key, item_id)
);
CREATE INDEX tracker_items_day ON tracker_items(day);
CREATE INDEX tracker_items_source_seen ON tracker_items(source_kind, source_key, first_seen_ms);

CREATE TABLE ignored_ids (
    kind    TEXT NOT NULL,
    id      TEXT NOT NULL,
    reason  TEXT NOT NULL DEFAULT '',
    at_ms   INTEGER NOT NULL,
    PRIMARY KEY (kind, id)
);

CREATE TABLE api_usage (
    service  TEXT NOT NULL,
    day      TEXT NOT NULL,
    units    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (service, day)
);
",
    // v6 (plan round-5-reading-pickers-intake WP-11): the intake view's done toggle
    // (`done_ms`, NULL = not done) and a captured note's vault-relative path (`note` outcomes).
    r"
ALTER TABLE tracker_items ADD COLUMN done_ms INTEGER;
ALTER TABLE tracker_items ADD COLUMN note_path TEXT;
",
    // v7 (plan intake-view-source-length WP-1): the source's display name at record time and the
    // item length (YouTube duration seconds / blog body word count) for the intake view.
    r"
ALTER TABLE tracker_items ADD COLUMN source_name TEXT;
ALTER TABLE tracker_items ADD COLUMN duration_s INTEGER;
ALTER TABLE tracker_items ADD COLUMN word_count INTEGER;
",
    // v8 (plan round-6-feedback-todos WP-6): the intake item's free-form task status (single
    // char, NULL = open), backfilled from the legacy `done_ms` (kept, no writer).
    r"
ALTER TABLE tracker_items ADD COLUMN status TEXT;
UPDATE tracker_items SET status = 'x' WHERE done_ms IS NOT NULL;
",
    // v9 (plan round-8-intake-views-anchor WP-1): data only — existing ignored rows (Ignore
    // Shorts / ignored ids) become cancelled intakes; a set status is kept.
    r"
UPDATE tracker_items SET status = '-' WHERE outcome = 'ignored' AND status IS NULL;
",
    // v10 (plan tn-review-baseline-remediation WP-R4): durable cancellation — a cancel on a
    // running (claimed) job sets this flag; the runner reads it before each step.
    r"
ALTER TABLE jobs ADD COLUMN cancel_requested INTEGER NOT NULL DEFAULT 0;
",
];

/// Highest schema version this build knows.
#[must_use]
pub fn latest_version() -> i64 {
    i64::try_from(MIGRATIONS.len()).unwrap_or(i64::MAX)
}

/// Current `user_version` of `conn`.
pub fn current_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Apply every pending step. Returns the resulting version.
pub fn migrate(conn: &mut Connection) -> Result<i64> {
    let found = current_version(conn)?;
    let supported = latest_version();
    if found > supported {
        return Err(StateError::NewerSchema { found, supported });
    }
    for (idx, sql) in MIGRATIONS
        .iter()
        .enumerate()
        .skip(usize::try_from(found).unwrap_or(0))
    {
        let target = i64::try_from(idx).unwrap_or(i64::MAX) + 1;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", target)?;
        tx.commit()?;
        tracing::info!(version = target, "state.db migrated");
    }
    current_version(conn)
}
