//! The trash seam (plan tidiness-trash-orphans WP-1): where a recoverable delete moves a file,
//! and the record of each move.
//!
//! - [`TrashTarget::Vault`]: today's `.trash/<path>` slotting (Obsidian-style), unchanged.
//! - [`TrashTarget::System`]: the XDG Trash spec home trash (`$XDG_DATA_HOME/Trash`): the file
//!   goes to `files/<name>` and `info/<name>.trashinfo` carries the percent-encoded absolute
//!   `Path` and the local `DeletionDate`. The `.trashinfo` is written first (`O_EXCL`), then the
//!   file is renamed; a failed rename removes the `.trashinfo` again and the file falls back to
//!   the vault `.trash/` with a warning. Never copied.
//!
//! **Crash receipt (audit M1).** Every cv-written `.trashinfo` carries the extra key
//! [`CV_MARKER_KEY`]` = <vault-relative path>` (the spec lets readers ignore unknown keys). A
//! crash between the move and the ledger row leaves a marked entry that
//! [`cv_owned_system_entries`] reports; entries without the marker (anything another program
//! trashed) are never reported, so cv never adopts what it did not create. A desktop "Restore"
//! deletes the `.trashinfo`, so restored files are not reported either.
//!
//! New, not carried from Crucible (Obsidian owned the trash there).

use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The `.trashinfo` key cv adds to every system-trash entry it creates (its crash receipt).
pub const CV_MARKER_KEY: &str = "X-Crucible-Vault-Path";

/// Local-time `YYYY-MM-DDThh:mm:ss` for `DeletionDate` (injected: the daemon supplies a clock).
pub type DeletionClock = Arc<dyn Fn() -> String + Send + Sync>;
/// The rename used for the system move (injected so tests can simulate `EXDEV`).
pub type RenameFn = Arc<dyn Fn(&Path, &Path) -> std::io::Result<()> + Send + Sync>;

/// A configured XDG home trash.
#[derive(Clone)]
pub struct SystemTrash {
    root: PathBuf,
    clock: DeletionClock,
    rename: RenameFn,
}

impl std::fmt::Debug for SystemTrash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemTrash")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl SystemTrash {
    /// The home trash under `data_home` (`$XDG_DATA_HOME`, i.e. `<data_home>/Trash`).
    #[must_use]
    pub fn new(data_home: &Path, clock: DeletionClock) -> Self {
        Self {
            root: data_home.join("Trash"),
            clock,
            rename: Arc::new(|a, b| std::fs::rename(a, b)),
        }
    }

    /// Replaces the rename (tests: inject `ErrorKind::CrossesDevices`).
    #[must_use]
    pub fn with_rename(mut self, rename: RenameFn) -> Self {
        self.rename = rename;
        self
    }

    /// The trash directory (`…/Trash`, holding `files/` and `info/`).
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Moves `src` (vault path `rel`) into the trash. `Ok(Some(name))` = moved to `files/<name>`;
    /// `Ok(None)` = the rename failed for a reason other than `NotFound` (the caller falls back);
    /// `Err` = the file vanished, or the trash directories could not be prepared.
    pub(crate) fn move_in(&self, src: &Path, rel: &str) -> std::io::Result<Option<String>> {
        let files = self.root.join("files");
        let info = self.root.join("info");
        std::fs::create_dir_all(&files)?;
        std::fs::create_dir_all(&info)?;
        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        let body = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n{CV_MARKER_KEY}={}\n",
            percent_encode_path(&src.to_string_lossy()),
            (self.clock)(),
            percent_encode_path(rel),
        );
        let mut n = 0u32;
        let (chosen, info_path) = loop {
            let cand = suffixed(&name, n);
            n += 1;
            if files.join(&cand).symlink_metadata().is_ok() {
                continue;
            }
            let ip = info.join(format!("{cand}.trashinfo"));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&ip)
            {
                Ok(mut f) => {
                    if let Err(e) = f.write_all(body.as_bytes()).and_then(|()| f.sync_all()) {
                        let _ = std::fs::remove_file(&ip);
                        return Err(e);
                    }
                    break (cand, ip);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        };
        match (self.rename)(src, &files.join(&chosen)) {
            Ok(()) => Ok(Some(chosen)),
            Err(e) => {
                let _ = std::fs::remove_file(&info_path);
                if e.kind() == std::io::ErrorKind::NotFound {
                    return Err(e);
                }
                tracing::warn!(error = %e, "system trash move failed; using the vault .trash/");
                Ok(None)
            }
        }
    }
}

/// Where recoverable deletes go.
#[derive(Debug, Clone, Default)]
pub enum TrashTarget {
    /// `<vault>/.trash/<path>` (the default).
    #[default]
    Vault,
    /// The XDG home trash.
    System(SystemTrash),
}

/// Why a file was trashed (the ledger's `reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrashReason {
    /// A plain recoverable delete (editor delete, step deletes).
    Deleted,
    /// A note absorbed by another ([`crate::VaultWriter::delete_replaced`]).
    Replaced,
    /// An unshared attachment taken along by [`crate::VaultWriter::delete_note`].
    NoteAttachment,
    /// An orphaned attachment (WP-2's sweep).
    Orphan,
    /// A pre-existing trash file given a row by WP-2's seed pass.
    Seeded,
}

impl TrashReason {
    /// The ledger spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deleted => "deleted",
            Self::Replaced => "replaced",
            Self::NoteAttachment => "note-attachment",
            Self::Orphan => "orphan",
            Self::Seeded => "seeded",
        }
    }
}

/// Which trash a file landed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrashedIn {
    /// The vault `.trash/`.
    Vault,
    /// The XDG home trash.
    System,
}

