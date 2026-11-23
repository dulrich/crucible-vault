//! Watcher event normalization: echo suppression, dedupe, rename pairing (plan WP-7, "Watcher
//! handling").
//!
//! S2 measured ~5 raw events per tmp+rename write (Create/Modify/Name From/To/Both across the
//! temp and the final name — `docs/spikes/core-index/FINDINGS.md:64`). Raw events are therefore
//! only *hints*: a path is re-examined once it has been quiet for the debounce window, and its
//! current `(size, mtime)` then hash decide whether anything changed. The writer records
//! `(path → after_hash)` for its own writes and reports them itself (`Origin::Daemon`, plan
//! note-path-continuity D1); a change matching that echo is dropped here, so the watcher only
//! reports `Origin::External`. Crucible's equivalent (Obsidian's startup event replay and
//! write-queue echo handling) is dropped as Obsidian-induced: the initial scan here only seeds
//! the `(size, mtime)` map and emits nothing (I4).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use notify::event::{AccessKind, AccessMode, ModifyKind, RenameMode};
use notify::{RecursiveMode, Watcher};

use crate::fs::is_file_or_file_link;
use crate::hash::ContentHash;
use crate::path::is_ignored;
use crate::writer::VaultWriter;
use cv_index::{
    DiscoveredFile, Discovery, Ownership, SkipReason, discover_listing, file_times, rel_path,
};

/// What happened to a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    Created,
    Modified,
    /// Gone; `replaced_by` names the note that absorbed it when the writer deleted it via
    /// `VaultWriter::delete_replaced` (round 11 WP-1, Decision 1; emitted by the writer).
    Deleted {
        replaced_by: Option<String>,
    },
    /// Moved from `from` to the event's path.
    Renamed {
        from: String,
    },
}

/// Who caused it: [`Origin::Daemon`] for the daemon's own mutation (reported by the writer before
/// its call returns), [`Origin::External`] for anyone else's (applied to the index before it is
/// emitted).
pub use cv_protocol::Origin;

/// A normalized vault change. Equality compares the change itself (`path`, `kind`, `origin`);
/// `id` and `hash` are delivery metadata (plan note-path-continuity D2).
#[derive(Debug, Clone)]
pub struct VaultEvent {
    /// Vault-relative path.
    pub path: String,
    pub kind: EventKind,
    pub origin: Origin,
    /// The note id of the file this event concerns (plan note-identity D1; minted on demand,
    /// `None` without an id sink or on a store failure). A rename keeps it; a delete carries the
    /// retired id.
    pub id: Option<crate::NoteId>,
    /// The content hash this change produced (`None` for deletes, or when unknown), captured
    /// at mutation time so the event never reports a later state of the path.
    pub hash: Option<ContentHash>,
}

impl VaultEvent {
    /// An event without delivery metadata.
    #[must_use]
    pub fn new(path: impl Into<String>, kind: EventKind, origin: Origin) -> Self {
        Self {
            path: path.into(),
            kind,
            origin,
            id: None,
            hash: None,
        }
    }
}

impl PartialEq for VaultEvent {
    fn eq(&self, o: &Self) -> bool {
        self.path == o.path && self.kind == o.kind && self.origin == o.origin
    }
}

impl Eq for VaultEvent {}

/// The one typed conversion to the wire's flat [`cv_protocol::FileChange`] (`from` is set only
/// for a rename, `replaced_by` only for a delete). The hash is the one the mutation produced,
/// carried on the event (plan note-path-continuity WP-2); renames and deletes report none.
impl From<VaultEvent> for cv_protocol::FileChange {
    fn from(ev: VaultEvent) -> Self {
        use cv_protocol::ChangeKind;
        let (kind, from, replaced_by) = match ev.kind {
            EventKind::Created => (ChangeKind::Created, None, None),
            EventKind::Modified => (ChangeKind::Modified, None, None),
            EventKind::Deleted { replaced_by } => (ChangeKind::Deleted, None, replaced_by),
            EventKind::Renamed { from } => (ChangeKind::Renamed, Some(from), None),
        };
        let hash = match kind {
            ChangeKind::Created | ChangeKind::Modified => ev.hash.map(|h| h.as_str().to_owned()),
            ChangeKind::Deleted | ChangeKind::Renamed => None,
        };
        Self {
            path: ev.path,
            kind,
            hash,
            origin: ev.origin,
            from,
            replaced_by,
            id: ev.id,
        }
    }
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

/// How a matched echo is handled: the event is always dropped (the writer emitted it); an
/// `Unindexed` echo additionally re-applies the change to the index, because the writer's own
/// index update failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Echo {
    Indexed,
    Unindexed,
}

