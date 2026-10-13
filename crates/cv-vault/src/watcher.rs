//! Watcher event normalization: echo suppression, dedupe, rename pairing (plan WP-7, "Watcher
//! handling").
//!
//! S2 measured ~5 raw events per tmp+rename write (Create/Modify/Name From/To/Both across the
//! temp and the final name — `runs/spike/core-index/FINDINGS.md:64`). Raw events are therefore
//! only *hints*: a path is re-examined once it has been quiet for the debounce window, and its
//! current `(size, mtime)` then hash decide whether anything changed. The writer records
//! `(path → after_hash)` for its own writes; a change matching that echo is `Origin::Daemon` and
//! is not re-applied to the index. Crucible's equivalent (Obsidian's startup event replay and
//! write-queue echo handling) is dropped as Obsidian-induced: the initial scan here only seeds
//! the `(size, mtime)` map and emits nothing (I4).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use notify::event::{AccessKind, AccessMode, ModifyKind, RenameMode};

use crate::fs::{rel_of, times};
use crate::hash::ContentHash;
use crate::path::is_ignored;
use crate::writer::VaultWriter;

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
    ttl: Duration,
}

impl Echoes {
    pub(crate) fn new(ttl: Duration) -> Self {
        Self {
            map: HashMap::new(),
            ttl,
        }
    }

    fn expire(&mut self, now: Instant) {
        let ttl = self.ttl;
        self.map.retain(|_, (_, at)| now.duration_since(*at) < ttl);
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
}

impl WatchState {
    /// Seeds the `(size, mtime)` map from a walk of `root`; emits nothing (I4).
    pub(crate) fn seed(root: &Path, debounce: Duration) -> Self {
        let mut seen = HashMap::new();
        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let Some(rel) = rel_of(root, entry.path()) else {
                continue;
            };
            if is_ignored(&rel) {
                continue;
            }
            if let Ok(md) = entry.metadata() {
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
        Self {
            root: root.to_path_buf(),
            seen,
            pending: HashMap::new(),
            renames: Vec::new(),
            debounce,
        }
    }

    fn rel(&self, p: &Path) -> Option<String> {
        rel_of(&self.root, p).filter(|r| !is_ignored(r))
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
            let (m, c) = times(&md);
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
                    if e.file_type().is_file()
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
        let full = self.root.join(path);
        let md = match std::fs::metadata(&full) {
            Ok(md) if md.is_file() => md,
            Ok(_) => return,
            Err(_) => {
                if self.seen.remove(path).is_none() {
                    return; // created and gone between hints: nothing to report
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
            let (m, c) = times(&md);
            if let Err(e) = w.index.upsert_file(path, &bytes, m, c) {
                tracing::warn!(path, error = %e, "index update for external change failed");
            }
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
