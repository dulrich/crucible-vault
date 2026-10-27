//! [`VaultFs`]: the filesystem every vault mutation goes through.
//!
//! It reads and writes the vault root. Writes through a symlinked note land on its target only
//! when that target lies under the vault root or an allowlisted link-write root.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{Result, VaultError, io};
use crate::hash::ContentHash;
use crate::path::{TMP_PREFIX, is_ignored, validate_rel_path};
use crate::trash::{TrashLocation, TrashMove, TrashTarget, identity};

/// The vault filesystem; see the module docs.
pub struct VaultFs {
    root: PathBuf,
    /// Canonical roots outside the vault a write through a symlinked note may land under
    /// (`CV_LINK_WRITE_ROOTS`, audit B1). Empty by default.
    link_write_roots: Vec<PathBuf>,
    /// Where recoverable deletes go (plan tidiness-trash-orphans WP-1; default the vault `.trash/`).
    trash: TrashTarget,
}

impl std::fmt::Debug for VaultFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultFs")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

/// Fsyncs directory `dir` so a just-persisted rename/link inside it survives a crash.
pub(crate) fn fsync_dir(dir: &Path) -> Result<()> {
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| io(dir, e))
}

fn read_opt(full: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(full) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(full, e)),
    }
}

/// A regular file, or a symlink resolving to one (directory links are never followed and dangling
/// links are skipped: the cv-index walk policy).
pub(crate) fn is_file_or_file_link(entry: &walkdir::DirEntry) -> bool {
    let ft = entry.file_type();
    ft.is_file() || (ft.is_symlink() && entry.path().is_file())
}

/// For a symlink at `src`: the link text to recreate it at `dest` (made absolute — canonical when it
/// resolves — when the original was relative and the folder changes). `None` when `src` is not a symlink.
fn relink_target(src: &Path, dest: &Path) -> Result<Option<PathBuf>> {
    let is_link = src
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink());
    if !is_link {
        return Ok(None);
    }
    let link = std::fs::read_link(src).map_err(|e| io(src, e))?;
    if link.is_absolute() || src.parent() == dest.parent() {
        return Ok(Some(link));
    }
    if let Ok(t) = std::fs::canonicalize(src) {
        return Ok(Some(t));
    }
    // Dangling: resolve `..` lexically so the link does not depend on the old folder existing.
    let mut out = src.parent().unwrap_or(Path::new("/")).to_path_buf();
    for c in link.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    Ok(Some(out))
}

#[cfg(test)]
pub(crate) static FAIL_BEFORE_PERSIST: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Test seam: `move_file`'s `remove_file(src)` fails (after the destination name was created).
#[cfg(test)]
pub(crate) static FAIL_REMOVE_SRC: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

impl VaultFs {
    /// Live mode on `root` (canonicalized; it must exist).
    pub fn live(root: &Path) -> Result<Self> {
        let root = std::fs::canonicalize(root).map_err(|e| io(root, e))?;
        Ok(Self {
            root,
            link_write_roots: Vec::new(),
            trash: TrashTarget::Vault,
        })
    }