/// The writer's own recent writes: path → (hash after (`None` = removed), indexed?, when). A
/// watcher fs change matching one is the writer's own mutation, which the writer already reported
/// itself (plan note-path-continuity D1): the watcher drops it and never synthesizes a daemon event.
#[derive(Debug)]
pub(crate) struct Echoes {
    map: HashMap<String, (Option<ContentHash>, Echo, Instant)>,
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
        self.map
            .retain(|_, (_, _, at)| now.duration_since(*at) < ttl);
    }

    pub(crate) fn register(&mut self, path: &str, after: Option<ContentHash>, kind: Echo) {
        let now = Instant::now();
        self.expire(now);
        if self.map.len() >= ECHO_CAP
            && let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, (_, _, at))| *at)
                .map(|(k, _)| k.clone())
        {
            self.map.remove(&oldest);
        }
        self.map.insert(path.to_string(), (after, kind, now));
    }

    /// Whether the writer itself recently removed `path` (not consumed).
    fn is_removal(&self, path: &str) -> bool {
        self.map.get(path).is_some_and(|(h, _, _)| h.is_none())
    }

    /// Whether the writer recently left `path` holding content `h` (not consumed).
    fn is_written(&mut self, path: &str, h: &ContentHash) -> bool {
        self.expire(Instant::now());
        self.map
            .get(path)
            .is_some_and(|(e, _, _)| e.as_ref() == Some(h))
    }

    /// Whether `path`'s current state `cur` is the writer's echo (consumed on match).
    pub(crate) fn take_match(&mut self, path: &str, cur: Option<&ContentHash>) -> Option<Echo> {
        self.expire(Instant::now());
        if self
            .map
            .get(path)
            .is_some_and(|(h, _, _)| h.as_ref() == cur)
        {
            return self.map.remove(path).map(|(_, k, _)| k);
        }
        None
    }
}

#[derive(Debug, Clone)]
struct Stat {
    size: u64,
    mtime: Option<SystemTime>,
    hash: Option<ContentHash>,
    /// The file's canonical location when last seen (a folder-link retarget changes it).
    canonical: Option<PathBuf>,
    /// Re-read even if `(size, mtime)` and content match (an equal-stat retarget).
    force: bool,
}

impl Stat {
    fn of(md: &std::fs::Metadata, canonical: Option<PathBuf>) -> Self {
        Self {
            size: md.len(),
            mtime: md.modified().ok(),
            hash: None,
            canonical,
            force: false,
        }
    }

    fn same(&self, md: &std::fs::Metadata) -> bool {
        !self.force && self.size == md.len() && self.mtime == md.modified().ok()
    }
}

/// How long an unpaired `Name(From)` waits for its `Name(To)` half before the source is examined
/// alone (a move out of the vault, reported `Deleted`). Bounds a notify-thread stall between the
/// halves; without it a stall past the debounce split an external rename into Deleted + Created.
const MOVE_PAIR_GRACE: Duration = Duration::from_millis(500);

