//! Watcher event normalization: echo suppression, dedupe, rename pairing (plan WP-7, "Watcher
//! handling").
//!
//! S2 measured ~5 raw events per tmp+rename write (Create/Modify/Name From/To/Both across the
//! temp and the final name — `docs/spikes/core-index/FINDINGS.md:64`). Raw events are therefore
//! only *hints*: a path is re-examined once it has been quiet for the debounce window, and its
//! current `(size, mtime)` then hash decide whether anything changed. The writer records
//! `(path → after_hash)` for its own writes; a change matching that echo is `Origin::Daemon` and
//! is not re-applied to the index. Crucible's equivalent (Obsidian's startup event replay and
//! write-queue echo handling) is dropped as Obsidian-induced: the initial scan here only seeds
//! the `(size, mtime)` map and emits nothing (I4).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use notify::event::{AccessKind, AccessMode, ModifyKind, RenameMode};

use crate::fs::is_file_or_file_link;
use crate::hash::ContentHash;
use crate::path::is_ignored;
use crate::writer::VaultWriter;
use cv_index::{file_times, rel_path};

/// What happened to a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    Created,
    Modified,
    Deleted,
    /// Moved from `from` to the event's path.
    Renamed {
        from: String,
    },
}

/// Who caused it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The daemon's own write (an echo; the index already has it).
    Daemon,
    /// Someone else (applied to the index before it is emitted).
    External,
}

/// A normalized vault change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultEvent {
    /// Vault-relative path.
    pub path: String,
    pub kind: EventKind,
    pub origin: Origin,
}

/// Watcher timings.
#[derive(Debug, Clone, Copy)]
pub struct WatchConfig {
    /// A path is examined once no raw event touched it for this long.
    pub debounce: Duration,
    /// How long the writer's echo records live.
    pub echo_ttl: Duration,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(150),
            echo_ttl: Duration::from_secs(5),
        }
    }
}

const ECHO_CAP: usize = 4096;

/// The writer's own recent writes: path → hash after (`None` = removed).
#[derive(Debug)]
pub(crate) struct Echoes {
    map: HashMap<String, (Option<ContentHash>, Instant)>,
    /// The writer's own moves: destination → (source, when). The vault fs moves by hard link +
    /// unlink (or a fresh symlink), never `rename(2)`, so notify never pairs them; this record
    /// lets the watcher collapse the source's removal and the destination's appearance into one
    /// `Renamed` (plan intake-round-2-clean-dedupe-reading WP-5a). Unmatched or expired, the two
    /// path echoes above still classify both halves as before (`Deleted` + `Created`).
    moves: HashMap<String, (String, Instant)>,
    /// Round 11 WP-1 (Decision 1): the writer's own replacement deletes, deleted path →
    /// (absorbing note, when). Read by the event bridge when it reports that path's `Deleted`.
    replaced: HashMap<String, (String, Instant)>,
    ttl: Duration,
}

impl Echoes {
    pub(crate) fn new(ttl: Duration) -> Self {
        Self {
            map: HashMap::new(),
            moves: HashMap::new(),
            replaced: HashMap::new(),
            ttl,
        }
    }

    fn expire(&mut self, now: Instant) {
        let ttl = self.ttl;
        self.map.retain(|_, (_, at)| now.duration_since(*at) < ttl);
        self.moves
            .retain(|_, (_, at)| now.duration_since(*at) < ttl);
        self.replaced
            .retain(|_, (_, at)| now.duration_since(*at) < ttl);
    }

    /// Records the writer's move `from → to` (alongside the two path echoes).
    pub(crate) fn register_move(&mut self, from: &str, to: &str) {
        let now = Instant::now();
        self.expire(now);
        if self.moves.len() >= ECHO_CAP
            && let Some(oldest) = self
                .moves
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(k, _)| k.clone())
        {
            self.moves.remove(&oldest);
        }
        self.moves.insert(to.to_string(), (from.to_string(), now));
    }

    /// Records that the writer deleted `path` because `by` absorbed it (bounded like the
    /// other echo records).
    pub(crate) fn register_replaced(&mut self, path: &str, by: &str) {
        let now = Instant::now();
        self.expire(now);
        if self.replaced.len() >= ECHO_CAP {
            self.replaced.clear();
        }
        self.replaced
            .insert(path.to_string(), (by.to_string(), now));
    }

    /// Consumes the live replacement record for `path`, returning the absorbing note.
    pub(crate) fn take_replaced(&mut self, path: &str) -> Option<String> {
        self.expire(Instant::now());
        self.replaced.remove(path).map(|(b, _)| b)
    }

    /// The destination of a live move echo whose source is `from` (not consumed).
    fn move_dest(&mut self, from: &str) -> Option<String> {
        self.expire(Instant::now());
        self.moves
            .iter()
            .find(|(_, (f, _))| f == from)
            .map(|(t, _)| t.clone())
    }

    /// Consumes the live move echo landing at `to`, returning its source.
    fn take_move(&mut self, to: &str) -> Option<String> {
        self.expire(Instant::now());
        self.moves.remove(to).map(|(f, _)| f)
    }

    pub(crate) fn register(&mut self, path: &str, after: Option<ContentHash>) {
        let now = Instant::now();
        self.expire(now);
        if self.map.len() >= ECHO_CAP
            && let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(k, _)| k.clone())
        {
            self.map.remove(&oldest);
        }
        self.map.insert(path.to_string(), (after, now));
    }

    /// Whether `path`'s current state `cur` is the writer's echo (consumed on match).
    pub(crate) fn take_match(&mut self, path: &str, cur: Option<&ContentHash>) -> bool {
        self.expire(Instant::now());
        if self.map.get(path).is_some_and(|(h, _)| h.as_ref() == cur) {
            self.map.remove(path);
            return true;
        }
        false
    }
}

