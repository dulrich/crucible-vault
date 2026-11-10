//! Forward-only, versioned migrations for `state.db`, from the 1.0.0 baseline (D31).
//!
//! Per-major baseline rule: structural migrations collapse into one baseline per major version;
//! within a major, schema changes are ladder steps appended to [`MIGRATIONS`].
//!
//! The schema is keyed on SQLite's `PRAGMA user_version`. A fresh database (version 0) is
//! created directly at [`BASELINE_VERSION`] (17) from [`BASELINE`], the schema the retired v15
//! baseline plus its v16 and v17 steps produced (fixture `tests/fixtures/baseline-v17-schema.txt`),
//! then brought forward by [`MIGRATIONS`] (empty at 1.0.0). A database at [`latest_version`]
//! opens with no schema write. Versions `1..BASELINE_VERSION` predate the baseline and are refused
//! with [`StateError::PreBaseline`]; a version newer than this build knows is refused with
//! [`StateError::NewerSchema`]. state.db is primary data (D14), so there is no down-migration and
//! no "drop and rebuild" path. Later schema changes append to [`MIGRATIONS`]: entry `i` moves the
//! database from `BASELINE_VERSION + i` to `BASELINE_VERSION + i + 1`, each inside its own
//! transaction.
//!
//! Crucible's store self-migrates via a meta-table version cookie
//! (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/AGENTS.md:55`, `db/SqliteJobStore.ts`); we use
//! `user_version` instead, which needs no table. Nothing Obsidian-induced is involved.

use crate::error::{Result, StateError};
use rusqlite::Connection;

/// `user_version` of the 1.0.0 baseline schema.
pub const BASELINE_VERSION: i64 = 17;

/// The 1.0.0 baseline schema (state.db v17). Byte-for-byte the `sqlite_master` the retired v15
/// baseline plus its v16 (job telemetry) and v17 (path lineage) steps produced, including the
/// column order their `ALTER TABLE … ADD COLUMN` steps left (fixture
/// `tests/fixtures/baseline-v17-schema.txt`).
pub const BASELINE: &str = r"
CREATE TABLE api_usage (
    service  TEXT NOT NULL,
    day      TEXT NOT NULL,
    units    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (service, day)
);
CREATE TABLE dup_labels (
         id                         INTEGER PRIMARY KEY,
         keep_path                  TEXT NOT NULL,
         other_path                 TEXT NOT NULL,
         keep_identity              TEXT,
         other_identity             TEXT,
         containment_keep_in_other  REAL NOT NULL,
         containment_other_in_keep  REAL NOT NULL,
         identity_match             INTEGER NOT NULL,
         verdict                    TEXT NOT NULL CHECK (verdict IN ('not-duplicates', 'keep')),
         at_ms                      INTEGER NOT NULL
     );
CREATE TABLE ignored_ids (
    kind    TEXT NOT NULL,
    id      TEXT NOT NULL,
    reason  TEXT NOT NULL DEFAULT '',
    at_ms   INTEGER NOT NULL,
    PRIMARY KEY (kind, id)
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
CREATE TABLE job_step_checkpoints (
    job_id      TEXT NOT NULL,
    step_index  INTEGER NOT NULL,
    outcome     TEXT NOT NULL,
    at_ms       INTEGER NOT NULL,
    PRIMARY KEY (job_id, step_index)
);
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
, cancel_requested INTEGER NOT NULL DEFAULT 0, tel_attempt INTEGER NOT NULL DEFAULT 0, first_claimed_ms INTEGER, last_claimed_ms INTEGER, tel_partial INTEGER NOT NULL DEFAULT 0);
CREATE TABLE job_attempt_times (
    job_id      TEXT NOT NULL,
    attempt     INTEGER NOT NULL,
    claimed_ms  INTEGER NOT NULL,
    settled_ms  INTEGER,
    PRIMARY KEY (job_id, attempt)
);
CREATE TABLE job_wait_events (
    seq       INTEGER PRIMARY KEY,
    job_id    TEXT NOT NULL,
    at_ms     INTEGER NOT NULL,
    cause     TEXT NOT NULL,
    blocker   TEXT,
    until_ms  INTEGER
);
CREATE TABLE job_step_times (
    job_id      TEXT NOT NULL,
    attempt     INTEGER NOT NULL,
    step_index  INTEGER NOT NULL,
    step_kind   TEXT NOT NULL,
    start_ms    INTEGER NOT NULL,
    end_ms      INTEGER,
    PRIMARY KEY (job_id, attempt, step_index)
);
CREATE TABLE path_lineage (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    kind       TEXT NOT NULL CHECK (kind IN ('created', 'renamed', 'replaced', 'deleted')),
    from_path  TEXT NOT NULL,
    to_path    TEXT,
    at_ms      INTEGER NOT NULL
);
CREATE TABLE path_lineage_floor (
    id      INTEGER PRIMARY KEY CHECK (id = 1),
    pruned  INTEGER NOT NULL
);
INSERT INTO path_lineage_floor (id, pruned) VALUES (1, 0);
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
CREATE TABLE mail_boilerplate (
         source_key  TEXT NOT NULL,
         line_key    INTEGER NOT NULL,
         count       INTEGER NOT NULL,
         PRIMARY KEY (source_key, line_key)
     );