/// Per-watcher state, living on the writer thread.
pub(crate) struct WatchState {
    root: PathBuf,
    seen: HashMap<String, Stat>,
    pending: HashMap<String, Instant>,
    renames: Vec<(String, String, Instant)>,
    /// Rename sources (`Name(From)`) still waiting for their `Name(Both)` pairing: notify sends the
    /// halves one at a time, so a stalled notify thread can split them past the debounce. A source
    /// is held for [`MOVE_PAIR_GRACE`] before it may be examined (and reported deleted) alone.
    moving_from: HashMap<String, Instant>,
    debounce: Duration,
    /// Symlinked vault entries → their resolved target file (plan editor-cutover-blockers WP-1).
    links: HashMap<String, PathBuf>,
    /// Target file → the vault paths linking to it.
    by_target: HashMap<PathBuf, BTreeSet<String>>,
    /// Set when `links` changed since the last [`Self::apply_watches`].
    links_changed: bool,
    /// Folder-link ownership of the last discovery (plan folder-links-follow WP-2b): translates
    /// canonical event paths to their owning vault path and names the watch set.
    own: Ownership,
    /// Vault paths of folder-link entries (owned link roots and skipped links): an event at or
    /// under one is a topology change.
    link_paths: BTreeSet<String>,
    /// The repo-owned `NonRecursive` registrations, keyed by canonical directory (all outside
    /// the vault root; the root itself is the one `Recursive` watch).
    registered: BTreeSet<PathBuf>,
    /// Registrations whose directory went away (removed, moved out, `MOVE_SELF`): dropped at the
    /// next resync so a directory reappearing at the same path is watched afresh.
    dropped: BTreeSet<PathBuf>,
    /// When the link topology last changed (ownership must be recomputed once it is quiet).
    topology: Option<Instant>,
    /// An overflow (`Flag::Rescan`) was seen: full reconcile and re-registration.
    rescan: bool,
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

/// The nearest existing directory at or above `p`, canonical.
fn nearest_existing(p: &Path) -> Option<PathBuf> {
    let mut cur = Some(p);
    while let Some(c) = cur {
        if c.is_dir() {
            return std::fs::canonicalize(c).ok();
        }
        cur = c.parent();
    }
    None
}

/// One discovery of `root` for the watcher: every file (excluded ones flagged, dot/temp
/// directories pruned) and the ownership map — the same walk the index uses (WP-2a).
fn discover_root(root: &Path) -> Option<Discovery> {
    match discover_listing(root, &|rel| is_ignored(rel)) {
        Ok(d) => Some(d),
        Err(e) => {
            tracing::warn!(error = %e, "watcher: vault discovery failed");
            None
        }
    }
}

impl WatchState {
    /// Seeds the `(size, mtime)` map and the ownership map from one discovery of `root`; emits
    /// nothing (I4). Registers no watches: the caller's first [`Self::apply_watches`] does.
    pub(crate) fn seed(root: &Path, debounce: Duration) -> Self {
        let mut st = Self {
            root: root.to_path_buf(),
            seen: HashMap::new(),
            pending: HashMap::new(),
            renames: Vec::new(),
            moving_from: HashMap::new(),
            debounce,
            links: HashMap::new(),
            by_target: HashMap::new(),
            links_changed: true,
            own: Ownership::default(),
            link_paths: BTreeSet::new(),
            registered: BTreeSet::new(),
            dropped: BTreeSet::new(),
            topology: None,
            rescan: false,
        };
        let Some(mut disc) = discover_root(root) else {
            return st;
        };
        let files = std::mem::take(&mut disc.files);
        st.set_ownership(disc.ownership);
        for f in &files {
            if is_ignored(&f.path) {
                continue;
            }
            st.seen
                .insert(f.path.clone(), Stat::of(&f.meta, Some(f.canonical.clone())));
        }
        st.refresh_links(&files);
        st
    }

    fn set_ownership(&mut self, own: Ownership) {
        let mut lp: BTreeSet<String> = own.skipped().map(|(p, _)| p.to_string()).collect();
        for (dir, canon) in own.dirs() {
            if dir.is_empty() {
                continue;
            }
            let (parent, name) = dir.rsplit_once('/').unwrap_or(("", dir));
            let physical = own
                .canonical_of(parent)
                .is_some_and(|pc| pc.join(name) == canon);
            if !physical {
                lp.insert(dir.to_string());
            }
        }
        self.own = own;
        self.link_paths = lp;
    }

    /// Records every file link and dangling link of a discovery.
    fn refresh_links(&mut self, files: &[DiscoveredFile]) {
        let dangling: Vec<String> = self
            .own
            .skipped()
            .filter(|(_, r)| *r == SkipReason::Dangling)
            .map(|(p, _)| p.to_string())
            .collect();
        for f in files.iter().filter(|f| f.file_link && !is_ignored(&f.path)) {
            self.refresh_link(&f.path);
        }
        for p in dangling {
            self.refresh_link(&p);
        }
    }

