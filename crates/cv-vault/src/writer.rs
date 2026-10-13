//! [`VaultWriter`]: the single writer (I2). Owns the [`VaultFs`] and the `index.db` writer handle;
//! every vault mutation goes through it as a content-hash CAS.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cv_index::{Index, LinkRow};

use crate::error::{Result, VaultError};
use crate::fs::{VaultFs, times};
use crate::hash::ContentHash;
use crate::path::validate_rel_path;
use crate::rewrite::rewrite_note;
use crate::watcher::Echoes;

/// What a rename did to inbound links.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenameOutcome {
    /// Source notes whose links were rewritten (post-rename paths), sorted.
    pub rewritten: Vec<String>,
    /// Source notes skipped because they changed since the index saw them (never clobbered).
    pub skipped_conflicts: Vec<String>,
}

impl RenameOutcome {
    fn merge(&mut self, other: RenameOutcome) {
        self.rewritten.extend(other.rewritten);
        self.skipped_conflicts.extend(other.skipped_conflicts);
        self.rewritten.sort();
        self.rewritten.dedup();
        self.skipped_conflicts.sort();
        self.skipped_conflicts.dedup();
    }
}

/// The vault writer; see the module docs. Run it on one thread ([`crate::spawn_writer`]).
pub struct VaultWriter {
    pub(crate) fs: VaultFs,
    pub(crate) index: Index,
    pub(crate) echoes: Echoes,
}

impl std::fmt::Debug for VaultWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultWriter")
            .field("fs", &self.fs)
            .finish_non_exhaustive()
    }
}

/// Default lifetime of a recorded echo.
pub(crate) const DEFAULT_ECHO_TTL: Duration = Duration::from_secs(5);

impl VaultWriter {
    /// A writer over `fs` and the index writer handle `index` (which must index `fs.root()`).
    pub fn new(fs: VaultFs, index: Index) -> Self {
        Self {
            fs,
            index,
            echoes: Echoes::new(DEFAULT_ECHO_TTL),
        }
    }

    /// Opens the index at base path `index_base` for `fs.root()`, creating it when there is none
    /// and rebuilding it on a schema/generation mismatch (D14: derived, never migrated).
    pub fn open(fs: VaultFs, index_base: &Path) -> Result<Self> {
        let index = match Index::open(index_base) {
            Ok(i) => i,
            Err(
                cv_index::Error::NoIndex(_)
                | cv_index::Error::SchemaMismatch { .. }
                | cv_index::Error::GenerationMismatch { .. },
            ) => Index::create(fs.root(), index_base)?,
            Err(e) => return Err(e.into()),
        };
        Ok(Self::new(fs, index))
    }

    /// Echo entries expire after `ttl` (tests use short values).
    pub fn with_echo_ttl(mut self, ttl: Duration) -> Self {
        self.echoes = Echoes::new(ttl);
        self
    }

    /// The filesystem.
    pub fn fs(&self) -> &VaultFs {
        &self.fs
    }

    /// The filesystem, mutably (e.g. [`VaultFs::set_job_id`]).
    pub fn fs_mut(&mut self) -> &mut VaultFs {
        &mut self.fs
    }

    /// The index (writer handle).
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// Rebuilds the index from the live root; on failure reopens the newest generation so the
    /// writer converges (cv-index contract).
    pub fn rebuild(&mut self) -> Result<u64> {
        let root = self.fs.root().to_path_buf();
        match self.index.rebuild(&root) {
            Ok(g) => Ok(g),
            Err(e) => {
                let base = self.index.base_path().to_path_buf();
                match Index::open(&base) {
                    Ok(i) => self.index = i,
                    Err(e2) => tracing::warn!(error = %e2, "reopening index after failed rebuild"),
                }
                Err(e.into())
            }
        }
    }

