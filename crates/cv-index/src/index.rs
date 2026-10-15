//! The writer: atomic rebuild under a generation number, and incremental per-file updates.
//!
//! Layout (see [`crate::generation`]): the configured path is a *base*; generation `n` is its own
//! WAL database at `<stem>-<n>.<ext>`. A rebuild writes `<final>.building`, switches it to WAL,
//! closes it (no `-wal` left) and renames it to a never-before-used final name, so no `-wal`/`-shm`
//! can ever pair with the wrong main file. The writer then reopens on the new file, checkpoints
//! and closes the outgoing one, and retires generations older than the outgoing one; readers
//! holding a retired file keep its inode (POSIX) and reopen on `index_generation`.
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
use crate::generation::{self as gen_files, generation_path, with_suffix};
use crate::ignore::IgnoreRules;
use crate::query::Query;
use crate::resolver::{Resolver, file_key, normalize, target_key};
use crate::schema;

/// The writer handle on the current `index.db` generation.
pub struct Index {
    conn: Connection,
    base: PathBuf,
    generation: u64,
    resolver: Resolver,
    /// The vault's `.gitignore` rules (WP-1, plan editor-ui-round-3); loaded on create/rebuild
    /// and by [`Index::load_ignore`] after [`Index::open`].
    ignore: IgnoreRules,
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

/// Vault-relative `/` path of `entry`, or `None` (root itself, non-UTF-8).
fn rel_path(root: &Path, p: &Path) -> Option<String> {
    let rel = p.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    let s = parts?.join("/");
    (!s.is_empty()).then_some(s)
}

fn open_writer(base: &Path, generation: u64) -> Result<Connection> {
    let path = generation_path(base, generation);
    if !path.is_file() {
        return Err(Error::GenerationMissing {
            generation,
            path: path.display().to_string(),
        });
    }
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    schema::configure_writer(&conn)?;
    gen_files::validate(&conn, generation)?;
    Ok(conn)
}

/// Builds a complete index of `vault_root` at `building` in one transaction, then switches it to
/// WAL and closes it.
fn build(vault_root: &Path, building: &Path, generation: u64, rules: &IgnoreRules) -> Result<()> {
    let mut conn = Connection::open(building)?;
    schema::configure_build(&conn)?;
    schema::create(&conn, generation)?;
    let mut entries: Vec<(String, PathBuf, std::fs::Metadata)> = Vec::new();
    let mut dangling = 0_usize;
    let walker = WalkDir::new(vault_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Prunes excluded directories (never descended) and drops excluded files.
            e.depth() == 0
                || rel_path(vault_root, e.path())
                    .is_none_or(|rel| !rules.is_excluded(&rel, e.file_type().is_dir()))
        });
    for entry in walker {
        let entry = entry.map_err(|e| Error::Io {
            path: e.path().map_or_else(
                || vault_root.display().to_string(),
                |p| p.display().to_string(),
            ),
            source: e.into(),
        })?;
        let ft = entry.file_type();
        let md = if ft.is_file() {
            entry.metadata().map_err(|e| Error::Io {
                path: entry.path().display().to_string(),
                source: e.into(),
            })?
        } else if ft.is_symlink() {
            // A symlink to a regular file is indexed under its vault path with the target's
            // metadata (plan editor-cutover-blockers WP-1); directory links stay unfollowed and a
            // dangling link is skipped and counted.
            match std::fs::metadata(entry.path()) {
                Ok(m) if m.is_file() => m,
                Ok(_) => continue,
                Err(_) => {
                    dangling += 1;
                    continue;
                }
            }
        } else {
            continue;
        };
        let Some(rel) = rel_path(vault_root, entry.path()) else {
            tracing::warn!("skipping a non-UTF-8 vault path");
            continue;
        };
        entries.push((rel, entry.path().to_path_buf(), md));
    }
    if dangling > 0 {
        tracing::info!(dangling, "skipped dangling symlinks while indexing");
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
    schema::set_wal(&conn)?;
    conn.close().map_err(|(_, e)| Error::Sqlite(e))?;
    Ok(())
}

const SIDE_FILES: [&str; 4] = ["", "-journal", "-wal", "-shm"];

fn remove_if_exists(p: &Path) -> Result<()> {
    match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io(p, e)),
    }
}

/// Builds the next generation beside `base` and renames it into place; returns its number.
/// On failure the `.building` files are removed and the current generation stays live.
fn build_generation(vault_root: &Path, base: &Path, rules: &IgnoreRules) -> Result<u64> {
    let generation = gen_files::next(&gen_files::scan(base)?);
    let fin = generation_path(base, generation);
    let building = with_suffix(&fin, ".building");
    for s in SIDE_FILES {
        remove_if_exists(&with_suffix(&building, s))?;
    }
    let res = build(vault_root, &building, generation, rules).and_then(|()| {
        let wal = with_suffix(&building, "-wal");
        if wal.exists() {
            return Err(Error::JournalMode(format!(
                "{} left behind after close",
                wal.display()
            )));
        }
        std::fs::rename(&building, &fin).map_err(|e| io(&fin, e))
    });
    if let Err(e) = res {
        for s in SIDE_FILES {
            gen_files::remove_best_effort(&with_suffix(&building, s));
        }
        return Err(e);
    }
    gen_files::sync_dir(&gen_files::dir(base));
    tracing::info!(generation, "index generation built");
    Ok(generation)
}