    /// Whether `rel` is a folder-link entry or has one below it.
    fn linked_under(&self, rel: &str) -> bool {
        if self.link_paths.contains(rel) {
            return true;
        }
        let prefix = format!("{rel}/");
        self.link_paths
            .range(prefix.clone()..)
            .next()
            .is_some_and(|p| p.starts_with(&prefix))
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

    /// The directories the owned set should watch: every folder-link subtree directory outside
    /// the vault root (descendants included — one `NonRecursive` watch each), every file-link
    /// target directory, and the nearest existing ancestor of each dangling link's target (so
    /// its appearance is seen). Canonical and deduped, so no two registrations share an inode;
    /// never inside the vault root (the root's own recursive watch covers it).
    pub(crate) fn want_dirs(&self) -> BTreeSet<PathBuf> {
        let croot = self.own.canonical_root().to_path_buf();
        let mut want: BTreeSet<PathBuf> = self.own.external_dirs().map(Path::to_path_buf).collect();
        for t in self.by_target.keys() {
            if let Some(d) = t.parent().and_then(nearest_existing) {
                want.insert(d);
            }
        }
        for (rel, r) in self.own.skipped() {
            if r == SkipReason::Dangling
                && let Some(t) = link_target(&self.root.join(rel))
                && let Some(d) = nearest_existing(&t)
            {
                want.insert(d);
            }
        }
        let root = std::fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());
        want.retain(|d| {
            !d.starts_with(&croot) && !d.starts_with(&root) && !croot.as_os_str().is_empty()
        });
        want
    }

    /// Brings the owned registrations in line with [`Self::want_dirs`] (dropping the ones whose
    /// directory went away first, so they are re-added fresh), then checks every newly
    /// registered directory for files that appeared before its watch (audit E3). Returns the
    /// number of new registrations.
    pub(crate) fn apply_watches<W: Watcher>(&mut self, watcher: &mut W) -> usize {
        self.links_changed = false;
        for d in std::mem::take(&mut self.dropped) {
            if self.registered.remove(&d)
                && let Err(e) = watcher.unwatch(&d)
            {
                tracing::debug!(error = %e, "unwatching a vanished directory");
            }
        }
        let want = self.want_dirs();
        for d in self.registered.difference(&want) {
            if let Err(e) = watcher.unwatch(d) {
                tracing::debug!(error = %e, "unwatching an owned directory");
            }
        }
        self.registered.retain(|d| want.contains(d));
        let mut fresh = Vec::new();
        for d in want {
            if self.registered.contains(&d) {
                continue;
            }
            match watcher.watch(&d, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    self.registered.insert(d.clone());
                    fresh.push(d);
                }
                Err(e) => tracing::warn!(error = %e, "watching an owned directory failed"),
            }
        }
        // Queued already due: the resync itself is the settled state.
        let now = Instant::now()
            .checked_sub(self.debounce)
            .unwrap_or_else(Instant::now);
        for d in &fresh {
            let Ok(rd) = std::fs::read_dir(d) else {
                continue;
            };
            for e in rd.filter_map(|e| e.ok()) {
                if !e.file_type().is_ok_and(|t| t.is_file() || t.is_symlink()) {
                    continue;
                }
                let p = e.path();
                let Ok(md) = std::fs::metadata(&p) else {
                    continue;
                };
                for r in self.vault_paths(&p) {
                    if self.seen.get(&r).is_none_or(|s| !s.same(&md)) {
                        self.pending.insert(r, now);
                    }
                }
            }
        }
        fresh.len()
    }

    /// The file-link set changed since the last [`Self::apply_watches`].
    pub(crate) fn links_changed(&self) -> bool {
        self.links_changed
    }

    /// Whether a resync is due: `Some(true)` after an overflow, `Some(false)` once a topology
    /// change has been quiet for the debounce window.
    pub(crate) fn resync_due(&self, now: Instant) -> Option<bool> {
        if self.rescan {
            return Some(true);
        }
        self.topology
            .filter(|at| now.duration_since(*at) >= self.debounce)
            .map(|_| false)
    }

