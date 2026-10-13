//! The writer: atomic rebuild under a generation number, and incremental per-file updates.
//!
//! The daemon's single writer thread owns one [`Index`] (S2 "Recommendation": single writer
//! owning the `Connection`, in-memory resolver in sync, per-file transaction; on create/rename
//! re-resolve, on delete null inbound).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, params};
use walkdir::WalkDir;

use crate::error::{Error, Result, io};
use crate::extract::{self, Extracted, is_parsed_path, is_skipped_path};
use crate::query::Query;
use crate::resolver::{Resolver, file_key, normalize, target_key};
use crate::schema;

/// The writer handle on `index.db`.
pub struct Index {
    conn: Connection,
    db_path: PathBuf,
    resolver: Resolver,
}

/// (folder, name, ext) of a vault path; `ext` lowercased, `name` without the extension.
fn split_path(path: &str) -> (String, String, String) {
    let (folder, file) = path
        .rsplit_once('/')
        .map_or((String::new(), path.to_string()), |(a, b)| {
            (a.to_string(), b.to_string())
        });
    let (name, ext) = match file.rsplit_once('.') {
        Some((a, b)) if !a.is_empty() => (a.to_string(), b.to_lowercase()),
        _ => (file.clone(), String::new()),
    };
    (folder, name, ext)
}

fn check_path(path: &str) -> Result<()> {
    if path.is_empty() || path.contains('\\') || normalize(path) != path {
        return Err(Error::InvalidPath(path.to_string()));
    }
    Ok(())
}

fn ms(t: std::io::Result<SystemTime>) -> Option<i64> {
    t.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
}

fn file_id(conn: &Connection, path: &str) -> Result<Option<i64>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row("SELECT id FROM files WHERE path = ?1", [path], |r| r.get(0))
        .optional()?)
}

fn insert_file(
    tx: &Connection,
    path: &str,
    size: i64,
    mtime: i64,
    ctime: i64,
    has_fm: bool,
) -> Result<i64> {
    let (folder, name, ext) = split_path(path);
    tx.execute(
        "INSERT INTO files(path, folder, name, ext, is_md, size, mtime, ctime, has_fm) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![path, folder, name, ext, is_parsed_path(path), size, mtime, ctime, has_fm],
    )?;
    Ok(tx.last_insert_rowid())
}

fn delete_children(tx: &Connection, id: i64) -> Result<()> {
    for t in ["props", "tags", "blocks", "tasks"] {
        tx.execute(&format!("DELETE FROM {t} WHERE file_id = ?1"), [id])?;
    }
    tx.execute("DELETE FROM links WHERE src_id = ?1", [id])?;
    tx.execute("DELETE FROM fts WHERE rowid = ?1", [id])?;
    Ok(())
}