impl TrashedIn {
    /// The ledger spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vault => "vault",
            Self::System => "system",
        }
    }
}

/// One completed trash move (what the fs layer did).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashMove {
    /// Where it landed.
    pub location: TrashedIn,
    /// Vault-relative `.trash/…` path (vault), or the name inside `Trash/files/` (system).
    pub trash_path: String,
    /// Size from `symlink_metadata` before the move.
    pub bytes: u64,
    /// mtime (ms) from `symlink_metadata` before the move (rename preserves it).
    pub mtime_ms: i64,
    /// Inode before the move (rename preserves it).
    pub inode: u64,
}

/// One trash move, ready for the ledger (`trash_entries`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashRecord {
    /// The move.
    pub moved: TrashMove,
    /// The vault path the file had.
    pub original_path: String,
    /// When it was trashed (ms since the epoch).
    pub trashed_ms: i64,
    /// Why.
    pub reason: TrashReason,
}

impl TrashRecord {
    /// System moves only: the `.trashinfo` file name (`info/<name>.trashinfo`).
    #[must_use]
    pub fn info_name(&self) -> Option<String> {
        (self.moved.location == TrashedIn::System)
            .then(|| format!("{}.trashinfo", self.moved.trash_path))
    }
}

/// Identity fields of `p` (no follow): `(bytes, mtime_ms, inode)`.
pub fn identity(p: &Path) -> std::io::Result<(u64, i64, u64)> {
    let md = p.symlink_metadata()?;
    let mtime_ms = md.mtime() * 1000 + md.mtime_nsec() / 1_000_000;
    Ok((md.len(), mtime_ms, md.ino()))
}

/// A cv-created system-trash entry (its `.trashinfo` carries [`CV_MARKER_KEY`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemTrashEntry {
    /// The name inside `Trash/files/` (the ledger's `trash_path`).
    pub name: String,
    /// The vault-relative path it was trashed from.
    pub original_path: String,
    /// The raw `DeletionDate` value.
    pub deletion_date: String,
    /// Identity of the file now in `files/` (bytes, mtime ms, inode).
    pub bytes: u64,
    pub mtime_ms: i64,
    pub inode: u64,
}

/// The adoption hook for WP-2's sweep: every cv-created entry of the home trash at `trash_root`
/// whose file still exists, sorted by name. The caller drops those the ledger already holds
/// (`StateDb::trash_entry_by_path("system", name)`). Unmarked entries are never returned. A
/// missing trash directory is an empty list.
pub fn cv_owned_system_entries(trash_root: &Path) -> std::io::Result<Vec<SystemTrashEntry>> {
    let info = trash_root.join("info");
    let rd = match std::fs::read_dir(&info) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for ent in rd {
        let ent = ent?;
        let fname = ent.file_name().to_string_lossy().into_owned();
        let Some(name) = fname.strip_suffix(".trashinfo") else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(ent.path()) else {
            continue;
        };
        let mut original = None;
        let mut date = String::new();
        for line in text.lines() {
            if let Some(v) = line.strip_prefix(&format!("{CV_MARKER_KEY}=")) {
                original = percent_decode(v);
            } else if let Some(v) = line.strip_prefix("DeletionDate=") {
                date = v.to_string();
            }
        }
        let Some(original_path) = original else {
            continue;
        };
        let Ok((bytes, mtime_ms, inode)) = identity(&trash_root.join("files").join(name)) else {
            continue;
        };
        out.push(SystemTrashEntry {
            name: name.to_string(),
            original_path,
            deletion_date: date,
            bytes,
            mtime_ms,
            inode,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// `name`, then `stem (n).ext` (the vault slot's suffix rule).
fn suffixed(name: &str, n: u32) -> String {
    if n == 0 {
        return name.to_string();
    }
    match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => format!("{s} ({n}).{e}"),
        _ => format!("{name} ({n})"),
    }
}

/// Percent-encodes a path for a `.trashinfo` `Path=` (RFC 3986 unreserved and `/` kept; every
/// other byte of the UTF-8 form as `%XX`).
#[must_use]
pub fn percent_encode_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    for b in p.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encoding_round_trips() {
        let p = "/v/My notes/caf\u{e9}.md";
        let e = percent_encode_path(p);
        assert_eq!(e, "/v/My%20notes/caf%C3%A9.md");
        assert_eq!(percent_decode(&e).as_deref(), Some(p));
    }

    #[test]
    fn suffix_rule() {
        assert_eq!(suffixed("a.md", 0), "a.md");
        assert_eq!(suffixed("a.md", 2), "a (2).md");
        assert_eq!(suffixed("noext", 1), "noext (1)");
    }

    #[test]
    fn unmarked_entries_are_never_adopted() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("Trash");
        std::fs::create_dir_all(t.join("files")).unwrap();
        std::fs::create_dir_all(t.join("info")).unwrap();
        std::fs::write(t.join("files/x.txt"), "x").unwrap();
        std::fs::write(
            t.join("info/x.txt.trashinfo"),
            "[Trash Info]\nPath=/x.txt\nDeletionDate=2026-01-01T00:00:00\n",
        )
        .unwrap();
        std::fs::write(t.join("files/y.md"), "y").unwrap();
        std::fs::write(
            t.join("info/y.md.trashinfo"),
            format!("[Trash Info]\nPath=/v/y.md\nDeletionDate=2026-01-01T00:00:00\n{CV_MARKER_KEY}=sub/y.md\n"),
        )
        .unwrap();
        let got = cv_owned_system_entries(&t).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "y.md");
        assert_eq!(got[0].original_path, "sub/y.md");
        assert_eq!(got[0].bytes, 1);
        assert!(
            cv_owned_system_entries(&d.path().join("none"))
                .unwrap()
                .is_empty()
        );
    }
}