    /// The one refresh seam (plan folder-links-follow WP-2b) a link change, a `.gitignore`
    /// rebuild and an overflow share: a fresh discovery replaces the ownership map (watcher and
    /// index), the watch set is brought in line — `full` first drops and re-adds the root and
    /// every owned registration, never keeping one because its path is unchanged (cycle-1 B2) —
    /// and every path whose discovered state differs from what was seen (new, gone, changed
    /// stat or canonical location, declined) is queued for examination.
    pub(crate) fn resync<W: Watcher>(&mut self, w: &mut VaultWriter, watcher: &mut W, full: bool) {
        self.topology = None;
        self.rescan = false;
        if full {
            if let Err(e) = watcher.unwatch(&self.root) {
                tracing::debug!(error = %e, "unwatching the vault root for a rescan");
            }
            if let Err(e) = watcher.watch(&self.root, RecursiveMode::Recursive) {
                tracing::warn!(error = %e, "re-watching the vault root failed");
            }
            for d in std::mem::take(&mut self.registered) {
                if let Err(e) = watcher.unwatch(&d) {
                    tracing::debug!(error = %e, "unwatching an owned directory for a rescan");
                }
            }
            self.dropped.clear();
        }
        let Some(mut disc) = discover_root(&self.root) else {
            return;
        };
        let files = std::mem::take(&mut disc.files);
        self.set_ownership(disc.ownership.clone());
        w.index.refresh_discovery(&self.root, disc);
        self.refresh_links(&files);
        let now = Instant::now()
            .checked_sub(self.debounce)
            .unwrap_or_else(Instant::now);
        let mut listed = std::collections::HashSet::new();
        for f in &files {
            if is_ignored(&f.path) {
                continue;
            }
            listed.insert(f.path.as_str());
            match self.seen.get_mut(&f.path) {
                None => {
                    self.pending.insert(f.path.clone(), now);
                }
                Some(s) if !s.same(&f.meta) => {
                    self.pending.insert(f.path.clone(), now);
                }
                Some(s) if s.canonical.as_ref() != Some(&f.canonical) => {
                    // A live equal-stat retarget: the content must be re-read and reindexed.
                    s.force = true;
                    self.pending.insert(f.path.clone(), now);
                }
                Some(_) => {}
            }
        }
        let gone: Vec<String> = self
            .seen
            .keys()
            .filter(|k| !listed.contains(k.as_str()))
            .cloned()
            .collect();
        for k in gone {
            self.pending.insert(k, now);
        }
        let n = self.apply_watches(watcher);
        tracing::debug!(
            full,
            registered = self.registered.len(),
            new = n,
            "watch set resynced"
        );
    }

