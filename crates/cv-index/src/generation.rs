//! Per-generation file layout. `CV_INDEX_DB` is a *base* path, never itself a DB: generation `n`
//! lives at `<dir>/<stem>-<n>.<ext>` with its own `-wal`/`-shm`. Nothing is ever renamed over, or
//! unlinked from under, a WAL database's path while its `-wal`/`-shm` could pair with a different
//! main file: a new generation is built under `<final>.building` in rollback-journal mode, switched
//! to WAL and closed (no `-wal` left), then renamed to a final name that has never existed.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::error::{Error, Result, io};
use crate::schema;

/// `(dir, stem, ext)` of a base path; `ext` empty when the base has none.
fn parts(base: &Path) -> (PathBuf, String, String) {
    let dir = match base.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let stem = base
        .file_stem()
        .map_or_else(|| "index".to_string(), |s| s.to_string_lossy().into_owned());
    let ext = base
        .extension()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    (dir, stem, ext)
}

/// The file of generation `generation` for `base` (`index.db` → `index-7.db`; no extension →
/// `<name>-<n>`).
pub fn generation_path(base: &Path, generation: u64) -> PathBuf {
    let (dir, stem, ext) = parts(base);
    if ext.is_empty() {
        dir.join(format!("{stem}-{generation}"))
    } else {
        dir.join(format!("{stem}-{generation}.{ext}"))
    }
}

/// `path` with `suffix` appended to its file name (`.building`, `-wal`, …).
pub(crate) fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s: OsString = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// One generation-shaped entry of the base directory.
pub(crate) struct Entry {
    pub generation: u64,
    /// Text after the final name: empty for the main file, else `.building`, `-wal`, …
    pub suffix: String,
    pub path: PathBuf,
}

/// Parses `name` as `<stem>-<n>[.<ext>]<suffix>` with a canonical decimal `n` and a suffix we
/// own (none, `.building*`, `-wal`, `-shm`, `-journal`); anything else is not ours to touch.
fn parse(name: &str, stem: &str, ext: &str) -> Option<(u64, String)> {
    let rest = name.strip_prefix(stem)?.strip_prefix('-')?;
    let digits_len = rest.bytes().take_while(u8::is_ascii_digit).count();
    let (digits, rest) = rest.split_at(digits_len);
    if digits.is_empty() || (digits.len() > 1 && digits.starts_with('0')) {
        return None;
    }
    let n = digits.parse::<u64>().ok()?;
    let suffix = if ext.is_empty() {
        rest
    } else {
        rest.strip_prefix('.')?.strip_prefix(ext)?
    };
    let known = suffix.is_empty()
        || suffix.starts_with(".building")
        || matches!(suffix, "-wal" | "-shm" | "-journal");
    known.then(|| (n, suffix.to_string()))
}

/// Every generation-shaped file beside `base` (a missing directory lists as empty).
pub(crate) fn scan(base: &Path) -> Result<Vec<Entry>> {
    let (dir, stem, ext) = parts(base);
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(&dir, e)),
    };
    let mut out = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|e| io(&dir, e))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some((generation, suffix)) = parse(name, &stem, &ext) {
            out.push(Entry {
                generation,
                suffix,
                path: entry.path(),
            });
        }
    }
    Ok(out)
}

/// Main-file generation numbers present on disk, descending.
pub(crate) fn finals(entries: &[Entry]) -> Vec<u64> {
    let set: BTreeSet<u64> = entries
        .iter()
        .filter(|e| e.suffix.is_empty())
        .map(|e| e.generation)
        .collect();
    set.into_iter().rev().collect()
}

/// Opens generation `generation` read-only and validates it: schema version, stored generation
/// equal to the file's number. Never creates the file.
pub(crate) fn open_read_only(base: &Path, generation: u64) -> Result<Connection> {
    let path = generation_path(base, generation);
    if !path.is_file() {
        return Err(Error::GenerationMissing {
            generation,
            path: path.display().to_string(),
        });
    }
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    validate(&conn, generation)?;
    Ok(conn)
}

/// Schema check plus "stored generation == file number".
pub(crate) fn validate(conn: &Connection, generation: u64) -> Result<()> {
    schema::check(conn)?;
    let stored = schema::generation(conn)?;
    if stored != generation {
        return Err(Error::GenerationMismatch {
            file: generation,
            stored,
        });
    }
    Ok(())
}

/// The highest generation whose final file exists and opens with a matching schema (and stored
/// generation); `.building` files and unparsable names are skipped.
pub fn current_generation(base: &Path) -> Result<Option<u64>> {
    Ok(current_in(base, &scan(base)?))
}

pub(crate) fn current_in(base: &Path, entries: &[Entry]) -> Option<u64> {
    finals(entries).into_iter().find(|&g| {
        open_read_only(base, g)
            .map_err(|e| tracing::warn!(generation = g, error = %e, "skipping index generation"))
            .is_ok()
    })
}

/// Best-effort unlink: NotFound is fine, anything else warns.
pub(crate) fn remove_best_effort(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(path = %path.display(), error = %e, "index cleanup failed"),
    }
}

/// Removes, best-effort: every `.building*` file, every file of a generation below `max(keep)`
/// that is not in `keep`, and every `-wal`/`-shm`/`-journal` whose main file is absent.
/// Generations above every kept one (e.g. a newer schema's) are left alone apart from orphans.
pub(crate) fn sweep(base: &Path, keep: &[u64]) -> Result<()> {
    let entries = scan(base)?;
    let present: BTreeSet<u64> = finals(&entries).into_iter().collect();
    let top = keep.iter().copied().max();
    for e in &entries {
        let building = e.suffix.starts_with(".building");
        let retired = top.is_some_and(|t| e.generation < t) && !keep.contains(&e.generation);
        let orphan = !e.suffix.is_empty() && !present.contains(&e.generation);
        if building || retired || orphan {
            remove_best_effort(&e.path);
        }
    }
    Ok(())
}

/// The next generation number: one past every generation-shaped file on disk.
pub(crate) fn next(entries: &[Entry]) -> u64 {
    entries
        .iter()
        .map(|e| e.generation)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

/// fsyncs `dir` (best-effort: warns on failure).
pub(crate) fn sync_dir(dir: &Path) {
    match std::fs::File::open(dir).and_then(|f| f.sync_all()) {
        Ok(()) => {}
        Err(e) => tracing::warn!(dir = %dir.display(), error = %e, "directory fsync failed"),
    }
}

/// The directory holding `base`'s generations.
pub(crate) fn dir(base: &Path) -> PathBuf {
    parts(base).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_parsing() {
        assert_eq!(
            generation_path(Path::new("/x/index.db"), 7),
            PathBuf::from("/x/index-7.db")
        );
        assert_eq!(
            generation_path(Path::new("/x/index"), 7),
            PathBuf::from("/x/index-7")
        );
        assert_eq!(parse("index-7.db", "index", "db"), Some((7, String::new())));
        assert_eq!(
            parse("index-9.db.building", "index", "db"),
            Some((9, ".building".into()))
        );
        assert_eq!(
            parse("index-8.db-wal", "index", "db"),
            Some((8, "-wal".into()))
        );
        assert_eq!(parse("index-8-wal", "index", ""), Some((8, "-wal".into())));
        assert_eq!(parse("index-07.db", "index", "db"), None);
        assert_eq!(parse("index-x.db", "index", "db"), None);
        assert_eq!(parse("index.db", "index", "db"), None);
        assert_eq!(parse("index-7.dbx", "index", "db"), None);
    }
}
