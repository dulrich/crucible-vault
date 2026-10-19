//! Save, conflict and external-change handling: the CAS save machine (D1) and the conflict bar
//! state (see the module docs of [`super`]).

use std::sync::Arc;
use std::time::Duration;

use iced::Task;

use super::SourceBuffer;
use crate::daemon::{ContentHash, DaemonClient, DaemonError, is_no_base, no_base};
use crate::message::{Message, SourceMsg};

/// Autosave debounce after the last edit.
pub const AUTOSAVE_DELAY: Duration = Duration::from_secs(1);

/// Result of one save attempt (cloneable for the message bus; never carries note text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved(ContentHash),
    Conflict(ContentHash),
    Failed(String),
}

/// A save ready to run off the UI thread.
#[derive(Debug, Clone)]
pub struct SaveJob {
    pub path: String,
    pub seq: u64,
    pub(super) text: String,
    pub(super) base: ContentHash,
}

impl SaveJob {
    /// Runs the CAS write (blocking; call from `spawn_blocking` or a test).
    pub fn run(&self, d: &dyn DaemonClient) -> SaveOutcome {
        match d.write_note(&self.path, &self.text, &self.base) {
            Ok(h) => SaveOutcome::Saved(h),
            Err(DaemonError::Conflict { current_hash }) => SaveOutcome::Conflict(current_hash),
            Err(e) => SaveOutcome::Failed(e.to_string()),
        }
    }

    /// The message that reports `outcome` back.
    pub fn done(&self, outcome: SaveOutcome) -> Message {
        Message::Source(SourceMsg::Saved {
            path: self.path.clone(),
            seq: self.seq,
            outcome,
            after: None,
        })
    }
}

/// Why the conflict bar is up (plan tn-review-baseline-remediation WP-R16, B5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conflict {
    /// The file on disk changed under the buffer's edits; its current hash.
    Changed(ContentHash),
    /// plan editor-data-safety WP-2: the file is gone (a save CAS conflict against no file, or
    /// a NotFound read under edits).
    Missing,
}

impl Conflict {
    /// The conflict a save CAS reply's `current_hash` means (the wire's empty hash = no file).
    fn from_disk(current: ContentHash) -> Self {
        if current.as_str().is_empty() {
            Self::Missing
        } else {
            Self::Changed(current)
        }
    }

    /// The hash Keep mine re-saves against (the CAS "no file" sentinel when missing).
    fn base(self) -> ContentHash {
        match self {
            Self::Changed(h) => h,
            Self::Missing => no_base(),
        }
    }
}

/// The save in flight (one per buffer).
#[derive(Debug, Clone)]
pub(super) struct InFlight {
    /// Hash of the text being saved (its `file_changed` echo is ignored).
    hash: ContentHash,
    /// WP-5: the frontmatter title of that text.
    title: Option<String>,
}

/// The buffer's save state.
#[derive(Debug, Clone)]
pub(super) struct SaveState {
    /// The hash the next save is CAS'd against.
    pub(super) base_hash: ContentHash,
    pub(super) dirty: bool,
    pub(super) in_flight: Option<InFlight>,
    /// Another save was requested while one was in flight.
    save_again: bool,
    /// Dirty buffer vs a newer (or vanished) file on disk, for the conflict bar.
    pub(super) conflict: Option<Conflict>,
    /// plan editor-data-safety WP-1 (audit E1): a reconnect arrived while a save on the old
    /// connection was in flight; resync once its completion has been processed.
    resync_pending: bool,
    /// Plan usability-sprint-captures-links WP-5: the frontmatter `title` of the text last known
    /// on disk (`new`/`reload`/landed saves).
    disk_title: Option<String>,
    /// WP-5: the title the current filename was derived from — the pre-change disk title, kept
    /// until a title rename happens (fresh buffer) or the title stops matching the filename.
    name_title: Option<String>,
    /// WP-5 (audit E2): a title rename of this note is in flight; saves are held (queued as
    /// `save_again`) so the old path is never written or recreated.
    renaming: bool,
}

impl SaveState {
    pub(super) fn new(base_hash: ContentHash, title: Option<String>) -> Self {
        Self {
            base_hash,
            dirty: false,
            in_flight: None,
            save_again: false,
            conflict: None,
            resync_pending: false,
            disk_title: title.clone(),
            name_title: title,
            renaming: false,
        }
    }
}

impl SourceBuffer {
    /// The hash the next save is CAS'd against.
    pub fn base_hash(&self) -> &ContentHash {
        &self.save.base_hash
    }
    /// Unsaved edits exist.
    pub fn is_dirty(&self) -> bool {
        self.save.dirty
    }
    /// A save is in flight.
    pub fn is_saving(&self) -> bool {
        self.save.in_flight.is_some()
    }
    /// The conflict when the conflict bar is up.
    pub fn conflict(&self) -> Option<&Conflict> {
        self.save.conflict.as_ref()
    }

    /// plan editor-data-safety WP-2: the conflict is "the file is gone".
    pub fn is_missing(&self) -> bool {
        self.save.conflict == Some(Conflict::Missing)
    }