    /// Allows writes through symlinked notes whose canonical target lies under one of `roots`
    /// (besides the vault root). Roots are canonicalized; missing ones are dropped. Default: none.
    #[must_use]
    pub fn with_link_write_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.link_write_roots = roots
            .iter()
            .filter_map(|r| std::fs::canonicalize(r).ok())
            .collect();
        self
    }

    /// The canonical link-write roots in effect.
    pub fn link_write_roots(&self) -> &[PathBuf] {
        &self.link_write_roots
    }

    /// Sets where recoverable deletes go (the daemon applies `vault.toml` `[trash] location`).
    pub fn set_trash_target(&mut self, target: TrashTarget) {
        self.trash = target;
    }

    /// Where recoverable deletes go.
    pub fn trash_target(&self) -> &TrashTarget {
        &self.trash
    }

    /// The file a live write of `path` lands on: `full` itself for a regular (or new) entry, or
    /// the canonical target of a symlinked entry when that target is a regular file under the
    /// vault root or an allowlisted root.
    /// Anything else is [`VaultError::LinkWriteRefused`]; the link is never replaced.
    fn write_target(&self, path: &str, full: &Path) -> Result<PathBuf> {
        let is_link = full
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink());
        if !is_link {
            return Ok(full.to_path_buf());
        }
        let refuse = |reason: &str| VaultError::LinkWriteRefused {
            path: path.to_string(),
            reason: reason.to_string(),
        };
        let Ok(target) = std::fs::canonicalize(full) else {
            return Err(refuse("the link is dangling"));
        };
        if !target.is_file() {
            return Err(refuse("the link target is not a regular file"));
        }
        let allowed = target.starts_with(&self.root)
            || self.link_write_roots.iter().any(|r| target.starts_with(r));
        if !allowed {
            return Err(refuse(
                "the link target is outside the vault root and every link-write root",
            ));
        }
        Ok(target)
    }

    /// The (canonical) live vault root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Reads `path`. `None` if absent.
    pub fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        validate_rel_path(path)?;
        read_opt(&self.root.join(path))
    }

    /// The current content hash of `path` (`None` if absent).
    pub fn hash(&self, path: &str) -> Result<Option<ContentHash>> {
        Ok(self.read(path)?.map(|b| ContentHash::of(&b)))
    }

    fn check(&self, path: &str, expected: Option<&ContentHash>) -> Result<Option<ContentHash>> {
        let found = self.hash(path)?;
        if found.as_ref() != expected {
            return Err(VaultError::Conflict {
                path: path.to_string(),
                expected: expected.cloned(),
                found,
            });
        }
        Ok(found)
    }

    /// CAS write: `expected` is the hash the caller read (`None` = must not exist). Writes a
    /// `.cv-tmp-*` file in the target's directory, fsyncs it, re-checks the hash immediately
    /// before the rename, then renames over the target (no-clobber when `expected` is `None`).
    /// Returns the new hash and the written file's full path.
    pub(crate) fn put(
        &mut self,
        path: &str,
        bytes: &[u8],
        expected: Option<&ContentHash>,
    ) -> Result<(ContentHash, PathBuf)> {
        validate_rel_path(path)?;
        let vault_full = self.root.join(path);
        // A write through a symlinked note lands on its (confined) target.
        let full = self.write_target(path, &vault_full)?;
        self.check(path, expected)?;
        let dir = full.parent().unwrap_or(&self.root).to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
        let mut tmp = tempfile::Builder::new()
            .prefix(TMP_PREFIX)
            .tempfile_in(&dir)
            .map_err(|e| io(&dir, e))?;
        tmp.write_all(bytes).map_err(|e| io(tmp.path(), e))?;
        tmp.as_file().sync_all().map_err(|e| io(tmp.path(), e))?;
        #[cfg(test)]
        if FAIL_BEFORE_PERSIST.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(VaultError::Io {
                path: path.to_string(),
                source: std::io::Error::other("injected failure"),
            });
        }
        // CAS: re-read immediately before the rename (I2).
        self.check(path, expected)?;
        let persisted = if expected.is_none() {
            tmp.persist_noclobber(&full)
        } else {
            tmp.persist(&full)
        };
        if let Err(e) = persisted {
            if e.error.kind() == std::io::ErrorKind::AlreadyExists {
                return Err(VaultError::Exists(path.to_string()));
            }
            return Err(io(&full, e.error));
        }
        fsync_dir(&dir)?;
        Ok((ContentHash::of(bytes), vault_full))
    }

    /// Recoverable delete (CAS on `expected`): moves the file into the configured trash
    /// ([`TrashTarget`]) and returns the move. The vault target is `.trash/<path>` (` (n)` suffix
    /// on collision); a system move that cannot rename falls back to it.
    /// `permanent` (plan editor-ui-round-3 WP-3) removes the file outright instead of the trash
    /// move (same CAS) and returns `None`.
    pub(crate) fn remove_with(
        &mut self,
        path: &str,
        expected: &ContentHash,
        permanent: bool,
    ) -> Result<Option<TrashMove>> {
        validate_rel_path(path)?;
        if self.hash(path)?.is_none() {
            return Err(VaultError::NotFound(path.to_string()));
        }
        self.check(path, Some(expected))?;
        if permanent {
            let src = self.root.join(path);
            std::fs::remove_file(&src).map_err(|e| io(&src, e))?;
            return Ok(None);
        }
        let src = self.root.join(path);
        let identity = identity(&src).map_err(|e| io(&src, e))?;
        if let TrashTarget::System(sys) = &self.trash
            && let Some(name) = sys.move_in(&src, path).map_err(|e| io(&src, e))?
        {
            return Ok(Some(TrashMove {
                location: TrashLocation::System,
                trash_path: name,
                identity,
            }));
        }
        let trash_rel = self.trash_slot(path);
        let dest = self.root.join(&trash_rel);
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d).map_err(|e| io(d, e))?;
        }
        std::fs::rename(&src, &dest).map_err(|e| io(&src, e))?;
        Ok(Some(TrashMove {
            location: TrashLocation::Vault,
            trash_path: trash_rel,
            identity,
        }))
    }

    /// The first free `.trash/<path>` slot: `name.ext`, then `name (1).ext`, `name (2).ext`, ….
    fn trash_slot(&self, path: &str) -> String {
        let (dir, name) = path.rsplit_once('/').map_or(("", path), |(d, n)| (d, n));
        let (stem, ext) = match name.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
            _ => (name, String::new()),
        };
        let prefix = if dir.is_empty() {
            ".trash/".to_string()
        } else {
            format!(".trash/{dir}/")
        };
        let mut n = 0u32;
        loop {
            let cand = if n == 0 {
                format!("{prefix}{name}")
            } else {
                format!("{prefix}{stem} ({n}){ext}")
            };
            if self.root.join(&cand).symlink_metadata().is_err() {
                return cand;
            }
            n += 1;
        }
    }

    /// Moves `from` to `to` (CAS on `from`; `to` must not exist). Returns the content hash and
    /// the destination's full path.
    pub(crate) fn move_file(
        &mut self,
        from: &str,
        to: &str,
        expected: &ContentHash,
    ) -> Result<(ContentHash, PathBuf)> {
        validate_rel_path(from)?;
        validate_rel_path(to)?;
        if self.read(from)?.is_none() {
            return Err(VaultError::NotFound(from.to_string()));
        }
        if self.read(to)?.is_some() {
            return Err(VaultError::Exists(to.to_string()));
        }
        self.check(from, Some(expected))?;
        let src = self.root.join(from);
        let dest = self.root.join(to);
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d).map_err(|e| io(d, e))?;
        }
        // A symlinked note moves as a link (the target never moves; a relative link target is
        // re-anchored so it still resolves from the new folder). A regular file moves by
        // hard_link (a no-clobber rename on one filesystem), then drops the old name.
        let linked = match relink_target(&src, &dest)? {
            Some(link) => std::os::unix::fs::symlink(&link, &dest),
            None => std::fs::hard_link(&src, &dest),
        };
        match linked {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(VaultError::Exists(to.to_string()));
            }
            Err(e) => return Err(io(&dest, e)),
        }
        #[cfg(test)]
        let removed = if FAIL_REMOVE_SRC.load(std::sync::atomic::Ordering::SeqCst) {
            Err(std::io::Error::other("injected failure"))
        } else {
            std::fs::remove_file(&src)
        };
        #[cfg(not(test))]
        let removed = std::fs::remove_file(&src);
        if let Err(e) = removed {
            // All-or-nothing: drop the new name so the vault is left as it was.
            let _ = std::fs::remove_file(&dest);
            return Err(io(&src, e));
        }
        if let Some(d) = dest.parent() {
            fsync_dir(d)?;
        }
        if let Some(d) = src.parent()
            && Some(d) != dest.parent()
        {
            fsync_dir(d)?;
        }
        Ok((expected.clone(), dest))
    }

    /// Every vault file under folder `dir` (`""` = whole vault), skipping ignored paths. Sorted.
    pub fn list_files(&self, dir: &str) -> Result<Vec<String>> {
        let mut out = BTreeSet::new();
        let base = &self.root;
        let start = if dir.is_empty() {
            base.clone()
        } else {
            base.join(dir)
        };
        if !start.is_dir() {
            return Ok(Vec::new());
        }
        for entry in walkdir::WalkDir::new(&start) {
            let entry = entry.map_err(|e| VaultError::Io {
                path: start.display().to_string(),
                source: std::io::Error::other(e.to_string()),
            })?;
            if !is_file_or_file_link(&entry) {
                continue;
            }
            if let Some(rel) = cv_index::rel_path(base, entry.path())
                && !is_ignored(&rel)
            {
                out.insert(rel);
            }
        }
        Ok(out.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn forced_failure_leaves_no_tmp() {
        let d = tempfile::tempdir().unwrap();
        let mut fs = VaultFs::live(d.path()).unwrap();
        FAIL_BEFORE_PERSIST.store(true, Ordering::SeqCst);
        let r = fs.put("a.md", b"x", None);
        FAIL_BEFORE_PERSIST.store(false, Ordering::SeqCst);
        assert!(r.is_err());
        let names: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(names.is_empty(), "{names:?}");
    }

    #[test]
    fn rename_undone_when_removing_the_source_fails() {
        let d = tempfile::tempdir().unwrap();
        let mut fs = VaultFs::live(d.path()).unwrap();
        // Seeded directly: `put` shares the parallel `FAIL_BEFORE_PERSIST` seam.
        std::fs::write(d.path().join("a.md"), b"x").unwrap();
        let h = ContentHash::of(b"x");
        FAIL_REMOVE_SRC.store(true, Ordering::SeqCst);
        let r = fs.move_file("a.md", "sub/b.md", &h);
        FAIL_REMOVE_SRC.store(false, Ordering::SeqCst);
        assert!(r.is_err());
        assert_eq!(fs.read("a.md").unwrap().as_deref(), Some(&b"x"[..]));
        assert!(fs.read("sub/b.md").unwrap().is_none());
    }

    #[test]
    fn trash_slot_suffixes() {
        let d = tempfile::tempdir().unwrap();
        let fs = VaultFs::live(d.path()).unwrap();
        assert_eq!(fs.trash_slot("f/a.md"), ".trash/f/a.md");
        std::fs::create_dir_all(d.path().join(".trash/f")).unwrap();
        std::fs::write(d.path().join(".trash/f/a.md"), "").unwrap();
        assert_eq!(fs.trash_slot("f/a.md"), ".trash/f/a (1).md");
        assert_eq!(fs.trash_slot("noext"), ".trash/noext");
    }
}