fn write_children(
    tx: &Connection,
    id: i64,
    path: &str,
    ex: &Extracted,
    resolver: &Resolver,
) -> Result<()> {
    tx.execute(
        "UPDATE files SET has_fm = ?2 WHERE id = ?1",
        params![id, ex.has_fm],
    )?;
    {
        let mut s =
            tx.prepare_cached("INSERT INTO props(file_id, key, value) VALUES(?1, ?2, ?3)")?;
        for (k, v) in &ex.props {
            s.execute(params![id, k, v])?;
        }
    }
    {
        let mut s = tx.prepare_cached(
            "INSERT INTO links(src_id, kind, raw, target, fragment, alias, external, base_key, resolved_id, line, span_start, span_end) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )?;
        for l in &ex.links {
            let (key, resolved) = if l.external {
                (String::new(), None)
            } else {
                (target_key(&l.target), resolver.resolve(path, id, &l.target))
            };
            s.execute(params![
                id,
                l.kind.as_str(),
                l.raw,
                l.target,
                l.fragment,
                l.alias,
                l.external,
                key,
                resolved,
                l.line,
                l.span.map(|s| s.0),
                l.span.map(|s| s.1),
            ])?;
        }
    }
    {
        let mut s = tx.prepare_cached(
            "INSERT INTO tags(file_id, tag, source, line) VALUES(?1, ?2, ?3, ?4)",
        )?;
        for (tag, src, line) in &ex.tags {
            s.execute(params![id, tag, src, line])?;
        }
    }
    {
        let mut s = tx.prepare_cached(
            "INSERT INTO blocks(file_id, kind, label, line) VALUES(?1, ?2, ?3, ?4)",
        )?;
        for (kind, label, line) in &ex.blocks {
            s.execute(params![id, kind, label, line])?;
        }
    }
    {
        let mut s = tx.prepare_cached(
            "INSERT INTO tasks(file_id, line, status, text, heading, heading_level) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for t in &ex.tasks {
            s.execute(params![
                id,
                t.line,
                t.status.to_string(),
                t.text,
                t.heading.as_ref().map(|h| h.0.as_str()),
                t.heading.as_ref().map(|h| h.1),
            ])?;
        }
    }
    tx.execute(
        "INSERT INTO fts(rowid, body) VALUES(?1, ?2)",
        params![id, ex.body],
    )?;
    Ok(())
}

/// Parses `bytes` when `path` is a note; invalid UTF-8 is recorded as a file without content.
fn extract_bytes(path: &str, bytes: &[u8]) -> Option<Extracted> {
    if !is_parsed_path(path) {
        return None;
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => Some(extract::extract(s)),
        Err(e) => {
            tracing::warn!(error = %e, "note is not UTF-8; indexed without content");
            None
        }
    }
}

/// Re-resolves every non-external link matching `filter` (an SQL predicate over `l`/`s`).
fn reresolve(
    tx: &Connection,
    resolver: &Resolver,
    filter: &str,
    p: impl rusqlite::Params,
) -> Result<usize> {
    let rows: Vec<(i64, String, i64, String, Option<i64>)> = {
        let mut s = tx.prepare(&format!(
            "SELECT l.id, s.path, l.src_id, l.target, l.resolved_id FROM links l JOIN files s ON s.id = l.src_id WHERE l.external = 0 AND ({filter})"
        ))?;
        let it = s.query_map(p, |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        it.collect::<rusqlite::Result<_>>()?
    };
    let mut changed = 0;
    let mut up = tx.prepare_cached("UPDATE links SET resolved_id = ?2 WHERE id = ?1")?;
    for (lid, src_path, src_id, target, old) in rows {
        let new = resolver.resolve(&src_path, src_id, &target);
        if new != old {
            up.execute(params![lid, new])?;
            changed += 1;
        }
    }
    Ok(changed)
}

fn load_resolver(conn: &Connection) -> Result<Resolver> {
    let mut r = Resolver::default();
    let mut s = conn.prepare("SELECT id, path FROM files")?;
    let rows = s.query_map([], |x| Ok((x.get::<_, i64>(0)?, x.get::<_, String>(1)?)))?;
    for row in rows {
        let (id, p) = row?;
        r.add(id, &p);
    }
    Ok(r)
}

/// The generation stored in an existing DB at `db_path` (0 if absent or unreadable).
fn previous_generation(db_path: &Path) -> u64 {
    if !db_path.exists() {
        return 0;
    }
    Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .ok()
        .and_then(|c| schema::generation(&c).ok())
        .unwrap_or(0)
}

/// Vault-relative `/` path of `entry`, or `None` (root itself, non-UTF-8).
fn rel_path(root: &Path, p: &Path) -> Option<String> {
    let rel = p.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    let s = parts?.join("/");
    (!s.is_empty()).then_some(s)
}

fn open_writer(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)?;
    schema::configure_writer(&conn)?;
    Ok(conn)
}

/// Builds a complete index of `vault_root` at `tmp`, returning nothing; one transaction.
fn build(vault_root: &Path, tmp: &Path, generation: u64) -> Result<()> {
    let mut conn = open_writer(tmp)?;
    schema::create(&conn, generation)?;
    let mut entries: Vec<(String, PathBuf, std::fs::Metadata)> = Vec::new();
    let walker = WalkDir::new(vault_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            e.depth() != 1 || e.file_name().to_str().is_none_or(|n| !is_skipped_path(n))
        });
    for entry in walker {
        let entry = entry.map_err(|e| Error::Io {
            path: e.path().map_or_else(
                || vault_root.display().to_string(),
                |p| p.display().to_string(),
            ),
            source: e.into(),
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(rel) = rel_path(vault_root, entry.path()) else {
            tracing::warn!("skipping a non-UTF-8 vault path");
            continue;
        };
        let md = entry.metadata().map_err(|e| Error::Io {
            path: entry.path().display().to_string(),
            source: e.into(),
        })?;
        entries.push((rel, entry.path().to_path_buf(), md));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let tx = conn.transaction()?;
    let mut resolver = Resolver::default();
    let mut ids = Vec::with_capacity(entries.len());
    // Pass 1: every file is a link target before any link resolves.
    for (rel, _, md) in &entries {
        let mtime = ms(md.modified()).unwrap_or(0);
        let ctime = ms(md.created()).unwrap_or(mtime);
        let size = i64::try_from(md.len()).unwrap_or(i64::MAX);
        let id = insert_file(&tx, rel, size, mtime, ctime, false)?;
        resolver.add(id, rel);
        ids.push(id);
    }
    // Pass 2: parse notes.
    for ((rel, full, _), id) in entries.iter().zip(&ids) {
        if !is_parsed_path(rel) {
            continue;
        }
        let bytes = std::fs::read(full).map_err(|e| io(full, e))?;
        if let Some(ex) = extract_bytes(rel, &bytes) {
            write_children(&tx, *id, rel, &ex, &resolver)?;
        }
    }
    tx.commit()?;
    conn.close().map_err(|(_, e)| Error::Sqlite(e))?;
    Ok(())
}

fn tmp_path(db_path: &Path, generation: u64) -> PathBuf {
    let mut name = db_path
        .file_name()
        .map_or_else(|| "index.db".into(), std::ffi::OsStr::to_os_string);
    name.push(format!(".tmp-{generation}"));
    db_path.with_file_name(name)
}

fn remove_if_exists(p: &Path) -> Result<()> {
    match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io(p, e)),
    }
}

impl Index {
    /// Opens an existing `index.db` for writing. A schema mismatch is an error: the caller
    /// rebuilds (D14 — derived, never migrated).
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = open_writer(db_path)?;
        schema::check(&conn)?;
        let resolver = load_resolver(&conn)?;
        Ok(Self {
            conn,
            db_path: db_path.to_path_buf(),
            resolver,
        })
    }

    /// Builds a fresh index of `vault_root` and swaps it in at `db_path` (see
    /// [`Index::rebuild`]); the generation continues from any DB already there.
    pub fn create(vault_root: &Path, db_path: &Path) -> Result<Self> {
        let generation = Self::build_and_swap(vault_root, db_path)?;
        let index = Self::open(db_path)?;
        debug_assert_eq!(index.generation().ok(), Some(generation));
        Ok(index)
    }

    /// Writes `index.db.tmp-<gen>` beside the live file in one transaction, then `rename`s it
    /// over the live path. Readers holding the old file keep a consistent old generation until
    /// they reopen; a reopen sees only the complete new file. Returns the new generation.
    ///
    /// The DB uses a rollback journal rather than WAL: a rename swap under WAL could leave the
    /// old file's `-wal` beside the new one.
    pub fn rebuild(&mut self, vault_root: &Path) -> Result<u64> {
        let generation = Self::build_and_swap(vault_root, &self.db_path)?;
        let fresh = Self::open(&self.db_path)?;
        let old = std::mem::replace(self, fresh);
        drop(old);
        Ok(generation)
    }

    fn build_and_swap(vault_root: &Path, db_path: &Path) -> Result<u64> {
        let generation = previous_generation(db_path).saturating_add(1);
        let tmp = tmp_path(db_path, generation);
        remove_if_exists(&tmp)?;
        let mut journal = tmp.clone().into_os_string();
        journal.push("-journal");
        remove_if_exists(Path::new(&journal))?;
        if let Err(e) = build(vault_root, &tmp, generation) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        std::fs::rename(&tmp, db_path).map_err(|e| io(db_path, e))?;
        tracing::info!(generation, "index rebuilt");
        Ok(generation)
    }

    /// The live DB path.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// The rebuild generation.
    pub fn generation(&self) -> Result<u64> {
        schema::generation(&self.conn)
    }

    /// The in-memory resolver (in sync with `files`).
    pub fn resolver(&self) -> &Resolver {
        &self.resolver
    }

    /// The query API over the writer's connection.
    pub fn query(&self) -> Query<'_> {
        Query::new(&self.conn)
    }

    /// Indexes `path` with content `bytes` (one transaction). A new file re-resolves every link
    /// whose basename could now land on it; a modified file keeps its id and rewrites its rows.
    /// Returns the file id, or `None` for a skipped path (`.obsidian/`, `.git/`, `.trash/`).
    pub fn upsert_file(
        &mut self,
        path: &str,
        bytes: &[u8],
        mtime: i64,
        ctime: i64,
    ) -> Result<Option<i64>> {
        check_path(path)?;
        if is_skipped_path(path) {
            return Ok(None);
        }
        let ex = extract_bytes(path, bytes);
        let size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
        let tx = self.conn.transaction()?;
        let existing = file_id(&tx, path)?;
        let id = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE files SET size = ?2, mtime = ?3, ctime = ?4, has_fm = 0 WHERE id = ?1",
                    params![id, size, mtime, ctime],
                )?;
                delete_children(&tx, id)?;
                id
            }
            None => {
                let id = insert_file(&tx, path, size, mtime, ctime, false)?;
                self.resolver.add(id, path);
                id
            }
        };
        let res = (|| -> Result<()> {
            if let Some(ex) = &ex {
                write_children(&tx, id, path, ex, &self.resolver)?;
            }
            if existing.is_none() {
                reresolve(&tx, &self.resolver, "l.base_key = ?1", [file_key(path)])?;
            }
            Ok(())
        })();
        match res.and_then(|()| tx.commit().map_err(Error::from)) {
            Ok(()) => Ok(Some(id)),
            Err(e) => {
                if existing.is_none() {
                    self.resolver.remove(id, path);
                }
                Err(e)
            }
        }
    }

    /// Drops `path` and its rows; inbound links re-resolve (to another candidate, else NULL).
    /// Returns whether it was indexed.
    pub fn remove_file(&mut self, path: &str) -> Result<bool> {
        check_path(path)?;
        let tx = self.conn.transaction()?;
        let Some(id) = file_id(&tx, path)? else {
            return Ok(false);
        };
        delete_children(&tx, id)?;
        tx.execute("DELETE FROM files WHERE id = ?1", [id])?;
        self.resolver.remove(id, path);
        let res = reresolve(
            &tx,
            &self.resolver,
            "l.resolved_id = ?1 OR l.base_key = ?2",
            params![id, file_key(path)],
        )
        .and_then(|_| tx.commit().map_err(Error::from));
        if let Err(e) = res {
            self.resolver.add(id, path);
            return Err(e);
        }
        Ok(true)
    }

    /// Moves `from` to `to`, keeping the file id. Inbound links, links whose basename matches
    /// either name, and the file's own (folder-relative) outgoing links re-resolve. If the move
    /// changes whether the file is a parsed note, its content rows are dropped and the caller
    /// re-indexes it with [`Index::upsert_file`].
    pub fn rename_file(&mut self, from: &str, to: &str) -> Result<i64> {
        check_path(from)?;
        check_path(to)?;
        if is_skipped_path(to) {
            return Err(Error::InvalidPath(to.to_string()));
        }
        let tx = self.conn.transaction()?;
        let id = file_id(&tx, from)?.ok_or_else(|| Error::NotFound(from.to_string()))?;
        if file_id(&tx, to)?.is_some() {
            return Err(Error::AlreadyExists(to.to_string()));
        }
        let (folder, name, ext) = split_path(to);
        tx.execute(
            "UPDATE files SET path = ?2, folder = ?3, name = ?4, ext = ?5, is_md = ?6 WHERE id = ?1",
            params![id, to, folder, name, ext, is_parsed_path(to)],
        )?;
        if is_parsed_path(from) != is_parsed_path(to) {
            delete_children(&tx, id)?;
            tx.execute("UPDATE files SET has_fm = 0 WHERE id = ?1", [id])?;
        }
        self.resolver.remove(id, from);
        self.resolver.add(id, to);
        let res = reresolve(
            &tx,
            &self.resolver,
            "l.resolved_id = ?1 OR l.src_id = ?1 OR l.base_key IN (?2, ?3)",
            params![id, file_key(from), file_key(to)],
        )
        .and_then(|_| tx.commit().map_err(Error::from));
        if let Err(e) = res {
            self.resolver.remove(id, to);
            self.resolver.add(id, from);
            return Err(e);
        }
        Ok(id)
    }
}