    /// A dirty buffer whose file is gone enters the missing-file state (no-op when clean).
    pub fn mark_missing(&mut self) {
        if self.save.dirty {
            self.save.conflict = Some(Conflict::Missing);
        }
    }

    /// Replaces the text with what is on disk, keeping the cursor line (clamped);
    /// history is reset, the buffer becomes clean, and any conflict clears.
    pub fn reload(&mut self, bytes: &[u8], hash: ContentHash) {
        let old = self.cursor().position;
        let text = String::from_utf8_lossy(bytes).into_owned();
        let line_count = text.split('\n').count();
        let line = old.line.min(line_count.saturating_sub(1));
        let line_text = text
            .split('\n')
            .nth(line)
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .unwrap_or_default();
        let mut column = old.column.min(line_text.len());
        while !line_text.is_char_boundary(column) {
            column -= 1;
        }
        let cursor = editor_model::undo::Cursor {
            pos: editor_model::undo::Pos::new(line, column),
            anchor: None,
        };
        let shaped = self.is_shaped();
        self.save.disk_title = crate::header::frontmatter_title(&text);
        self.save.name_title.clone_from(&self.save.disk_title);
        self.content = super::buffer::Repr::Plain { text, cursor };
        if shaped {
            self.materialise();
        }
        self.epoch += 1;
        self.save.base_hash = hash;
        self.history.reset();
        self.save.dirty = false;
        self.save.conflict = None;
        self.save.save_again = false;
    }

    /// Prepares a save, or `None` (clean, conflicted or already in flight — in the
    /// last case a follow-up save is queued).
    pub fn begin_save(&mut self) -> Option<SaveJob> {
        if !self.save.dirty || self.save.conflict.is_some() {
            return None;
        }
        if self.save.in_flight.is_some() || self.save.renaming {
            self.save.save_again = true;
            return None;
        }
        let text = self.text();
        self.save.in_flight = Some(InFlight {
            hash: ContentHash::of(text.as_bytes()),
            title: crate::header::frontmatter_title(&text),
        });
        self.save.save_again = false;
        Some(SaveJob {
            path: self.path.clone(),
            seq: self.edit_seq,
            text,
            base: self.save.base_hash.clone(),
        })
    }

    /// Applies a save result. Returns `true` if a queued follow-up save should start.
    pub fn finish_save(&mut self, seq: u64, outcome: SaveOutcome) -> Result<bool, String> {
        let title = self.save.in_flight.take().and_then(|f| f.title);
        match outcome {
            SaveOutcome::Saved(h) => {
                self.save.base_hash = h;
                self.save.disk_title = title;
                if self.title_rename().is_none() {
                    self.save.name_title.clone_from(&self.save.disk_title);
                }
                if seq == self.edit_seq {
                    self.save.dirty = false;
                }
                Ok(self.save.dirty && std::mem::take(&mut self.save.save_again))
            }
            SaveOutcome::Conflict(current) => {
                self.save.conflict = Some(Conflict::from_disk(current));
                Ok(false)
            }
            SaveOutcome::Failed(e) => Err(e),
        }
    }

    /// A `file_changed` for this buffer's path. `hash` is the notification's hash;
    /// `read` re-reads the file. Returns an error string worth surfacing, if any.
    pub fn external_change(
        &mut self,
        hash: Option<&str>,
        read: impl FnOnce() -> Result<(Vec<u8>, ContentHash), DaemonError>,
    ) -> Option<String> {
        if let Some(h) = hash
            && (h == self.save.base_hash.as_str()
                || self
                    .save
                    .in_flight
                    .as_ref()
                    .is_some_and(|f| f.hash.as_str() == h))
        {
            return None; // Our own save's echo (origin is always `daemon`).
        }
        if self.save.dirty {
            let current = match hash {
                Some(h) => ContentHash::from_hex(h),
                None => match read() {
                    Ok((_, h)) => h,
                    // WP-2 (audit E2a): deleted / moved away under edits → missing-file state.
                    Err(DaemonError::NotFound(_)) => {
                        if !is_no_base(&self.save.base_hash) {
                            self.save.conflict = Some(Conflict::Missing);
                        }
                        return None;
                    }
                    Err(e) => return Some(format!("file changed: {e}")),
                },
            };
            if current != self.save.base_hash {
                self.save.conflict = Some(Conflict::from_disk(current));
            }
            return None;
        }
        match read() {
            Ok((bytes, h)) => {
                if h != self.save.base_hash {
                    self.reload(&bytes, h);
                }
                None
            }
            Err(DaemonError::NotFound(_)) => Some("file deleted on disk".into()),
            Err(e) => Some(format!("reload failed: {e}")),
        }
    }

    /// WP-5: the title-sync rename target for the text on disk (the disk title changed from the
    /// one the filename was derived from), per [`crate::title_sync::title_sync_target`].
    pub fn title_rename(&self) -> Option<String> {
        crate::title_sync::title_sync_target(
            &self.path,
            self.save.name_title.as_deref(),
            self.save.disk_title.as_deref(),
        )
    }

