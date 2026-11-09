//! [`VaultWriter`]: the single writer (I2). Owns the [`VaultFs`] and the `index.db` writer handle;
//! every vault mutation goes through it as a content-hash CAS.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

use cv_index::{Index, LinkRow};

use crate::error::{Result, VaultError};
use crate::fs::VaultFs;
use crate::hash::ContentHash;
use crate::lineage::{LineageSink, NoLineage};
use crate::path::validate_rel_path;
use crate::rewrite::rewrite_note;
use crate::trash::{TrashReason, TrashRecord};
use crate::watcher::{Echo, Echoes, EventKind, Origin, VaultEvent};
use cv_index::file_times;

/// What a rename did to inbound links.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenameOutcome {
    /// Source notes whose links were rewritten (post-rename paths), sorted.
    pub rewritten: Vec<String>,
    /// Source notes skipped because they changed since the index saw them (never clobbered).
    pub skipped_conflicts: Vec<String>,
    /// Folder renames only: files moved (post-rename paths), sorted.
    pub moved: Vec<String>,
    /// Folder renames only: `(source path, reason)` for files left in place (pre-flight
    /// collision, hash read failure, or a per-file move error), sorted.
    pub failed: Vec<(String, String)>,
}

impl RenameOutcome {
    fn merge(&mut self, other: RenameOutcome) {
        self.rewritten.extend(other.rewritten);
        self.skipped_conflicts.extend(other.skipped_conflicts);
        self.rewritten.sort();
        self.rewritten.dedup();
        self.skipped_conflicts.sort();
        self.skipped_conflicts.dedup();
        self.moved.extend(other.moved);
        self.moved.sort();
        self.failed.extend(other.failed);
        self.failed.sort();
    }
}

/// The vault writer; see the module docs. Run it on one thread ([`crate::spawn_writer`]).
pub struct VaultWriter {
    pub(crate) fs: VaultFs,
    pub(crate) index: Index,
    pub(crate) echoes: Echoes,
    /// Trash records not yet handed to a sink (plan tidiness-trash-orphans WP-1).
    trash_records: Vec<TrashRecord>,
    trash_sink: Option<TrashSink>,
    /// Writer-authored events (plan note-path-continuity D1); `None` without a watcher.
    events: Option<Sender<VaultEvent>>,
    lineage: Box<dyn LineageSink>,
}

