//! [`VaultFs`]: the Live/Shadow filesystem every vault mutation goes through.
//!
//! Live mode reads and writes the vault root; it is refused when the root is (under) the
//! protected live vault and the cut-over marker is absent (I1). Shadow mode reads the live vault
//! through a scratch overlay (`<scratch>/vault/`), applies every mutation to the overlay only and
//! records it through a [`ShadowLog`] (ops `create`, `modify`, `delete`, and `rename_to` +
//! `rename_from` for a rename); the live root is never written. Deletes in the overlay are
//! tombstones kept in `<scratch>/.cv-tombstones.json` (outside the overlay tree, so they never
//! look like vault content).

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use cv_state::NewShadowWrite;

use crate::error::{Result, VaultError, io};
use crate::hash::ContentHash;
use crate::path::{TMP_PREFIX, is_ignored, validate_rel_path};

/// The live vault (I1). A parameter everywhere so tests use a temp dir as the protected root.
pub const DEFAULT_PROTECTED_ROOT: &str = "~/vault";

/// Roots no scratch dir may live under (every repo on this machine).
pub const DEFAULT_REPO_ROOTS: &[&str] = &["<code-root>"];

const TOMBSTONES: &str = ".cv-tombstones.json";
const OVERLAY: &str = "vault";

/// The cut-over check (I1), injected by the caller. `cv::cutover::StateCutover` implements it over
/// `state.db` (plan `cut-over-now` WP-1); this crate ships only [`NoCutover`] and never creates a
/// marker.
pub trait CutoverMarker {
    /// Whether the cut-over marker is present.
    fn present(&self) -> Result<bool>;
}

/// The only non-test [`CutoverMarker`]: never present.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoCutover;

impl CutoverMarker for NoCutover {
    fn present(&self) -> Result<bool> {
        Ok(false)
    }
}

/// Where shadow-mode mutations are recorded (`state.db.shadow_writes`).
pub trait ShadowLog: Send {
    /// Appends one would-be write.
    fn record(&mut self, w: &NewShadowWrite) -> Result<()>;
}

impl ShadowLog for cv_state::StateDb {
    fn record(&mut self, w: &NewShadowWrite) -> Result<()> {
        self.append_shadow_write(w)?;
        Ok(())
    }
}

/// An in-memory [`ShadowLog`] (tests, dry runs); clones share the same entries.
#[derive(Debug, Clone, Default)]
pub struct MemoryShadowLog(Arc<Mutex<Vec<NewShadowWrite>>>);

impl MemoryShadowLog {
    /// A snapshot of the recorded writes, oldest first.
    pub fn entries(&self) -> Vec<NewShadowWrite> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl ShadowLog for MemoryShadowLog {
    fn record(&mut self, w: &NewShadowWrite) -> Result<()> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(w.clone());
        Ok(())
    }
}

fn canon_or_self(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Validates the shadow scratch dir: creates it if missing, canonicalizes, and refuses it when
/// it is equal to/under/above `live_root`, equal to/under any `repo_roots` entry
/// ([`DEFAULT_REPO_ROOTS`] in production), or when it or any ancestor contains a `.git` entry.
/// Returns the canonical path.
pub fn validate_scratch(
    scratch: &Path,
    live_root: &Path,
    repo_roots: &[PathBuf],
) -> Result<PathBuf> {
    let refuse = |reason: String| VaultError::ScratchRefused {
        scratch: scratch.display().to_string(),
        reason,
    };
    std::fs::create_dir_all(scratch).map_err(|e| io(scratch, e))?;
    let canon = std::fs::canonicalize(scratch).map_err(|e| io(scratch, e))?;
    let live = canon_or_self(live_root);
    if canon.starts_with(&live) || live.starts_with(&canon) {
        return Err(refuse(format!("overlaps the live root {}", live.display())));
    }
    for repo in repo_roots {
        let repo = canon_or_self(repo);
        if canon.starts_with(&repo) {
            return Err(refuse(format!("under the repo root {}", repo.display())));
        }
    }
    for a in canon.ancestors() {
        if a.join(".git").symlink_metadata().is_ok() {
            return Err(refuse(format!("{} contains .git", a.display())));
        }
    }
    Ok(canon)
}

struct Shadow {
    overlay: PathBuf,
    tomb_file: PathBuf,
    tombstones: BTreeSet<String>,
    log: Box<dyn ShadowLog>,
}

/// The vault filesystem (Live or Shadow); see the module docs.
pub struct VaultFs {
    root: PathBuf,
    shadow: Option<Shadow>,
    job_id: Option<String>,
    /// The canonical protected root (I1), for the write-through-link destination check.
    protected: Option<PathBuf>,
    /// Whether the cut-over marker was present when live mode was opened.
    marker_present: bool,
    /// Canonical roots outside the vault a write through a symlinked note may land under
    /// (`CV_LINK_WRITE_ROOTS`, audit B1). Empty by default.
    link_write_roots: Vec<PathBuf>,
}

impl std::fmt::Debug for VaultFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultFs")
            .field("root", &self.root)
            .field("overlay", &self.shadow.as_ref().map(|s| &s.overlay))
            .finish_non_exhaustive()
    }
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
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