    /// Every vault path an event at `p` concerns: `p` itself inside the root, else the owning
    /// vault path of a canonical directory or entry (WP-2b translation); plus every file link
    /// whose target is `p`.
    fn vault_paths(&self, p: &Path) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(r) = rel_path(&self.root, p) {
            if !is_ignored(&r) {
                out.push(r);
            }
        } else if let Some(r) = self
            .own
            .owner_of(p)
            .map(str::to_string)
            .or_else(|| self.own.vault_path_of(p))
            && !r.is_empty()
            && !is_ignored(&r)
        {
            out.push(r);
        }
        if let Some(set) = self.by_target.get(p) {
            out.extend(set.iter().cloned());
        }
        out
    }

    fn rel(&self, p: &Path) -> Option<String> {
        self.vault_paths(p).into_iter().next()
    }

    /// Notes a topology change at raw event path `p`: a folder-link entry created, removed or
    /// retargeted in the vault, a directory moved into or out of the vault, a `.gitignore`
    /// change, or a directory created, moved or removed outside the root (an owned subtree
    /// growing or shrinking, a dangling target appearing). A directory that went away loses its
    /// registrations.
    fn note_topology(&mut self, kind: &notify::EventKind, p: &Path, now: Instant) {
        use notify::EventKind as K;
        use notify::event::{CreateKind, RemoveKind};
        let dir_kind = matches!(
            kind,
            K::Create(CreateKind::Folder)
                | K::Remove(RemoveKind::Folder)
                | K::Modify(ModifyKind::Name(_))
        );
        let mut dirty = p.file_name().is_some_and(|n| n == cv_index::GITIGNORE);
        let lmd = p.symlink_metadata().ok();
        let is_dir = lmd.as_ref().is_some_and(std::fs::Metadata::is_dir);
        if let Some(rel) = rel_path(&self.root, p) {
            let is_link = lmd.as_ref().is_some_and(|m| m.file_type().is_symlink());
            if is_link && (p.is_dir() || std::fs::metadata(p).is_err()) {
                dirty = true;
            }
            if (is_link || dir_kind || lmd.is_none()) && self.linked_under(&rel) {
                dirty = true;
            }
            if is_dir && matches!(kind, K::Modify(ModifyKind::Name(_))) {
                dirty = true; // a directory moved in may hold folder links
            }
        } else if dir_kind {
            if is_dir || self.own.owner_of(p).is_some() || self.registered.contains(p) {
                dirty = true;
            }
            if !is_dir {
                let gone: Vec<PathBuf> = self
                    .registered
                    .iter()
                    .filter(|d| d.starts_with(p))
                    .cloned()
                    .collect();
                self.dropped.extend(gone);
            }
        }
        if dirty {
            self.topology = Some(now);
        }
    }

    /// Records a raw notify event as hints.
    pub(crate) fn on_notify(&mut self, ev: &notify::Event) {
        use notify::EventKind as K;
        if ev.need_rescan() {
            self.rescan = true;
            return;
        }
        let now = Instant::now();
        if !matches!(ev.kind, K::Access(_)) {
            for p in &ev.paths {
                self.note_topology(&ev.kind, p, now);
            }
        }
        match ev.kind {
            K::Access(AccessKind::Close(AccessMode::Write)) => {}
            K::Access(_) => return,
            K::Modify(ModifyKind::Name(RenameMode::From)) => {
                for p in &ev.paths {
                    if let Some(r) = self.rel(p) {
                        self.moving_from.insert(r, now);
                    }
                }
            }
            K::Modify(ModifyKind::Name(RenameMode::Both)) if ev.paths.len() == 2 => {
                let f = rel_path(&self.root, &ev.paths[0]).or_else(|| self.rel(&ev.paths[0]));
                if let Some(f) = f {
                    self.moving_from.remove(&f);
                }
                if let (Some(f), Some(t)) = (self.rel(&ev.paths[0]), self.rel(&ev.paths[1])) {
                    self.renames.push((f, t, now));
                    return;
                }
            }
            _ => {}
        }
        for p in &ev.paths {
            for r in self.vault_paths(p) {
                self.pending.insert(r, now);
            }
        }
    }

    /// How long until the next pending path is due (`None` = nothing pending).
    pub(crate) fn next_due(&self, now: Instant) -> Option<Duration> {
        self.pending
            .iter()
            .map(|(p, at)| self.hold_until(p, *at))
            .chain(self.renames.iter().map(|r| r.2))
            .chain(self.topology)
            .map(|at| (at + self.debounce).saturating_duration_since(now))
            .min()
    }

    /// When pending `path` (last hinted at `at`) may be examined: quiet for the debounce window,
    /// and past the pairing grace if it is an unpaired rename source.
    fn hold_until(&self, path: &str, at: Instant) -> Instant {
        let grace = self
            .moving_from
            .get(path)
            .map(|t| *t + MOVE_PAIR_GRACE.saturating_sub(self.debounce));
        grace.map_or(at, |g| g.max(at))
    }

    /// Examines every path quiet for the debounce window; applies external changes to the index
    /// on `w` and returns the events.
    pub(crate) fn flush(&mut self, w: &mut VaultWriter) -> Vec<VaultEvent> {
        // A pending topology change is applied first (the resync seam), so no path is examined
        // against a stale ownership map.
        if self.topology.is_some() || self.rescan {
            return Vec::new();
        }
        let now = Instant::now();
        let deb = self.debounce;
        let due = |at: Instant| now.duration_since(at) >= deb;
        // The writer's own moves (into `.trash`, or by link + unlink) need no pairing grace.
        self.moving_from.retain(|p, _| !w.echoes.is_removal(p));
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
        // A half of a rename still waiting to pair waits with it (its hint and the pairing land
        // microseconds apart, so the hint alone can fall due first).
        let pairing: std::collections::HashSet<&str> = self
            .renames
            .iter()
            .flat_map(|(f, t, _)| [f.as_str(), t.as_str()])
            .collect();
        let mut paths: Vec<String> = self
            .pending
            .iter()
            .filter(|(p, at)| due(self.hold_until(p, **at)) && !pairing.contains(p.as_str()))
            .map(|(p, _)| p.clone())
            .collect();
        // Declined paths first, so an alias id moves to its winner before the winner is bound.
        paths.sort_by_key(|p| (w.index.admit(p).is_ok(), p.clone()));
        for p in paths {
            self.pending.remove(&p);
            self.moving_from.remove(&p);
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
        // `from` is either a path the watcher has seen unchanged, or one the writer just put
        // there and the watcher has not flushed yet (its live echo carries the content hash).
        let seen_same = match self.seen.get(from) {
            Some(prev) if !prev.same(&md) => return false,
            Some(_) => true,
            None => false,
        };
        let Ok(bytes) = std::fs::read(&to_full) else {
            return false;
        };
        let h = ContentHash::of(&bytes);
        if !seen_same && !w.echoes.is_written(from, &h) {
            return false;
        }
        self.seen.remove(from);
        let mut st = Stat::of(&md, std::fs::canonicalize(&to_full).ok());
        st.hash = Some(h.clone());
        self.seen.insert(to.to_string(), st);
        let echo = w.echoes.take_match(to, Some(&h));
        w.echoes.take_match(from, None);
        if !seen_same {
            w.echoes.take_match(from, Some(&h)); // the writer's state at `from`, now moved on
        }
        if echo == Some(Echo::Indexed) {
            return true; // the writer's own move: already reported by the writer
        }
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
        if echo.is_some() {
            return true;
        }
        let id = external_id(w.moved_external(from, to));
        out.push(VaultEvent {
            path: to.to_string(),
            kind: EventKind::Renamed {
                from: from.to_string(),
            },
            origin: Origin::External,
            id,
            hash: Some(ContentHash::of(&bytes)),
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
        if w.index.admit(path).is_err() {
            self.retire_declined(w, path, out);
            return;
        }
        let full = self.root.join(path);
        let md = match std::fs::metadata(&full) {
            Ok(md) if md.is_file() => md,
            Ok(_) => return,
            Err(_) => {
                if self.seen.remove(path).is_none() {
                    return; // created and gone between hints: nothing to report
                }
                match w.echoes.take_match(path, None) {
                    Some(Echo::Indexed) => {}
                    echo => {
                        if let Err(e) = w.index.remove_file(path) {
                            tracing::warn!(path, error = %e, "index update for external delete failed");
                        }
                        if echo.is_none() {
                            let id = external_id(w.note_ids_mut().retired(path, None));
                            out.push(VaultEvent {
                                path: path.to_string(),
                                kind: EventKind::Deleted { replaced_by: None },
                                origin: Origin::External,
                                id,
                                hash: None,
                            });
                        }
                    }
                }
                return;
            }
        };
        let prev = self.seen.get(path).cloned();
        if prev.as_ref().is_some_and(|p| p.same(&md)) {
            return;
        }
        let forced = prev.as_ref().is_some_and(|p| p.force);
        let bytes = match std::fs::read(&full) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(path, error = %e, "reading changed file failed");
                return;
            }
        };
        let h = ContentHash::of(&bytes);
        let mut stat = Stat::of(&md, std::fs::canonicalize(&full).ok());
        stat.hash = Some(h.clone());
        self.seen.insert(path.to_string(), stat);
        if !forced && prev.as_ref().and_then(|p| p.hash.as_ref()) == Some(&h) {
            return;
        }
        let echo = w.echoes.take_match(path, Some(&h));
        if echo != Some(Echo::Indexed) {
            let (m, c) = file_times(&md);
            if let Err(e) = w.index.upsert_file(path, &bytes, m, c) {
                tracing::warn!(path, error = %e, "index update for external change failed");
            }
        }
        if echo.is_some() {
            return; // the writer's own mutation: already reported by the writer
        }
        let id = external_id(w.bind_external(path));
        out.push(VaultEvent {
            path: path.to_string(),
            kind: if prev.is_none() {
                EventKind::Created
            } else {
                EventKind::Modified
            },
            origin: Origin::External,
            id,
            hash: Some(h),
        });
    }
}