    /// Reads `path` as the filesystem sees it.
    pub fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        self.fs.read(path)
    }

    /// Indexes a just-written live file and records its echo. An index failure is logged and
    /// the echo is *not* recorded, so the watcher re-applies the change.
    fn index_written(&mut self, path: &str, bytes: &[u8], full: &Path, hash: &ContentHash) {
        if self.fs.is_shadow() {
            return;
        }
        let res = std::fs::metadata(full)
            .map_err(|e| crate::error::io(full, e))
            .and_then(|md| {
                let (m, c) = times(&md);
                Ok(self.index.upsert_file(path, bytes, m, c)?)
            });
        match res {
            Ok(_) => self.echoes.register(path, Some(hash.clone())),
            Err(e) => tracing::warn!(path, error = %e, "index update after write failed"),
        }
    }

    /// CAS write: `expected` is the hash the caller read (`None` = must not exist). Returns the
    /// new hash.
    pub fn write(
        &mut self,
        path: &str,
        bytes: &[u8],
        expected: Option<ContentHash>,
    ) -> Result<ContentHash> {
        let (h, full) = self.fs.put(path, bytes, expected.as_ref())?;
        self.index_written(path, bytes, &full, &h);
        Ok(h)
    }

    /// Creates `path`; [`VaultError::Exists`] if it exists.
    pub fn create(&mut self, path: &str, bytes: &[u8]) -> Result<ContentHash> {
        validate_rel_path(path)?;
        if self.fs.read(path)?.is_some() {
            return Err(VaultError::Exists(path.to_string()));
        }
        self.write(path, bytes, None)
    }

    /// Recoverable delete (Live: into `.trash/`; Shadow: tombstone). CAS on `expected`.
    pub fn delete(&mut self, path: &str, expected: ContentHash) -> Result<()> {
        self.fs.remove(path, &expected)?;
        if !self.fs.is_shadow() {
            match self.index.remove_file(path) {
                Ok(_) => self.echoes.register(path, None),
                Err(e) => tracing::warn!(path, error = %e, "index update after delete failed"),
            }
        }
        Ok(())
    }

    /// Whether the index's view of `src` still matches what the filesystem holds (Live mode,
    /// live-root files: size and mtime equal the indexed row).
    fn fresh(&self, src: &str, bytes: &[u8], from_overlay: bool) -> Result<bool> {
        if from_overlay {
            return Ok(true); // the span check in rewrite_note is the guard for overlay copies
        }
        let Some(row) = self.index.query().file(src)? else {
            return Ok(false);
        };
        let full = self.fs.root().join(src);
        let md = std::fs::metadata(&full).map_err(|e| crate::error::io(&full, e))?;
        let (m, _) = times(&md);
        Ok(usize::try_from(row.size).ok() == Some(bytes.len()) && row.mtime == m)
    }

    /// Renames `from` to `to` (CAS on `expected`), then rewrites inbound links (Obsidian's
    /// `alwaysUpdateLinks: true`): see [`crate::rewrite`]'s rules. Source notes that changed
    /// since the index saw them are skipped and reported, never clobbered.
    pub fn rename(&mut self, from: &str, to: &str, expected: ContentHash) -> Result<RenameOutcome> {
        validate_rel_path(from)?;
        validate_rel_path(to)?;
        if self.fs.read(to)?.is_some() {
            return Err(VaultError::Exists(to.to_string()));
        }
        let mut groups: BTreeMap<String, Vec<LinkRow>> = BTreeMap::new();
        for row in self.index.query().backlinks(from)? {
            groups.entry(row.src_path.clone()).or_default().push(row);
        }
        // The moved note's own folder-relative outgoing links (to other files) are rebased onto
        // its new folder in the same CAS write as its self-link rewrites.
        let outgoing: Vec<LinkRow> = self
            .index
            .query()
            .outgoing(from)?
            .into_iter()
            .filter(|l| l.span.is_some() && l.resolved_path.as_deref().is_some_and(|r| r != from))
            .collect();
        if !outgoing.is_empty() {
            groups.entry(from.to_string()).or_default();
        }
        let mut outcome = RenameOutcome::default();
        // Freshness check before the move: remember the hash each source had.
        let mut pre: BTreeMap<String, ContentHash> = BTreeMap::new();
        for src in groups.keys() {
            let new_src = if src == from { to } else { src.as_str() };
            match self.fs.read_src(src)? {
                Some((b, ov)) if self.fresh(src, &b, ov)? => {
                    pre.insert(src.clone(), ContentHash::of(&b));
                }
                _ => outcome.skipped_conflicts.push(new_src.to_string()),
            }
        }
        let moved = self.index.query().file(from)?.map(|r| r.id);
        let (h, full) = self.fs.move_file(from, to, &expected)?;
        if !self.fs.is_shadow() {
            let bytes = self
                .fs
                .read(to)?
                .ok_or_else(|| VaultError::NotFound(to.to_string()))?;
            let res = (|| -> Result<()> {
                match self.index.rename_file(from, to) {
                    Ok(_) | Err(cv_index::Error::NotFound(_)) => {}
                    Err(e) => return Err(e.into()),
                }
                let md = std::fs::metadata(&full).map_err(|e| crate::error::io(&full, e))?;
                let (m, c) = times(&md);
                self.index.upsert_file(to, &bytes, m, c)?;
                Ok(())
            })();
            match res {
                Ok(()) => {
                    self.echoes.register(from, None);
                    self.echoes.register(to, Some(h.clone()));
                }
                Err(e) => tracing::warn!(from, to, error = %e, "index update after rename failed"),
            }
        }
        // Resolver after the rename, and the same without the moved file (uniqueness check).
        let mut with = self.index.resolver().clone();
        if let Some(id) = moved
            && self.fs.is_shadow()
        {
            with.remove(id, from);
            with.add(id, to);
        }
        let mut without = with.clone();
        if let Some(id) = moved {
            without.remove(id, to);
        }
        for (src, links) in groups {
            let Some(pre_hash) = pre.remove(&src) else {
                continue;
            };
            let src_new = if src == from {
                to.to_string()
            } else {
                src.clone()
            };
            let src_id = if src == from {
                moved
            } else {
                self.index.query().file(&src_new)?.map(|r| r.id)
            };
            let cur = self.fs.read(&src_new)?;
            let text = match cur {
                Some(b) if ContentHash::of(&b) == pre_hash => String::from_utf8(b).ok(),
                _ => None,
            };
            let Some(text) = text else {
                outcome.skipped_conflicts.push(src_new);
                continue;
            };
            let unique = |cand: &str| match (moved, src_id) {
                (Some(m), Some(sid)) => {
                    with.resolve(&src_new, sid, cand) == Some(m)
                        && without.resolve(&src_new, sid, cand).is_none()
                }
                _ => false,
            };
            let Some(new) = rewrite_note(
                &text,
                &links,
                if src == from { &outgoing } else { &[] },
                &src_new,
                from,
                to,
                &unique,
            ) else {
                outcome.skipped_conflicts.push(src_new);
                continue;
            };
            if new == text {
                continue;
            }
            match self.write(&src_new, new.as_bytes(), Some(pre_hash)) {
                Ok(_) => outcome.rewritten.push(src_new),
                Err(VaultError::Conflict { .. }) => outcome.skipped_conflicts.push(src_new),
                Err(e) => {
                    tracing::warn!(path = %src_new, error = %e, "link rewrite write failed");
                    outcome.skipped_conflicts.push(src_new);
                }
            }
        }
        outcome.rewritten.sort();
        outcome.skipped_conflicts.sort();
        Ok(outcome)
    }

    /// Renames folder `from` to `to` by renaming every file under it (each with its current
    /// hash, each rewriting inbound links). Empty live source folders are removed best-effort.
    pub fn rename_folder(&mut self, from: &str, to: &str) -> Result<RenameOutcome> {
        validate_rel_path(from)?;
        validate_rel_path(to)?;
        let mut outcome = RenameOutcome::default();
        for f in self.fs.list_files(from)? {
            let Some(rest) = f.strip_prefix(&format!("{from}/")) else {
                continue;
            };
            let Some(h) = self.fs.hash(&f)? else {
                continue;
            };
            let o = self.rename(&f, &format!("{to}/{rest}"), h)?;
            outcome.merge(o);
        }
        if !self.fs.is_shadow() {
            remove_empty_dirs(&self.fs.root().join(from));
        }
        Ok(outcome)
    }
}

/// Removes `dir` and its empty subfolders (best-effort; failures are logged).
fn remove_empty_dirs(dir: &Path) {
    let mut dirs: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_dir())
        .map(|e| e.into_path())
        .collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in dirs {
        if let Err(e) = std::fs::remove_dir(&d) {
            tracing::warn!(dir = %d.display(), error = %e, "leaving non-empty or busy folder");
        }
    }
}