#[derive(Debug, Clone)]
struct Stat {
    size: u64,
    mtime: Option<SystemTime>,
    hash: Option<ContentHash>,
}

impl Stat {
    fn same(&self, md: &std::fs::Metadata) -> bool {
        self.size == md.len() && self.mtime == md.modified().ok()
    }
}

/// Per-watcher state, living on the writer thread.
pub(crate) struct WatchState {
    root: PathBuf,
    seen: HashMap<String, Stat>,
    pending: HashMap<String, Instant>,
    renames: Vec<(String, String, Instant)>,
    debounce: Duration,
    /// Symlinked vault entries → their resolved target file (plan editor-cutover-blockers WP-1).
    links: HashMap<String, PathBuf>,
    /// Target file → the vault paths linking to it.
    by_target: HashMap<PathBuf, BTreeSet<String>>,
    /// Set when `links` changed since the last [`Self::take_watch_dirs`].
    links_changed: bool,
}

/// The file a symlink at `full` points to: canonical when it resolves, else (dangling) the
/// link's text resolved against the canonical parent it names, so the target's directory can be
/// watched until the target appears (audit E2). `None` when `full` is not a symlink.
fn link_target(full: &Path) -> Option<PathBuf> {
    let md = full.symlink_metadata().ok()?;
    if !md.file_type().is_symlink() {
        return None;
    }
    if let Ok(t) = std::fs::canonicalize(full) {
        return Some(t);
    }
    let link = std::fs::read_link(full).ok()?;
    let abs = if link.is_absolute() {
        link
    } else {
        full.parent()?.join(link)
    };
    let name = abs.file_name()?.to_owned();
    let dir = abs.parent()?;
    Some(
        std::fs::canonicalize(dir)
            .unwrap_or_else(|_| dir.to_path_buf())
            .join(name),
    )
}

