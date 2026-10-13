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
