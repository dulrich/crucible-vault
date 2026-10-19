//! Forward-only, versioned migrations for `state.db`, from the 0.1.0 baseline (D23).
//!
//! The schema is keyed on SQLite's `PRAGMA user_version`. A fresh database (version 0) is
//! created directly at [`BASELINE_VERSION`] from [`BASELINE`], the schema the retired v1→v10
//! chain produced (fixture `tests/fixtures/baseline-v10-schema.txt`), then brought forward by
//! [`MIGRATIONS`]. A database at [`latest_version`] opens with no schema write. Versions
//! `1..BASELINE_VERSION` predate the baseline and are refused with [`StateError::PreBaseline`]; a version newer than this build knows is
//! refused with [`StateError::NewerSchema`]. state.db is primary data (D14), so there is no
//! down-migration and no "drop and rebuild" path. Later schema changes append to
//! [`MIGRATIONS`]: entry `i` moves the database from `BASELINE_VERSION + i` to
//! `BASELINE_VERSION + i + 1`, each inside its own transaction.
//!
//! Crucible's store self-migrates via a meta-table version cookie
//! (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/AGENTS.md:55`, `db/SqliteJobStore.ts`); we use
//! `user_version` instead, which needs no table. Nothing Obsidian-induced is involved.

use crate::error::{Result, StateError};
use rusqlite::Connection;

/// `user_version` of the 0.1.0 baseline schema.
pub const BASELINE_VERSION: i64 = 10;

/// The 0.1.0 baseline schema (state.db v10). Byte-for-byte the `sqlite_master` the v1→v10
/// chain produced, including the column order its `ALTER TABLE … ADD COLUMN` steps left.
pub const BASELINE: &str = r"
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
, cancel_requested INTEGER NOT NULL DEFAULT 0);
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
CREATE TABLE image_descriptions (
    md5               TEXT PRIMARY KEY,
    kind              TEXT NOT NULL,
    narrative         TEXT NOT NULL,
    extraction        TEXT NOT NULL,
    description_hash  TEXT NOT NULL,
    described_at      TEXT NOT NULL,
    raw               TEXT NOT NULL
);
CREATE TABLE cutover_marker (
    id             INTEGER PRIMARY KEY CHECK (id = 1),
    vault          TEXT NOT NULL,
    created_at_ms  INTEGER NOT NULL,
    created_by     TEXT NOT NULL
);
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
    full_body      INTEGER, done_ms INTEGER, note_path TEXT, source_name TEXT, duration_s INTEGER, word_count INTEGER, status TEXT,
    PRIMARY KEY (source_kind, source_key, item_id)
);
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
CREATE INDEX jobs_claim ON jobs(status, lane, id);
CREATE INDEX jobs_dedupe ON jobs(dedupe_key) WHERE dedupe_key IS NOT NULL;
CREATE INDEX jobs_settled ON jobs(settled_at_ms) WHERE settled_at_ms IS NOT NULL;
CREATE INDEX link_records_x_status_id ON link_records(x_status_id);
CREATE INDEX link_records_yt_video_id ON link_records(yt_video_id);
CREATE INDEX shadow_writes_at ON shadow_writes(at_ms);
CREATE INDEX tracker_items_day ON tracker_items(day);
CREATE INDEX tracker_items_source_seen ON tracker_items(source_kind, source_key, first_seen_ms);
";

/// Post-baseline steps (v11+). Later WPs append (never edit) entries.
pub const MIGRATIONS: &[&str] = &[
    // v10 -> v11 (D24): drop the retired shadow-write log, the cut-over marker and the unused
    // `tracker_items.done_ms` column. Nothing else is touched.
    "DROP INDEX IF EXISTS shadow_writes_at;
     DROP TABLE IF EXISTS shadow_writes;
     DROP TABLE IF EXISTS cutover_marker;
     ALTER TABLE tracker_items DROP COLUMN done_ms;",
];

/// Highest schema version this build knows.
#[must_use]
pub fn latest_version() -> i64 {
    BASELINE_VERSION + i64::try_from(MIGRATIONS.len()).unwrap_or(i64::MAX - BASELINE_VERSION)
}

/// Current `user_version` of `conn`.
pub fn current_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Refuse a version this build cannot open: pre-baseline (`1..BASELINE_VERSION`) or newer than
/// [`latest_version`]. Version 0 (fresh) passes.
pub fn check_version(found: i64) -> Result<()> {
    let supported = latest_version();
    if found > supported {
        return Err(StateError::NewerSchema { found, supported });
    }
    if (1..BASELINE_VERSION).contains(&found) {
        return Err(StateError::PreBaseline {
            found,
            baseline: BASELINE_VERSION,
        });
    }
    Ok(())
}

/// Create the baseline on a fresh database, then apply every pending post-baseline step.
/// Returns the resulting version. A database already at [`latest_version`] is not written.
pub fn migrate(conn: &mut Connection) -> Result<i64> {
    let found = current_version(conn)?;
    check_version(found)?;
    if found == 0 {
        let tx = conn.transaction()?;
        tx.execute_batch(BASELINE)?;
        tx.pragma_update(None, "user_version", BASELINE_VERSION)?;
        tx.commit()?;
        tracing::info!(version = BASELINE_VERSION, "state.db created at baseline");
    }
    let applied = current_version(conn)? - BASELINE_VERSION;
    for (idx, sql) in MIGRATIONS
        .iter()
        .enumerate()
        .skip(usize::try_from(applied).unwrap_or(0))
    {
        let target =
            BASELINE_VERSION + i64::try_from(idx).unwrap_or(i64::MAX - BASELINE_VERSION) + 1;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", target)?;
        tx.commit()?;
        tracing::info!(version = target, "state.db migrated");
    }
    current_version(conn)
}