impl WatchState {
    /// Seeds the `(size, mtime)` map from a walk of `root`; emits nothing (I4).
    pub(crate) fn seed(root: &Path, debounce: Duration) -> Self {
        let mut seen = HashMap::new();
        let mut links = HashMap::new();
        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let ft = entry.file_type();
            if !ft.is_file() && !ft.is_symlink() {
                continue;
            }
            let Some(rel) = rel_path(root, entry.path()) else {
                continue;
            };
            if is_ignored(&rel) {
                continue;
            }
            if ft.is_symlink()
                && let Some(t) = link_target(entry.path())
            {
                // Directory links stay unfollowed; dangling links are tracked for E2.
                if t.is_dir() {
                    continue;
                }
                links.insert(rel.clone(), t);
            }
            if let Ok(md) = std::fs::metadata(entry.path())
                && md.is_file()
            {
                seen.insert(
                    rel,
                    Stat {
                        size: md.len(),
                        mtime: md.modified().ok(),
                        hash: None,
                    },
                );
            }
        }
        let mut by_target: HashMap<PathBuf, BTreeSet<String>> = HashMap::new();
        for (rel, t) in &links {
            by_target.entry(t.clone()).or_default().insert(rel.clone());
        }
        Self {
            root: root.to_path_buf(),
            seen,
            pending: HashMap::new(),
            renames: Vec::new(),
            debounce,
            links,
            by_target,
            links_changed: true,
        }
    }

    /// Records (or clears) `rel`'s symlink target from the current filesystem state.
    fn refresh_link(&mut self, rel: &str) {
        let new = link_target(&self.root.join(rel)).filter(|t| !t.is_dir());
        if self.links.get(rel) == new.as_ref() {
            return;
        }
        if let Some(old) = self.links.remove(rel)
            && let Some(set) = self.by_target.get_mut(&old)
        {
            set.remove(rel);
            if set.is_empty() {
                self.by_target.remove(&old);
            }
        }
        if let Some(t) = new {
            self.by_target
                .entry(t.clone())
                .or_default()
                .insert(rel.to_string());
            self.links.insert(rel.to_string(), t);
        }
        self.links_changed = true;
    }

    /// The directories outside the root holding link targets (one per directory; the root
    /// itself is watched recursively), when the link set changed since the last call.
    pub(crate) fn take_watch_dirs(&mut self) -> Option<BTreeSet<PathBuf>> {
        if !self.links_changed {
            return None;
        }
        self.links_changed = false;
        Some(
            self.by_target
                .keys()
                .filter(|t| !t.starts_with(&self.root))
                .filter_map(|t| t.parent().map(Path::to_path_buf))
                .filter(|d| d.is_dir())
                .collect(),
        )
    }

    fn rel(&self, p: &Path) -> Option<String> {
        rel_path(&self.root, p).filter(|r| !is_ignored(r))
    }

    /// Records a raw notify event as hints.
    pub(crate) fn on_notify(&mut self, ev: &notify::Event) {
        use notify::EventKind as K;
        let now = Instant::now();
        match ev.kind {
            K::Access(AccessKind::Close(AccessMode::Write)) => {}
            K::Access(_) => return,
            K::Modify(ModifyKind::Name(RenameMode::Both)) if ev.paths.len() == 2 => {
                if let (Some(f), Some(t)) = (self.rel(&ev.paths[0]), self.rel(&ev.paths[1])) {
                    self.renames.push((f, t, now));
                    return;
                }
            }
            _ => {}
        }
        for p in &ev.paths {
            if let Some(r) = self.rel(p) {
                self.pending.insert(r, now);
            }
            // A change to a link target maps back to every vault path linking to it.
            if let Some(set) = self.by_target.get(p) {
                for r in set {
                    self.pending.insert(r.clone(), now);
                }
            }
        }
    }

    /// How long until the next pending path is due (`None` = nothing pending).
    pub(crate) fn next_due(&self, now: Instant) -> Option<Duration> {
        self.pending
            .values()
            .copied()
            .chain(self.renames.iter().map(|r| r.2))
            .map(|at| (at + self.debounce).saturating_duration_since(now))
            .min()
    }

    /// Examines every path quiet for the debounce window; applies external changes to the index
    /// on `w` and returns the events.
    pub(crate) fn flush(&mut self, w: &mut VaultWriter) -> Vec<VaultEvent> {
        let now = Instant::now();
        let deb = self.debounce;
        let due = |at: Instant| now.duration_since(at) >= deb;
        let mut out = Vec::new();
        // Renames whose halves are both quiet.
        let busy = |p: &str, pending: &HashMap<String, Instant>| {
            pending.get(p).is_some_and(|at| !due(*at))
        };
        let (ready, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.renames)
            .into_iter()
            .partition(|(f, t, at)| due(*at) && !busy(f, &self.pending) && !busy(t, &self.pending));
        self.renames = keep;
        for (from, to, _) in ready {
            self.pending.remove(&from);
            self.pending.remove(&to);
            if !self.paired_rename(w, &from, &to, &mut out) {
                self.examine(w, &from, &mut out);
                self.examine(w, &to, &mut out);
            }
        }
        let mut paths: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, at)| due(**at))
            .map(|(p, _)| p.clone())
            .collect();
        paths.sort();
        for p in paths {
            self.pending.remove(&p);
            self.examine(w, &p, &mut out);
        }
        out
    }

    /// A notify-paired rename of one file whose content moved unchanged.
    fn paired_rename(
        &mut self,
        w: &mut VaultWriter,
        from: &str,
        to: &str,
        out: &mut Vec<VaultEvent>,
    ) -> bool {
        let to_full = self.root.join(to);
        let Ok(md) = std::fs::metadata(&to_full) else {
            return false;
        };
        if !md.is_file() || self.root.join(from).symlink_metadata().is_ok() {
            return false;
        }
        let Some(prev) = self.seen.get(from) else {
            return false;
        };
        if !prev.same(&md) {
            return false;
        }
        let Ok(bytes) = std::fs::read(&to_full) else {
            return false;
        };
        let h = ContentHash::of(&bytes);
        self.seen.remove(from);
        self.seen.insert(
            to.to_string(),
            Stat {
                size: md.len(),
                mtime: md.modified().ok(),
                hash: Some(h.clone()),
            },
        );
        let daemon = w.echoes.take_match(to, Some(&h));
        w.echoes.take_match(from, None);
        if !daemon {
            let (m, c) = file_times(&md);
            let res = match w.index.rename_file(from, to) {
                Ok(_) | Err(cv_index::Error::NotFound(_)) => {
                    w.index.upsert_file(to, &bytes, m, c).map(|_| ())
                }
                Err(e) => Err(e),
            };
            if let Err(e) = res {
                tracing::warn!(from, to, error = %e, "index update for external rename failed");
            }
        }
        out.push(VaultEvent {
            path: to.to_string(),
            kind: EventKind::Renamed {
                from: from.to_string(),
            },
            origin: if daemon {
                Origin::Daemon
            } else {
                Origin::External
            },
        });
        true
    }

    /// Re-examines one path (a folder expands to its files, current and previously seen).
    fn examine(&mut self, w: &mut VaultWriter, path: &str, out: &mut Vec<VaultEvent>) {
        let full = self.root.join(path);
        let md = std::fs::metadata(&full).ok();
        if md.as_ref().is_some_and(|m| m.is_dir())
            || (md.is_none() && !self.seen.contains_key(path))
        {
            let prefix = format!("{path}/");
            let mut files: Vec<String> = self
                .seen
                .keys()
                .filter(|k| k.starts_with(&prefix))
                .cloned()
                .collect();
            if md.is_some() {
                for e in walkdir::WalkDir::new(&full)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    if (is_file_or_file_link(&e) || e.path_is_symlink())
                        && let Some(r) = self.rel(e.path())
                    {
                        files.push(r);
                    }
                }
            }
            files.sort();
            files.dedup();
            for f in files {
                self.examine_file(w, &f, out);
            }
            return;
        }
        self.examine_file(w, path, out);
    }

    fn examine_file(&mut self, w: &mut VaultWriter, path: &str, out: &mut Vec<VaultEvent>) {
        self.refresh_link(path);
        let full = self.root.join(path);
        let md = match std::fs::metadata(&full) {
            Ok(md) if md.is_file() => md,
            Ok(_) => return,
            Err(_) => {
                let Some(prev) = self.seen.remove(path) else {
                    return; // created and gone between hints: nothing to report
                };
                // The source of a daemon move: examine its destination now, which claims this
                // path (`seen` entry consumed) when it classifies the move as one `Renamed`.
                if let Some(to) = w.echoes.move_dest(path)
                    && to != path
                    && self.root.join(&to).exists()
                {
                    self.seen.insert(path.to_string(), prev);
                    self.pending.remove(&to);
                    self.examine_file(w, &to, out);
                    if self.seen.remove(path).is_none() {
                        return;
                    }
                }
                let daemon = w.echoes.take_match(path, None);
                if !daemon && let Err(e) = w.index.remove_file(path) {
                    tracing::warn!(path, error = %e, "index update for external delete failed");
                }
                out.push(VaultEvent {
                    path: path.to_string(),
                    kind: EventKind::Deleted,
                    origin: if daemon {
                        Origin::Daemon
                    } else {
                        Origin::External
                    },
                });
                return;
            }
        };
        let prev = self.seen.get(path).cloned();
        if prev.as_ref().is_some_and(|p| p.same(&md)) {
            return;
        }
        let bytes = match std::fs::read(&full) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(path, error = %e, "reading changed file failed");
                return;
            }
        };
        let h = ContentHash::of(&bytes);
        let stat = Stat {
            size: md.len(),
            mtime: md.modified().ok(),
            hash: Some(h.clone()),
        };
        self.seen.insert(path.to_string(), stat);
        if prev.as_ref().and_then(|p| p.hash.as_ref()) == Some(&h) {
            return;
        }
        let daemon = w.echoes.take_match(path, Some(&h));
        if !daemon {
            let (m, c) = file_times(&md);
            if let Err(e) = w.index.upsert_file(path, &bytes, m, c) {
                tracing::warn!(path, error = %e, "index update for external change failed");
            }
        }
        // A daemon move's destination, its source gone and not yet reported: one `Renamed`.
        if daemon
            && prev.is_none()
            && let Some(from) = w.echoes.take_move(path)
            && self.root.join(&from).symlink_metadata().is_err()
            && self.seen.remove(&from).is_some()
        {
            w.echoes.take_match(&from, None);
            self.pending.remove(&from);
            self.refresh_link(&from);
            out.push(VaultEvent {
                path: path.to_string(),
                kind: EventKind::Renamed { from },
                origin: Origin::Daemon,
            });
            return;
        }
        out.push(VaultEvent {
            path: path.to_string(),
            kind: if prev.is_none() {
                EventKind::Created
            } else {
                EventKind::Modified
            },
            origin: if daemon {
                Origin::Daemon
            } else {
                Origin::External
            },
        });
    }
}