    /// WP-5: a title rename is in flight.
    pub fn is_renaming(&self) -> bool {
        self.save.renaming
    }

    /// WP-5: marks a title rename in flight (saves are held) or finished (returns whether a held
    /// save is waiting).
    pub fn set_renaming(&mut self, on: bool) -> bool {
        self.save.renaming = on;
        !on && self.save.dirty && self.save.save_again
    }

    /// WP-5: stop syncing the current title change (rename refused or failed): the filename is
    /// no longer derived from a pending title.
    pub fn settle_title(&mut self) {
        self.save.name_title.clone_from(&self.save.disk_title);
    }

    /// The note moved to `to` (whose on-disk hash is `disk`) while this buffer had edits the move
    /// did not carry: follow the path, keep the text, and raise the conflict bar (Keep mine
    /// saves it at `to`; Take theirs discards it). editor-usability-round WP-5, audit M2.
    pub fn follow_move(&mut self, to: &str, disk: ContentHash) {
        to.clone_into(&mut self.path);
        self.save.dirty = true;
        self.save.conflict = Some(Conflict::from_disk(disk));
    }

    /// Plan round-7-feedback-decisions WP-2: a clean buffer whose file was renamed to `to` with
    /// unchanged bytes is re-keyed in place, so cursor, selection, undo history and `top_line`
    /// survive. Unlike [`Self::follow_move`] it neither dirties nor raises a conflict.
    pub fn rename_clean(&mut self, to: &str) {
        to.clone_into(&mut self.path);
    }

    /// Keep mine: re-save against the on-disk hash (recreating a missing file).
    pub fn keep_mine(&mut self) -> Option<SaveJob> {
        let current = self.save.conflict.take()?;
        self.save.base_hash = current.base();
        self.begin_save()
    }

    /// plan editor-data-safety WP-1 (audit E1): mark a reconnect resync as waiting on the
    /// in-flight save's completion.
    pub fn defer_resync(&mut self) {
        self.save.resync_pending = true;
    }

    /// Takes the deferred-resync mark (set while a save was in flight across a reconnect).
    pub fn take_resync_pending(&mut self) -> bool {
        std::mem::take(&mut self.save.resync_pending)
    }

    /// Reconnect resync against the file now on disk (`bytes`, `disk`). Clean → reload when
    /// the disk differs. Dirty → if the disk already holds this exact text (an old save landed
    /// but its reply was lost) adopt it as clean; else conflict bar when the disk differs from
    /// the base; else a fresh save (any queued `save_again` is subsumed: the save carries the
    /// latest text). Never call while a save is in flight.
    pub fn resync(&mut self, bytes: &[u8], disk: ContentHash) -> Option<SaveJob> {
        if !self.save.dirty {
            if disk != self.save.base_hash {
                self.reload(bytes, disk);
            }
            return None;
        }
        if ContentHash::of(self.text().as_bytes()) == disk {
            self.save.base_hash = disk;
            self.save.dirty = false;
            self.save.conflict = None;
            self.save.save_again = false;
            return None;
        }
        if self.save.conflict.is_some() || disk != self.save.base_hash {
            self.save.conflict = Some(Conflict::from_disk(disk));
            return None;
        }
        self.begin_save()
    }
}

/// Plan editor-feedback-round-2 WP-3: flush `buf` now (tab switch away, window unfocus) through
/// the one save machine — `begin_save` is a no-op for clean, read-only and conflicted
/// buffers and re-queues (`save_again`) behind an in-flight save.
pub(crate) fn flush(
    buf: &mut SourceBuffer,
    daemon: Option<&Arc<dyn DaemonClient>>,
) -> Task<Message> {
    if daemon.is_none() {
        return Task::none();
    }
    spawn_save(daemon, buf.begin_save())
}

pub(crate) fn spawn_save(
    daemon: Option<&Arc<dyn DaemonClient>>,
    job: Option<SaveJob>,
) -> Task<Message> {
    let (Some(d), Some(job)) = (daemon, job) else {
        return Task::none();
    };
    let d = Arc::clone(d);
    let done = job.clone();
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || job.run(d.as_ref()))
                .await
                .unwrap_or_else(|e| SaveOutcome::Failed(e.to_string()))
        },
        move |o| done.done(o),
    )
}

pub(crate) fn autosave_timer(path: String, seq: u64) -> Task<Message> {
    // The sleep future is built lazily inside the async block (it needs the executor's reactor).
    Task::perform(
        async { tokio::time::sleep(AUTOSAVE_DELAY).await },
        move |()| {
            Message::Source(SourceMsg::AutosaveDue {
                path: path.clone(),
                seq,
            })
        },
    )
}

/// Handles `file_changed` for `buf` (one open document on the notified path).
pub fn file_changed(
    buf: &mut SourceBuffer,
    hash: Option<&str>,
    daemon: Option<&Arc<dyn DaemonClient>>,
) -> Option<String> {
    let d = daemon?;
    let path = buf.path.clone();
    buf.external_change(hash, || d.read_file(&path))
}