impl Index {
    /// Opens the current generation beside `base` for writing (sweeping stale `.building` files,
    /// retired generations and orphan `-wal`/`-shm` first). No valid generation →
    /// [`Error::NoIndex`] (the caller runs [`Index::create`]); when the newest file exists but
    /// fails validation its error ([`Error::SchemaMismatch`], …) is returned instead — rebuild
    /// (D14 — derived, never migrated).
    pub fn open(base: &Path) -> Result<Self> {
        let entries = gen_files::scan(base)?;
        let Some(generation) = gen_files::current_in(base, &entries) else {
            if let Some(&newest) = gen_files::finals(&entries).first() {
                gen_files::open_read_only(base, newest)?;
            }
            return Err(Error::NoIndex(base.display().to_string()));
        };
        let prev = gen_files::finals(&entries)
            .into_iter()
            .find(|&g| g < generation);
        let keep: Vec<u64> = std::iter::once(generation).chain(prev).collect();
        gen_files::sweep(base, &keep)?;
        Self::open_generation(base, generation, IgnoreRules::default())
    }

    fn open_generation(base: &Path, generation: u64, ignore: IgnoreRules) -> Result<Self> {
        let conn = open_writer(base, generation)?;
        let resolver = load_resolver(&conn)?;
        Ok(Self {
            conn,
            base: base.to_path_buf(),
            generation,
            resolver,
            ignore,
        })
    }

    /// Builds the next generation of `vault_root` beside `base` and opens it. The previous valid
    /// generation (if any) is kept one more round for readers still on it; older ones retire.
    pub fn create(vault_root: &Path, base: &Path) -> Result<Self> {
        let entries = gen_files::scan(base)?;
        let prev = gen_files::current_in(base, &entries);
        gen_files::sweep(base, &prev.into_iter().collect::<Vec<_>>())?;
        let rules = IgnoreRules::load(vault_root);
        let generation = build_generation(vault_root, base, &rules)?;
        let index = Self::open_generation(base, generation, rules)?;
        let keep: Vec<u64> = std::iter::once(generation).chain(prev).collect();
        gen_files::sweep(base, &keep)?;
        Ok(index)
    }

    /// Builds generation `g` (one past every generation file on disk), reopens the writer on it,
    /// checkpoints (best-effort) and closes the outgoing generation, then retires every generation
    /// except `g` and the outgoing one. A failed build leaves the current generation live.
    /// Returns `g`.
    pub fn rebuild(&mut self, vault_root: &Path) -> Result<u64> {
        let rules = IgnoreRules::load(vault_root);
        let generation = build_generation(vault_root, &self.base, &rules)?;
        let fresh = Self::open_generation(&self.base, generation, rules)?;
        let old = std::mem::replace(self, fresh);
        let outgoing = old.generation;
        // PASSIVE, never TRUNCATE/RESTART: those invoke the busy handler and would let a reader
        // still on the outgoing generation stall the writer thread (plan :95/:298). The outgoing
        // file keeps its own `-wal` beside it until it retires, so a partial checkpoint is safe.
        if let Err(e) = old
            .conn
            .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(()))
        {
            tracing::warn!(generation = outgoing, error = %e, "outgoing checkpoint failed");
        }
        if let Err((_, e)) = old.conn.close() {
            tracing::warn!(generation = outgoing, error = %e, "closing outgoing generation failed");
        }
        if let Err(e) = gen_files::sweep(&self.base, &[generation, outgoing]) {
            tracing::warn!(error = %e, "retiring old generations failed");
        }
        Ok(generation)
    }

    /// (Re)loads the vault's `.gitignore` rules for incremental updates (call after
    /// [`Index::open`], which has no vault root; create/rebuild load them themselves).
    pub fn load_ignore(&mut self, vault_root: &Path) {
        self.ignore = IgnoreRules::load(vault_root);
    }

    /// `path` is never indexed: a skipped folder, a dot-prefixed component or a `.gitignore`
    /// match (or under an excluded directory).
    pub fn is_excluded(&self, path: &str) -> bool {
        is_skipped_path(path) || self.ignore.is_excluded(path, false)
    }

    /// The current generation's file.
    pub fn db_path(&self) -> PathBuf {
        generation_path(&self.base, self.generation)
    }

    /// The configured base path (what `hello`'s `index_db` carries; never itself a DB).
    pub fn base_path(&self) -> &Path {
        &self.base
    }

    /// The current generation (equal to the file's number and to `meta.generation`).
    pub fn generation(&self) -> Result<u64> {
        Ok(self.generation)
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
    /// Returns the file id, or `None` for an excluded path ([`Index::is_excluded`]; a stale row
    /// for it is dropped).
    pub fn upsert_file(
        &mut self,
        path: &str,
        bytes: &[u8],
        mtime: i64,
        ctime: i64,
    ) -> Result<Option<i64>> {
        check_path(path)?;
        if self.is_excluded(path) {
            self.remove_file(path)?;
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
    /// re-indexes it with [`Index::upsert_file`]. A move to an excluded (ignored) path drops
    /// `from` and returns [`Error::NotFound`], so callers' "not indexed → upsert" path runs and
    /// the upsert is dropped too.
    pub fn rename_file(&mut self, from: &str, to: &str) -> Result<i64> {
        check_path(from)?;
        check_path(to)?;
        if is_skipped_path(to) {
            return Err(Error::InvalidPath(to.to_string()));
        }
        if self.is_excluded(to) {
            self.remove_file(from)?;
            return Err(Error::NotFound(from.to_string()));
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