CREATE TABLE mail_boilerplate_seen (
         source_key  TEXT NOT NULL,
         message_id  TEXT NOT NULL,
         PRIMARY KEY (source_key, message_id)
     );
CREATE TABLE schedule_runs (
    action_id    TEXT PRIMARY KEY,
    last_run_ms  INTEGER NOT NULL
);
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
    full_body      INTEGER, note_path TEXT, source_name TEXT, duration_s INTEGER, word_count INTEGER, status TEXT, access TEXT, complete INTEGER, full_word_count INTEGER,
    PRIMARY KEY (source_kind, source_key, item_id)
);
CREATE TABLE trash_entries (
         id             INTEGER PRIMARY KEY,
         location       TEXT NOT NULL CHECK (location IN ('vault', 'system')),
         trash_path     TEXT NOT NULL,
         original_path  TEXT NOT NULL,
         trashed_ms     INTEGER NOT NULL,
         bytes          INTEGER NOT NULL,
         reason         TEXT NOT NULL
             CHECK (reason IN ('deleted', 'replaced', 'note-attachment', 'orphan', 'seeded')),
         mtime_ms       INTEGER,
         inode          INTEGER,
         info_name      TEXT
     );
CREATE INDEX jobs_claim ON jobs(status, lane, id);
CREATE INDEX jobs_dedupe ON jobs(dedupe_key) WHERE dedupe_key IS NOT NULL;
CREATE INDEX jobs_settled ON jobs(settled_at_ms) WHERE settled_at_ms IS NOT NULL;
CREATE INDEX link_records_x_status_id ON link_records(x_status_id);
CREATE INDEX link_records_yt_video_id ON link_records(yt_video_id);
CREATE INDEX tracker_items_day ON tracker_items(day);
CREATE INDEX tracker_items_source_seen ON tracker_items(source_kind, source_key, first_seen_ms);
CREATE INDEX tracker_items_url ON tracker_items(url);
CREATE INDEX trash_entries_path ON trash_entries(location, trash_path);
CREATE INDEX trash_entries_trashed ON trash_entries(trashed_ms);
CREATE INDEX job_wait_events_job ON job_wait_events(job_id, seq);
CREATE INDEX path_lineage_from ON path_lineage(from_path, seq);
CREATE INDEX path_lineage_at ON path_lineage(at_ms);
";

/// Steps after the baseline. Within a major version, schema changes append here (never edit)
/// and entry `i` moves the database from `BASELINE_VERSION + i` to `BASELINE_VERSION + i + 1`;
/// the next major folds them into a new baseline (D31).
pub const MIGRATIONS: &[&str] = &[
    // v18 (plan note-identity WP-1): durable note ids. A live row (`retired_ms IS NULL`) binds an
    // id to the file now at `path`; a tombstone keeps its last path and absorber. AUTOINCREMENT
    // keeps the id high-water mark in `sqlite_sequence`, so pruning never lets an id be reused.
    r"
CREATE TABLE note_ids (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    path         TEXT NOT NULL,
    retired_ms   INTEGER,
    replaced_by  INTEGER
);
CREATE UNIQUE INDEX note_ids_live_path ON note_ids(path) WHERE retired_ms IS NULL;
CREATE INDEX note_ids_retired ON note_ids(retired_ms) WHERE retired_ms IS NOT NULL;
",
    // v19 (plan note-identity WP-6): the path-lineage scheme of plan note-path-continuity (D2-D4)
    // is superseded by note ids and removed; its indexes drop with the tables. The 1.0.0 baseline
    // still creates them (migration history), so a fresh database passes through both.
    r"
DROP TABLE path_lineage;
DROP TABLE path_lineage_floor;
",
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
