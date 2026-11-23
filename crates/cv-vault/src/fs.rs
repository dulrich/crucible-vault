//! [`VaultFs`]: the filesystem every vault mutation goes through.
//!
//! It reads and writes the vault root. Writes through a symlinked note land on its target only
//! when that target lies under the vault root or an allowlisted link-write root.

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

/// A regular file, or a symlink resolving to one (dangling links are skipped).
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

/// An io error, with a cross-filesystem failure named as such: a move never falls back to a
/// silent copy + delete.
fn cross_device(path: &Path, e: std::io::Error) -> VaultError {
    if e.kind() == std::io::ErrorKind::CrossesDevices {
        return io(
            path,
            std::io::Error::new(
                e.kind(),
                "cross-filesystem move refused (cv never copies and deletes)",
            ),
        );
    }
    io(path, e)
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
        if !self.allowed(&target) {
            return Err(refuse(
                "the link target is outside the vault root and every link-write root",
            ));
        }
        Ok(target)
    }

    /// The canonical location of `path`'s parent folder (Decision 2: every link component of
    /// the parent chain resolved): the nearest existing ancestor, canonicalized, plus the
    /// not-yet-existing tail a write would create as real folders. `Err(reason)` when an
    /// existing component is a dangling link or cannot be resolved.
    fn canonical_parent(&self, path: &str) -> std::result::Result<PathBuf, &'static str> {
        let full = self.root.join(path);
        let mut existing = full.parent().unwrap_or(&self.root).to_path_buf();
        let mut tail = Vec::new();
        while existing != self.root {
            match existing.symlink_metadata() {
                Ok(_) => break,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let Some(name) = existing.file_name() else {
                        break;
                    };
                    tail.push(name.to_os_string());
                    if !existing.pop() {
                        break;
                    }
                }
                Err(_) => return Err("a folder on its path cannot be resolved"),
            }
        }
        let mut canon = std::fs::canonicalize(&existing)
            .map_err(|_| "a folder link on its path is dangling")?;
        for name in tail.into_iter().rev() {
            canon.push(name);
        }
        Ok(canon)
    }

    /// Whether canonical `p` lies under the vault root or a link-write root.
    fn allowed(&self, p: &Path) -> bool {
        p.starts_with(&self.root) || self.link_write_roots.iter().any(|r| p.starts_with(r))
    }

    /// Decision 2: the canonical parent folder of the **entry** `path` (what a create, rename or
    /// unlink mutates), refused unless it lies under the vault root or a link-write root.
    fn confine_entry(&self, path: &str) -> Result<PathBuf> {
        let refuse = |reason: &str| VaultError::LinkWriteRefused {
            path: path.to_string(),
            reason: reason.to_string(),
        };
        let parent = self.canonical_parent(path).map_err(refuse)?;
        if !self.allowed(&parent) {
            return Err(refuse(
                "its folder resolves outside the vault root and every link-write root",
            ));
        }
        Ok(parent)
    }

    /// Decision 4: whether folder `dir` is reached through a link (any component of its own
    /// chain, itself included, is a symlink or cannot be resolved).
    pub(crate) fn dir_through_link(&self, dir: &str) -> bool {
        self.canonical_parent(&format!("{dir}/x"))
            .map_or(true, |c| c != self.root.join(dir))
    }

    /// Decision 4: the first folder link (a symlink that is not a file link, dangling included)
    /// under folder `dir`, as a vault path. `dir` itself is not considered.
    pub(crate) fn folder_link_under(&self, dir: &str) -> Option<String> {
        let start = self.root.join(dir);
        walkdir::WalkDir::new(&start)
            .min_depth(1)
            .into_iter()
            .filter_map(std::result::Result::ok)
            .find(|e| e.path_is_symlink() && !e.path().is_file())
            .and_then(|e| cv_index::rel_path(&self.root, e.path()))
    }

    /// Decision 3: whether a removal or move of the entry `path` would act outside the vault
    /// root — its parent chain resolves outside it (through a folder link). Unresolvable parents
    /// count as outside (fail safe). A terminal file link is judged by its entry, not its
    /// referent (cycle-2 E2: rename/delete act on the link). Write roots are not consulted.
    pub fn outside_root(&self, path: &str) -> bool {
        self.canonical_parent(path)
            .map_or(true, |parent| !parent.starts_with(&self.root))
    }

    /// Bumps the mtime of `path`'s referent to now (the attachment-reuse touch), confined like
    /// [`Self::put`]: a terminal link through [`Self::write_target`], a plain entry by its
    /// canonical parent chain.
    pub fn touch(&self, path: &str) -> Result<()> {
        validate_rel_path(path)?;
        let full = self.root.join(path);
        let target = self.write_target(path, &full)?;
        if target == full {
            self.confine_entry(path)?;
        }
        std::fs::OpenOptions::new()
            .write(true)
            .open(&target)
            .and_then(|f| f.set_modified(std::time::SystemTime::now()))
            .map_err(|e| io(&target, e))
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
        if full == vault_full {
            // Decision 2: a plain entry lands through its parent chain; confine it.
            self.confine_entry(path)?;
        }
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
        self.confine_entry(path)?;
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
        // Trash storage guard: the `.trash/…` destination never runs through a link.
        let dest = crate::trash::storage_path(&self.root, &trash_rel).map_err(|e| {
            VaultError::LinkWriteRefused {
                path: path.to_string(),
                reason: e.to_string(),
            }
        })?;
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d).map_err(|e| io(d, e))?;
        }
        std::fs::rename(&src, &dest).map_err(|e| cross_device(&src, e))?;
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
        // Decision 2: both entries' parent chains are confined.
        self.confine_entry(from)?;
        self.confine_entry(to)?;
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
            Err(e) => return Err(cross_device(&dest, e)),
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
    /// Filters the vault-wide discovery ([`cv_index::discover_listing`], plan folder-links-follow
    /// Decision 5): folder links are followed with one owner per canonical directory, so a losing
    /// alias or loop is never listed; `.gitignore`d files are still listed (only
    /// [`is_ignored`] paths are skipped).
    pub fn list_files(&self, dir: &str) -> Result<Vec<String>> {
        let start = if dir.is_empty() {
            self.root.clone()
        } else {
            self.root.join(dir)
        };
        if !start.is_dir() {
            return Ok(Vec::new());
        }
        let prefix = format!("{dir}/");
        let d = cv_index::discover_listing(&self.root, &is_ignored)?;
        Ok(d.files
            .into_iter()
            .map(|f| f.path)
            .filter(|p| (dir.is_empty() || p.starts_with(&prefix)) && !is_ignored(p))
            .collect())
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

#[cfg(test)]
mod discovery_tests {
    use super::*;

    /// Plan folder-links-follow Decision 5: `list_files` agrees with the index walk — linked files
    /// listed under the owning alias only, a loop never listed, `.gitignore`d files kept.
    #[test]
    fn list_files_follows_folder_links_with_one_owner() {
        let d = tempfile::tempdir().unwrap();
        let vault = d.path().join("vault");
        let ext = d.path().join("ext");
        std::fs::create_dir_all(vault.join("n")).unwrap();
        std::fs::create_dir_all(&ext).unwrap();
        std::fs::write(ext.join("s.md"), "s").unwrap();
        std::fs::write(vault.join("n/ign.md"), "i").unwrap();
        std::fs::write(vault.join(".gitignore"), "ign.md\n").unwrap();
        std::os::unix::fs::symlink(&ext, vault.join("n/a")).unwrap();
        std::os::unix::fs::symlink(&ext, vault.join("n/b")).unwrap();
        std::os::unix::fs::symlink(&vault, vault.join("n/loop")).unwrap();
        let fs = VaultFs::live(&vault).unwrap();
        assert_eq!(fs.list_files("n").unwrap(), vec!["n/a/s.md", "n/ign.md"]);
        assert_eq!(fs.list_files("n/b").unwrap(), Vec::<String>::new());
    }
}