impl WatchState {
    /// A seen path the ownership map now declines (its link lost to another alias, looped or
    /// went dangling — Decision 5): its row is dropped, and its id follows the WP-2a rule — it
    /// moves to the winning path when that has none (reported as a rename), else it retires.
    fn retire_declined(&mut self, w: &mut VaultWriter, path: &str, out: &mut Vec<VaultEvent>) {
        if self.seen.remove(path).is_none() {
            return;
        }
        if let Err(e) = w.index.remove_file(path) {
            tracing::warn!(path, error = %e, "index update for a declined path failed");
        }
        let winner = w.index.ownership().winner_of(path);
        let id = external_id(w.note_ids_mut().retired(path, None));
        if let (Some(win), Some(id)) = (&winner, id)
            && w.note_ids_mut().revive(id, win).unwrap_or(false)
        {
            out.push(VaultEvent {
                path: win.clone(),
                kind: EventKind::Renamed {
                    from: path.to_string(),
                },
                origin: Origin::External,
                id: Some(id),
                hash: None,
            });
            return;
        }
        out.push(VaultEvent {
            path: path.to_string(),
            kind: EventKind::Deleted { replaced_by: None },
            origin: Origin::External,
            id,
            hash: None,
        });
    }
}

/// An External transition's id: there is no caller to report a store failure to, so it is logged
/// and the event carries no id; startup repair reconciles a row left stale (Decision 3/4).
fn external_id(
    r: crate::note_id::IdResult<crate::note_id::NoteId>,
) -> Option<crate::note_id::NoteId> {
    r.map_err(|e| tracing::warn!(error = %e, "note id: external transition not recorded"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{EventKind as K, ModifyKind, RenameMode};

    fn name(mode: RenameMode, paths: &[&Path]) -> notify::Event {
        paths.iter().fold(
            notify::Event::new(K::Modify(ModifyKind::Name(mode))),
            |e, p| e.add_path(p.to_path_buf()),
        )
    }

    /// WP-3 (plan test-flake-pass): a rename source stays held past the debounce until its
    /// `Both` half pairs it, so a notify-thread stall cannot split an external rename.
    #[test]
    fn unpaired_rename_source_is_held_until_paired() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("c.md"), "C").unwrap();
        let deb = Duration::from_millis(50);
        let mut st = WatchState::seed(dir.path(), deb);
        let (c, d) = (dir.path().join("c.md"), dir.path().join("d.md"));
        st.on_notify(&name(RenameMode::From, &[&c]));
        let wait = st.next_due(Instant::now()).unwrap();
        assert!(wait > deb * 4, "From half held for the grace, got {wait:?}");
        st.on_notify(&name(RenameMode::To, &[&d]));
        st.on_notify(&name(RenameMode::Both, &[&c, &d]));
        assert!(st.moving_from.is_empty());
        assert!(st.next_due(Instant::now()).unwrap() <= deb);
    }

    /// The typed event → flat wire input: `from` only on a rename, `replaced_by` only on a
    /// delete, and only a create/modify carries its hash.
    #[test]
    fn event_converts_to_file_change() {
        use cv_protocol::{ChangeKind, FileChange, NoteId};
        let mut ev = VaultEvent::new(
            "b.md",
            EventKind::Renamed {
                from: "a.md".into(),
            },
            Origin::Daemon,
        );
        ev.id = Some(NoteId(3));
        ev.hash = Some(ContentHash::of(b"x"));
        let c = FileChange::from(ev);
        assert_eq!(
            (c.kind, c.from.as_deref(), c.hash, c.id),
            (ChangeKind::Renamed, Some("a.md"), None, Some(NoteId(3)))
        );
        let mut ev = VaultEvent::new("b.md", EventKind::Modified, Origin::External);
        ev.hash = Some(ContentHash::of(b"x"));
        let c = FileChange::from(ev);
        assert_eq!(
            (c.kind, c.from, c.hash),
            (
                ChangeKind::Modified,
                None,
                Some(ContentHash::of(b"x").as_str().to_owned())
            )
        );
        let c = FileChange::from(VaultEvent::new(
            "a.md",
            EventKind::Deleted {
                replaced_by: Some("k.md".into()),
            },
            Origin::Daemon,
        ));
        assert_eq!(
            (c.kind, c.from, c.replaced_by.as_deref()),
            (ChangeKind::Deleted, None, Some("k.md"))
        );
    }
}
