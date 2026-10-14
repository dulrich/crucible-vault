//! `index.db` DDL. The index is derived (D14): a schema change bumps [`SCHEMA_VERSION`] and the
//! daemon rebuilds — there are no migrations.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, Result};

/// The `index.db` schema version, carried by the `hello` handshake (`plans/v0-core-daemon.md`
/// Public Interfaces) so a stale client fails loudly.
pub const SCHEMA_VERSION: u32 = 2;

/// Tables per S2 (`FINDINGS.md` "Index design") plus WP-5's additions: `files.ctime`, link
/// line/span, `tasks`. Every child table carries a `file_id` index — S2 measured a 4× full-index
/// slowdown without them (O(n²) per-file deletes).
const DDL: &str = "
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE files(
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,
  folder TEXT NOT NULL,
  name TEXT NOT NULL,
  ext TEXT NOT NULL,
  is_md INTEGER NOT NULL,
  size INTEGER NOT NULL,
  mtime INTEGER NOT NULL,
  ctime INTEGER NOT NULL,
  has_fm INTEGER NOT NULL
);
CREATE INDEX files_folder ON files(folder);
CREATE TABLE props(file_id INTEGER NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL);
CREATE INDEX props_file ON props(file_id);
CREATE INDEX props_key ON props(key);
CREATE TABLE links(
  id INTEGER PRIMARY KEY,
  src_id INTEGER NOT NULL,
  kind TEXT NOT NULL,
  raw TEXT NOT NULL,
  target TEXT NOT NULL,
  fragment TEXT,
  alias TEXT,
  external INTEGER NOT NULL,
  base_key TEXT NOT NULL,
  resolved_id INTEGER,
  line INTEGER,
  span_start INTEGER,
  span_end INTEGER
);
CREATE INDEX links_src ON links(src_id);
CREATE INDEX links_dst ON links(resolved_id);
CREATE INDEX links_base ON links(base_key);
CREATE TABLE tags(file_id INTEGER NOT NULL, tag TEXT NOT NULL, source TEXT NOT NULL, line INTEGER);
CREATE INDEX tags_file ON tags(file_id);
CREATE INDEX tags_tag ON tags(tag);
CREATE TABLE blocks(file_id INTEGER NOT NULL, kind TEXT NOT NULL, label TEXT NOT NULL, line INTEGER NOT NULL);
CREATE INDEX blocks_file ON blocks(file_id);
CREATE TABLE tasks(
  file_id INTEGER NOT NULL,
  line INTEGER NOT NULL,
  status TEXT NOT NULL,
  text TEXT NOT NULL,
  heading TEXT,
  heading_level INTEGER
);
CREATE INDEX tasks_file ON tasks(file_id);
CREATE INDEX tasks_status ON tasks(status);
CREATE VIRTUAL TABLE fts USING fts5(body, content='', contentless_delete=1);
";

/// Switches `conn` to WAL and verifies the returned mode.
pub(crate) fn set_wal(conn: &Connection) -> Result<()> {
    let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(Error::JournalMode(mode));
    }
    Ok(())
}

/// Writer pragmas on a final generation file: WAL (verified), `synchronous=NORMAL`, 5 s busy
/// timeout, default `wal_autocheckpoint`.
pub(crate) fn configure_writer(conn: &Connection) -> Result<()> {
    set_wal(conn)?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(())
}

/// Pragmas for the bulk build into `<final>.building` (rollback journal until the final switch).
pub(crate) fn configure_build(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "DELETE")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

pub(crate) fn create(conn: &Connection, generation: u64) -> Result<()> {
    conn.execute_batch(DDL)?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES('schema_version', ?1), ('generation', ?2)",
        params![SCHEMA_VERSION.to_string(), generation.to_string()],
    )?;
    Ok(())
}

fn meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    let has_meta: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if !has_meta {
        return Ok(None);
    }
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
        .optional()?)
}

/// Fails with [`Error::SchemaMismatch`] unless the DB carries [`SCHEMA_VERSION`].
pub(crate) fn check(conn: &Connection) -> Result<()> {
    let found = meta(conn, "schema_version")?.and_then(|v| v.parse::<u32>().ok());
    if found != Some(SCHEMA_VERSION) {
        return Err(Error::SchemaMismatch {
            found,
            expected: SCHEMA_VERSION,
        });
    }
    Ok(())
}

/// The stored rebuild generation (0 when absent).
pub(crate) fn generation(conn: &Connection) -> Result<u64> {
    Ok(meta(conn, "generation")?
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0))
}