/// Receives each [`TrashRecord`] right after its move (the daemon persists it to `trash_entries`).
/// Runs on the writer thread: it must only hand the record off (e.g. a channel send), never block.
pub type TrashSink = Box<dyn FnMut(TrashRecord) + Send>;

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
            trash_records: Vec::new(),
            trash_sink: None,
            events: None,
            lineage: Box::new(NoLineage),
        }
    }

    /// Opens the index at base path `index_base` for `fs.root()`, creating it when there is none
    /// and rebuilding it on a schema/generation mismatch (D14: derived, never migrated).
    pub fn open(fs: VaultFs, index_base: &Path) -> Result<Self> {
        let index = match Index::open(index_base) {
            Ok(mut i) => {
                i.load_ignore(fs.root());
                // Catch up with changes made while the daemon was stopped (I4: no events).
                let started = std::time::Instant::now();
                match i.reconcile(fs.root()) {
                    Ok(c) => {
                        tracing::info!(
                            added = c.added,
                            updated = c.updated,
                            removed = c.removed,
                            unchanged = c.unchanged,
                            elapsed_ms =
                                u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                            "index reconciled with the vault"
                        );
                        i
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "index reconcile failed; rebuilding");
                        drop(i);
                        Index::create(fs.root(), index_base)?
                    }
                }
            }
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

    /// The filesystem, mutably.
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
                    Ok(mut i) => {
                        i.load_ignore(&root);
                        self.index = i;
                    }
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
    /// the echo is recorded [`Echo::Unindexed`], so the watcher re-applies the change to the
    /// index (but still drops the event: the writer reports it).
    fn index_written(&mut self, path: &str, bytes: &[u8], full: &Path, hash: &ContentHash) {
        let res = std::fs::metadata(full)
            .map_err(|e| crate::error::io(full, e))
            .and_then(|md| {
                let (m, c) = file_times(&md);
                Ok(self.index.upsert_file(path, bytes, m, c)?)
            });
        match res {
            Ok(_) => self
                .echoes
                .register(path, Some(hash.clone()), Echo::Indexed),
            Err(e) => {
                tracing::warn!(path, error = %e, "index update after write failed");
                self.echoes
                    .register(path, Some(hash.clone()), Echo::Unindexed);
            }
        }
    }

    /// Hands the writer its event sender (the watcher thread's channel).
    pub(crate) fn set_event_sender(&mut self, tx: Sender<VaultEvent>) {
        self.events = Some(tx);
    }

    /// Installs the lineage sink (plan note-path-continuity D2); the default records nothing.
    pub fn set_lineage_sink(&mut self, sink: Box<dyn LineageSink>) {
        self.lineage = sink;
    }

    /// The lineage sink (the watcher records External transitions through it).
    pub(crate) fn lineage_mut(&mut self) -> &mut dyn LineageSink {
        self.lineage.as_mut()
    }

    /// Emits one writer-authored (`Origin::Daemon`) event, synchronously.
    fn emit(&self, path: &str, kind: EventKind) {
        if let Some(tx) = &self.events
            && tx
                .send(VaultEvent {
                    path: path.to_string(),
                    kind,
                    origin: Origin::Daemon,
                })
                .is_err()
        {
            tracing::debug!("vault event dropped: no subscriber");
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
        let created = expected.is_none();
        let (h, full) = self.fs.put(path, bytes, expected.as_ref())?;
        self.index_written(path, bytes, &full, &h);
        if created {
            self.lineage.created(path);
            self.emit(path, EventKind::Created);
        } else {
            self.emit(path, EventKind::Modified);
        }
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

    /// Recoverable delete (into `.trash/`). CAS on `expected`.
    pub fn delete(&mut self, path: &str, expected: ContentHash) -> Result<()> {
        self.delete_with(path, expected, false)
    }

    /// Round 11 WP-1 (Decision 1): [`Self::delete`] of a note absorbed by `by` (auto-file's
    /// replacement / containment trash). The writer emits `Deleted { replaced_by: Some(by) }`
    /// itself, so the bridge sends
    /// `file_changed { kind: deleted, replaced_by }` and clients retarget instead.
    pub fn delete_replaced(&mut self, path: &str, expected: ContentHash, by: &str) -> Result<()> {
        self.remove(path, expected, false, TrashReason::Replaced, Some(by))
    }

    /// Recoverable delete recorded with `reason` (WP-2's sweep passes [`TrashReason::Orphan`]).
    pub fn trash_with_reason(
        &mut self,
        path: &str,
        expected: ContentHash,
        reason: TrashReason,
    ) -> Result<()> {
        self.remove(path, expected, false, reason, None)
    }

    /// Installs the trash-record sink; records queued before it are handed over now.
    pub fn set_trash_sink(&mut self, mut sink: TrashSink) {
        for r in self.trash_records.drain(..) {
            sink(r);
        }
        self.trash_sink = Some(sink);
    }

    /// Drains the queued trash records (only filled while no sink is installed).
    pub fn take_trash_records(&mut self) -> Vec<TrashRecord> {
        std::mem::take(&mut self.trash_records)
    }

    /// [`Self::delete`] with a mode: `permanent` removes the file outright instead of the
    /// `.trash/` move.
    pub fn delete_with(
        &mut self,
        path: &str,
        expected: ContentHash,
        permanent: bool,
    ) -> Result<()> {
        self.remove(path, expected, permanent, TrashReason::Deleted, None)
    }

    /// The one delete path: CAS remove, then a [`TrashRecord`] for a recoverable move (only
    /// after the move succeeded, so a record never names a file that was not moved).
    fn remove(
        &mut self,
        path: &str,
        expected: ContentHash,
        permanent: bool,
        reason: TrashReason,
        replaced_by: Option<&str>,
    ) -> Result<()> {
        if let Some(moved) = self.fs.remove_with(path, &expected, permanent)? {
            let trashed_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
            let rec = TrashRecord {
                moved,
                original_path: path.to_string(),
                trashed_ms,
                reason,
            };
            match &mut self.trash_sink {
                Some(s) => s(rec),
                None => self.trash_records.push(rec),
            }
        }
        match self.index.remove_file(path) {
            Ok(_) => self.echoes.register(path, None, Echo::Indexed),
            Err(e) => {
                tracing::warn!(path, error = %e, "index update after delete failed");
                self.echoes.register(path, None, Echo::Unindexed);
            }
        }
        match replaced_by {
            Some(by) => self.lineage.replaced(path, by),
            None => self.lineage.deleted(path),
        }
        self.emit(
            path,
            EventKind::Deleted {
                replaced_by: replaced_by.map(str::to_string),
            },
        );
        Ok(())
    }

    /// Deletes `note` (CAS on its current hash, `permanent` as in [`Self::delete_with`]), then
    /// each of `attachments` that no indexed file other than `note` references *at this moment*
    /// (plan editor-ui-round-3 WP-3: never removes a path referenced after the preview). Returns
    /// `(removed, skipped)`; `removed` starts with the note.
    pub fn delete_note(
        &mut self,
        note: &str,
        attachments: &[String],
        permanent: bool,
    ) -> Result<(Vec<String>, Vec<String>)> {
        let h = self
            .fs
            .hash(note)?
            .ok_or_else(|| VaultError::NotFound(note.to_string()))?;
        self.delete_with(note, h, permanent)?;
        let mut removed = vec![note.to_string()];
        let mut skipped = Vec::new();
        for a in attachments {
            if a == note || cv_index::referenced_elsewhere(&self.index.query(), a, note)? {
                skipped.push(a.clone());
                continue;
            }
            let Some(ah) = self.fs.hash(a)? else {
                skipped.push(a.clone());
                continue;
            };
            match self.remove(a, ah, permanent, TrashReason::NoteAttachment, None) {
                Ok(()) => removed.push(a.clone()),
                Err(e) => {
                    tracing::warn!(path = %a, error = %e, "delete_note: attachment kept");
                    skipped.push(a.clone());
                }
            }
        }
        Ok((removed, skipped))
    }

    /// Whether the index's view of `src` still matches what the filesystem holds (size and mtime
    /// equal the indexed row).
    fn fresh(&self, src: &str, bytes: &[u8]) -> Result<bool> {
        let Some(row) = self.index.query().file(src)? else {
            return Ok(false);
        };
        let full = self.fs.root().join(src);
        let md = std::fs::metadata(&full).map_err(|e| crate::error::io(&full, e))?;
        let (m, _) = file_times(&md);
        Ok(usize::try_from(row.size).ok() == Some(bytes.len()) && row.mtime == m)
    }

    /// The inbound links of `from`, grouped by source note: the notes a rename of `from` rewrites.
    fn backlink_groups(&self, from: &str) -> Result<BTreeMap<String, Vec<LinkRow>>> {
        let mut groups: BTreeMap<String, Vec<LinkRow>> = BTreeMap::new();
        for row in self.index.query().backlinks(from)? {
            groups.entry(row.src_path.clone()).or_default().push(row);
        }
        Ok(groups)
    }

    /// Every path renaming `from` to `to` touches: both endpoints plus the backlink sources it
    /// rewrites (the same grouping [`Self::rename`] gathers). Daemon hold checks use it.
    pub fn rename_touched(&self, from: &str, to: &str) -> Result<Vec<String>> {
        let mut touched = vec![from.to_owned(), to.to_owned()];
        touched.extend(self.backlink_groups(from)?.into_keys());
        Ok(touched)
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
        let mut groups = self.backlink_groups(from)?;
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
            match self.fs.read(src)? {
                Some(b) if self.fresh(src, &b)? => {
                    pre.insert(src.clone(), ContentHash::of(&b));
                }
                _ => outcome.skipped_conflicts.push(new_src.to_string()),
            }
        }
        let moved = self.index.query().file(from)?.map(|r| r.id);
        let (h, full) = self.fs.move_file(from, to, &expected)?;
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
            let (m, c) = file_times(&md);
            self.index.upsert_file(to, &bytes, m, c)?;
            Ok(())
        })();
        let echo = match res {
            Ok(()) => Echo::Indexed,
            Err(e) => {
                tracing::warn!(from, to, error = %e, "index update after rename failed");
                Echo::Unindexed
            }
        };
        self.echoes.register(from, None, echo);
        self.echoes.register(to, Some(h.clone()), echo);
        self.lineage.renamed(from, to);
        self.emit(
            to,
            EventKind::Renamed {
                from: from.to_string(),
            },
        );
        // Resolver after the rename, and the same without the moved file (uniqueness check).
        let with = self.index.resolver().clone();
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
    /// hash, each rewriting inbound links). Every target and source hash is pre-flighted before
    /// anything moves; a file that fails pre-flight or its own move is reported in
    /// [`RenameOutcome::failed`] and the rest still move, so the merged outcome always covers the
    /// files already moved. Empty live source folders are removed best-effort.
    pub fn rename_folder(&mut self, from: &str, to: &str) -> Result<RenameOutcome> {
        validate_rel_path(from)?;
        validate_rel_path(to)?;
        let mut outcome = RenameOutcome::default();
        let mut plan = Vec::new();
        for f in self.fs.list_files(from)? {
            let Some(rest) = f.strip_prefix(&format!("{from}/")) else {
                continue;
            };
            let target = format!("{to}/{rest}");
            match (self.fs.hash(&f), self.fs.read(&target)) {
                (Ok(None), _) => {}
                (Ok(Some(_)), Ok(Some(_))) => outcome
                    .failed
                    .push((f, VaultError::Exists(target).to_string())),
                (Ok(Some(h)), Ok(None)) => plan.push((f, target, h)),
                (Err(e), _) | (_, Err(e)) => outcome.failed.push((f, e.to_string())),
            }
        }
        for (f, target, h) in plan {
            match self.rename(&f, &target, h) {
                Ok(mut o) => {
                    o.moved.push(target);
                    outcome.merge(o);
                }
                Err(e) => {
                    tracing::warn!(path = %f, error = %e, "folder rename: file left in place");
                    outcome.failed.push((f, e.to_string()));
                }
            }
        }
        outcome.failed.sort();
        remove_empty_dirs(&self.fs.root().join(from));
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