impl VaultFs {
    /// Live mode on `root`. Refused ([`VaultError::ProtectedRoot`]) when `root` canonicalizes to
    /// or under the canonical `protected_root`, or is an ancestor of it, ([`DEFAULT_PROTECTED_ROOT`] in production) and
    /// `marker` is not present (I1).
    pub fn live(root: &Path, protected_root: &Path, marker: &dyn CutoverMarker) -> Result<Self> {
        let root = std::fs::canonicalize(root).map_err(|e| io(root, e))?;
        let protected = canon_or_self(protected_root);
        // Either containing the other is refused: a root above the live vault reaches into it.
        let marker_present = marker.present()?;
        if (root.starts_with(&protected) || protected.starts_with(&root)) && !marker_present {
            return Err(VaultError::ProtectedRoot {
                root: root.display().to_string(),
                protected: protected.display().to_string(),
            });
        }
        Ok(Self {
            root,
            shadow: None,
            job_id: None,
            protected: Some(protected),
            marker_present,
            link_write_roots: Vec::new(),
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

    /// The file a live write of `path` lands on: `full` itself for a regular (or new) entry, or
    /// the canonical target of a symlinked entry when that target is a regular file under the
    /// vault root or an allowlisted root, and not under the protected root without the marker.
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
        if let Some(p) = &self.protected
            && target.starts_with(p)
            && !self.marker_present
        {
            return Err(refuse(
                "the link target is under the protected root and no cut-over marker is present",
            ));
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

    /// Shadow mode: reads `live_root` (never writes it), mutations go to `<scratch>/vault/` and
    /// `log`. `scratch` is validated by [`validate_scratch`] against `repo_roots`.
    pub fn shadow(
        live_root: &Path,
        scratch: &Path,
        repo_roots: &[PathBuf],
        log: Box<dyn ShadowLog>,
    ) -> Result<Self> {
        let root = std::fs::canonicalize(live_root).map_err(|e| io(live_root, e))?;
        let scratch = validate_scratch(scratch, &root, repo_roots)?;
        let overlay = scratch.join(OVERLAY);
        std::fs::create_dir_all(&overlay).map_err(|e| io(&overlay, e))?;
        let tomb_file = scratch.join(TOMBSTONES);
        let tombstones = match read_opt(&tomb_file)? {
            Some(b) => serde_json::from_slice(&b)?,
            None => BTreeSet::new(),
        };
        Ok(Self {
            root,
            shadow: Some(Shadow {
                overlay,
                tomb_file,
                tombstones,
                log,
            }),
            job_id: None,
            protected: None,
            marker_present: false,
            link_write_roots: Vec::new(),
        })
    }

    /// The (canonical) live vault root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Shadow mode?
    pub fn is_shadow(&self) -> bool {
        self.shadow.is_some()
    }

    /// The overlay dir in shadow mode.
    pub fn overlay(&self) -> Option<&Path> {
        self.shadow.as_ref().map(|s| s.overlay.as_path())
    }

    /// The job id recorded with subsequent shadow writes.
    pub fn set_job_id(&mut self, job_id: Option<String>) {
        self.job_id = job_id;
    }

    /// The directory mutations land in (live root, or the overlay).
    fn write_base(&self) -> &Path {
        self.shadow.as_ref().map_or(&self.root, |s| &s.overlay)
    }

    /// Reads `path` (overlay first, tombstones hide live files). `None` if absent.
    pub fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.read_src(path)?.map(|(b, _)| b))
    }

    /// Like [`Self::read`], also telling whether the bytes came from the overlay.
    pub(crate) fn read_src(&self, path: &str) -> Result<Option<(Vec<u8>, bool)>> {
        validate_rel_path(path)?;
        if let Some(s) = &self.shadow {
            if let Some(b) = read_opt(&s.overlay.join(path))? {
                return Ok(Some((b, true)));
            }
            if s.tombstones.contains(path) {
                return Ok(None);
            }
        }
        Ok(read_opt(&self.root.join(path))?.map(|b| (b, false)))
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

    fn log(
        &mut self,
        op: &str,
        path: &str,
        before: Option<&ContentHash>,
        after: Option<&ContentHash>,
    ) -> Result<()> {
        let job_id = self.job_id.clone();
        if let Some(s) = &mut self.shadow {
            s.log.record(&NewShadowWrite {
                op: op.to_string(),
                path: path.to_string(),
                before_hash: before.map(|h| h.as_str().to_string()),
                after_hash: after.map(|h| h.as_str().to_string()),
                job_id,
                at_ms: now_ms(),
            })?;
        }
        Ok(())
    }

    fn save_tombstones(&self) -> Result<()> {
        let Some(s) = &self.shadow else {
            return Ok(());
        };
        let dir = s.tomb_file.parent().unwrap_or(Path::new("."));
        let bytes = serde_json::to_vec(&s.tombstones)?;
        let mut tmp = tempfile::Builder::new()
            .prefix(TMP_PREFIX)
            .tempfile_in(dir)
            .map_err(|e| io(dir, e))?;
        tmp.write_all(&bytes).map_err(|e| io(tmp.path(), e))?;
        tmp.as_file().sync_all().map_err(|e| io(&s.tomb_file, e))?;
        tmp.persist(&s.tomb_file)
            .map_err(|e| io(&s.tomb_file, e.error))?;
        fsync_dir(dir)
    }

    fn set_tombstone(&mut self, path: &str, on: bool) -> Result<()> {
        let changed = match &mut self.shadow {
            Some(s) if on => s.tombstones.insert(path.to_string()),
            Some(s) => s.tombstones.remove(path),
            None => false,
        };
        if changed {
            self.save_tombstones()?;
        }
        Ok(())
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
        let op = if expected.is_none() {
            "create"
        } else {
            "modify"
        };
        self.put_op(path, bytes, expected, op)
    }

    /// [`Self::put`] logging shadow op `op`.
    fn put_op(
        &mut self,
        path: &str,
        bytes: &[u8],
        expected: Option<&ContentHash>,
        op: &str,
    ) -> Result<(ContentHash, PathBuf)> {
        validate_rel_path(path)?;
        let vault_full = self.write_base().join(path);
        // Live mode writes through a symlinked note onto its (confined) target; shadow mode
        // writes only regular overlay files.
        let full = if self.is_shadow() {
            vault_full.clone()
        } else {
            self.write_target(path, &vault_full)?
        };
        self.check(path, expected)?;
        let dir = full.parent().unwrap_or(self.write_base()).to_path_buf();
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
        let after = ContentHash::of(bytes);
        if self.is_shadow() {
            self.set_tombstone(path, false)?;
            self.log(op, path, expected, Some(&after))?;
        }
        Ok((after, vault_full))
    }

    /// Recoverable delete (CAS on `expected`). Live: moves the file to `.trash/<path>` (` (n)`
    /// suffix on collision) and returns that vault-relative trash path. Shadow: drops the overlay
    /// copy, tombstones the path and returns `None`.
    /// `permanent` (plan editor-ui-round-3 WP-3) removes the live file outright instead of the
    /// trash move (same CAS, same shadow tombstone) and returns `None`.
    pub(crate) fn remove_with(
        &mut self,
        path: &str,
        expected: &ContentHash,
        permanent: bool,
    ) -> Result<Option<String>> {
        validate_rel_path(path)?;
        if self.hash(path)?.is_none() {
            return Err(VaultError::NotFound(path.to_string()));
        }
        self.check(path, Some(expected))?;
        if let Some(s) = &self.shadow {
            let ov = s.overlay.join(path);
            if ov.exists() {
                std::fs::remove_file(&ov).map_err(|e| io(&ov, e))?;
            }
            let live_has = self.root.join(path).is_file();
            self.set_tombstone(path, live_has)?;
            self.log("delete", path, Some(expected), None)?;
            return Ok(None);
        }
        if permanent {
            let src = self.root.join(path);
            std::fs::remove_file(&src).map_err(|e| io(&src, e))?;
            return Ok(None);
        }
        let trash_rel = self.trash_slot(path);
        let dest = self.root.join(&trash_rel);
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d).map_err(|e| io(d, e))?;
        }
        let src = self.root.join(path);
        std::fs::rename(&src, &dest).map_err(|e| io(&src, e))?;
        Ok(Some(trash_rel))
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
        let Some(bytes) = self.read(from)? else {
            return Err(VaultError::NotFound(from.to_string()));
        };
        if self.read(to)?.is_some() {
            return Err(VaultError::Exists(to.to_string()));
        }
        self.check(from, Some(expected))?;
        if self.is_shadow() {
            let (h, full) = self.put_op(to, &bytes, None, "rename_to")?;
            let ov = self.write_base().join(from);
            if ov.exists() {
                std::fs::remove_file(&ov).map_err(|e| io(&ov, e))?;
            }
            let live_has = self.root.join(from).is_file();
            self.set_tombstone(from, live_has)?;
            self.log("rename_from", from, Some(expected), None)?;
            return Ok((h, full));
        }
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
        if let Err(e) = std::fs::remove_file(&src) {
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

    /// Every vault file under folder `dir` (`""` = whole vault) as this filesystem sees it
    /// (overlay ∪ live, minus tombstones), skipping ignored paths. Sorted.
    pub fn list_files(&self, dir: &str) -> Result<Vec<String>> {
        let mut out = BTreeSet::new();
        let mut bases = vec![self.root.clone()];
        if let Some(s) = &self.shadow {
            bases.push(s.overlay.clone());
        }
        for base in bases {
            let start = if dir.is_empty() {
                base.clone()
            } else {
                base.join(dir)
            };
            if !start.is_dir() {
                continue;
            }
            for entry in walkdir::WalkDir::new(&start) {
                let entry = entry.map_err(|e| VaultError::Io {
                    path: start.display().to_string(),
                    source: std::io::Error::other(e.to_string()),
                })?;
                if !is_file_or_file_link(&entry) {
                    continue;
                }
                if let Some(rel) = cv_index::rel_path(&base, entry.path())
                    && !is_ignored(&rel)
                {
                    out.insert(rel);
                }
            }
        }
        if let Some(s) = &self.shadow {
            for t in &s.tombstones {
                if !s.overlay.join(t).is_file() {
                    out.remove(t);
                }
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
        let prot = tempfile::tempdir().unwrap();
        let mut fs = VaultFs::live(d.path(), prot.path(), &NoCutover).unwrap();
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
    fn trash_slot_suffixes() {
        let d = tempfile::tempdir().unwrap();
        let prot = tempfile::tempdir().unwrap();
        let fs = VaultFs::live(d.path(), prot.path(), &NoCutover).unwrap();
        assert_eq!(fs.trash_slot("f/a.md"), ".trash/f/a.md");
        std::fs::create_dir_all(d.path().join(".trash/f")).unwrap();
        std::fs::write(d.path().join(".trash/f/a.md"), "").unwrap();
        assert_eq!(fs.trash_slot("f/a.md"), ".trash/f/a (1).md");
        assert_eq!(fs.trash_slot("noext"), ".trash/noext");
    }
}
