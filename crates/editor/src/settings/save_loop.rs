//! The generic per-file save loop: [`ConfigFile`] (load, edit sequence, the one in-flight
//! write and its requeue) and the exact-name config write ([`write_config`]).

use std::collections::BTreeMap;

use cv_schema::{ConfigDoc, DocError, FieldError, Node};

use super::{Section, Settings};
use crate::daemon::{ContentHash, DaemonClient, DaemonError};

/// A whole-file config write's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    /// Written; the new on-disk hash.
    Saved(ContentHash),
    /// The file changed (or appeared) since it was read: nothing written.
    Conflict,
}

/// The one exact-name config write (Decisions locked, audit M6): `write_note(path, content,
/// base)` with the last-read hash, or the empty hash when the file was absent at load — the
/// daemon's absence-CAS (`crates/cv/src/daemon.rs:476-480`). Never `create_note`: a file that
/// appeared since load is a [`WriteOutcome::Conflict`], never a suffixed copy. Blocking.
pub fn write_config(
    d: &dyn DaemonClient,
    path: &str,
    content: &str,
    base: Option<&ContentHash>,
) -> Result<WriteOutcome, String> {
    let absent = ContentHash::from_hex("");
    match d.write_note(path, content, base.unwrap_or(&absent)) {
        Ok(h) => Ok(WriteOutcome::Saved(h)),
        Err(DaemonError::Conflict { .. }) => Ok(WriteOutcome::Conflict),
        Err(e) => Err(e.to_string()),
    }
}

/// A config file's load state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LoadState {
    #[default]
    NotLoaded,
    /// The file does not exist (the doc is empty; Save creates it at the exact name).
    Absent,
    Loaded,
    /// The file does not parse as TOML: no doc, never overwritten until the user starts over.
    Malformed(String),
    /// The read failed.
    Error(String),
}

/// A save ready to run off the UI thread.
#[derive(Debug, Clone)]
pub struct SaveJob {
    pub path: String,
    pub content: String,
    pub base: Option<ContentHash>,
}

impl SaveJob {
    /// Runs the write (blocking).
    pub fn run(&self, d: &dyn DaemonClient) -> Result<WriteOutcome, String> {
        write_config(d, &self.path, &self.content, self.base.as_ref())
    }
}

/// The auto-save debounce after the last valid edit (WP-2: a few hundred ms, so a text input
/// never writes per keystroke).
pub const AUTOSAVE_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

/// The notice set when a save hit a CAS conflict (the file was reloaded).
pub const CHANGED_ON_DISK: &str = "changed on disk; reloaded (your edits were not saved)";

/// A local check of a file's candidate text: `(blocking errors, warnings)`.
pub type LocalCheck = fn(&str, &str) -> (Vec<String>, Vec<String>);

/// The write phase of a [`ConfigFile`]: at most one write is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Idle,
    /// A write of edit sequence `seq` is in flight; `requeue`: a save was requested meanwhile
    /// (re-run once when it lands).
    Writing { seq: u64, requeue: bool },
}

/// What [`ConfigFile::finish_save`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinishOutcome {
    /// The write did not land (conflict or error).
    pub failed: bool,
    /// A save requested during the write is due now: run [`ConfigFile::request_save`] again.
    pub requeue: bool,
}

/// One config file under edit: the generic per-file save loop.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    /// Vault-relative path.
    path: String,
    /// Form-level schema (`None` = no checks).
    schema: Option<&'static Node>,
    /// The document (`None` = not loaded / malformed / read error).
    doc: Option<ConfigDoc>,
    /// The hash the doc was read at (`None` = the file was absent).
    base: Option<ContentHash>,
    state: LoadState,
    /// Form-level errors of the current doc (non-empty blocks Save).
    errors: Vec<FieldError>,
    /// The daemon's last `ConfigReloaded` errors naming this file.
    reload_errors: Vec<String>,
    /// A one-off notice (e.g. [`CHANGED_ON_DISK`], a write error).
    notice: Option<String>,
    /// Local non-blocking diagnostics (views compile notices).
    warnings: Vec<String>,
    /// A local check over the candidate text (views carve-out).
    local_check: Option<LocalCheck>,
    /// Bumped by every edit (auto-save coalescing key).
    edit_seq: u64,
    /// The edit sequence on disk (loaded or written): `dirty` = `edit_seq != saved_seq`.
    saved_seq: u64,
    /// The edit sequence the last debounce timer was armed for.
    scheduled_seq: u64,
    phase: Phase,
}

impl ConfigFile {
    /// An unloaded file.
    pub fn new(path: impl Into<String>, schema: Option<&'static Node>) -> Self {
        Self {
            path: path.into(),
            schema,
            doc: None,
            base: None,
            state: LoadState::NotLoaded,
            errors: Vec::new(),
            reload_errors: Vec::new(),
            notice: None,
            warnings: Vec::new(),
            local_check: None,
            edit_seq: 0,
            saved_seq: 0,
            scheduled_seq: 0,
            phase: Phase::Idle,
        }
    }

    /// A file that does not exist yet, with an empty doc and one pending edit (its first save
    /// creates it at the exact name).
    pub fn new_absent(
        path: impl Into<String>,
        schema: Option<&'static Node>,
        check: Option<LocalCheck>,
    ) -> Self {
        let mut f = Self::new(path, schema);
        f.local_check = check;
        f.doc = Some(ConfigDoc::new());
        f.state = LoadState::Absent;
        f.edit_seq = 1;
        f.revalidate();
        f
    }

    /// Sets the local check (views carve-out).
    pub fn set_local_check(&mut self, check: Option<LocalCheck>) {
        self.local_check = check;
    }

    pub fn local_check(&self) -> Option<LocalCheck> {
        self.local_check
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn schema(&self) -> Option<&'static Node> {
        self.schema
    }

    pub fn doc(&self) -> Option<&ConfigDoc> {
        self.doc.as_ref()
    }

    pub fn base(&self) -> Option<&ContentHash> {
        self.base.as_ref()
    }

    pub fn state(&self) -> &LoadState {
        &self.state
    }

    pub fn errors(&self) -> &[FieldError] {
        &self.errors
    }

    pub fn reload_errors(&self) -> &[String] {
        &self.reload_errors
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn set_notice(&mut self, n: Option<String>) {
        self.notice = n;
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn edit_seq(&self) -> u64 {
        self.edit_seq
    }

    pub fn scheduled_seq(&self) -> u64 {
        self.scheduled_seq
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Unsaved edits.
    pub fn dirty(&self) -> bool {
        self.edit_seq != self.saved_seq
    }

    /// A write is in flight.
    pub fn saving(&self) -> bool {
        matches!(self.phase, Phase::Writing { .. })
    }

    /// A save was requested during the in-flight write.
    pub fn queued(&self) -> bool {
        matches!(self.phase, Phase::Writing { requeue: true, .. })
    }

    /// The doc for an in-memory seed that is not an edit (no dirty, no revalidation): the
    /// one-time surround migration ([`super::editor_prefs::seed_surround`]).
    pub fn seed_doc(&mut self) -> Option<&mut ConfigDoc> {
        self.doc.as_mut()
    }

    /// Tests: replace the doc without an edit.
    #[cfg(test)]
    pub(crate) fn test_set_doc(&mut self, doc: Option<ConfigDoc>) {
        self.doc = doc;
    }

    /// Tests: mark an unsaved edit without touching the doc.
    #[cfg(test)]
    pub(crate) fn test_mark_dirty(&mut self) {
        self.edit_seq += 1;
    }

    /// (Re)loads from disk, dropping unsaved edits.
    pub fn load(&mut self, d: &dyn DaemonClient) {
        self.apply_read(d.read_file(&self.path));
    }

    /// Installs a read result (pure; [`ConfigFile::load`] feeds it).
    pub fn apply_read(&mut self, r: Result<(Vec<u8>, ContentHash), DaemonError>) {
        self.saved_seq = self.edit_seq;
        if let Phase::Writing { requeue, .. } = &mut self.phase {
            *requeue = false;
        }
        self.scheduled_seq = self.edit_seq;
        self.errors.clear();
        match r {
            Ok((bytes, hash)) => {
                self.base = Some(hash);
                let parsed = std::str::from_utf8(&bytes)
                    .map_err(|_| "not UTF-8".to_owned())
                    .and_then(|s| ConfigDoc::parse(s).map_err(|e| e.to_string()));
                match parsed {
                    Ok(doc) => {
                        self.doc = Some(doc);
                        self.state = LoadState::Loaded;
                    }
                    Err(e) => {
                        self.doc = None;
                        self.state = LoadState::Malformed(e);
                    }
                }
            }
            Err(DaemonError::NotFound(_)) => {
                self.base = None;
                self.doc = Some(ConfigDoc::new());
                self.state = LoadState::Absent;
            }
            Err(e) => {
                self.doc = None;
                self.state = LoadState::Error(e.to_string());
            }
        }
        self.revalidate();
    }

    /// Re-runs the form-level checks.
    pub fn revalidate(&mut self) {
        self.errors = match (&self.doc, self.schema) {
            (Some(doc), Some(schema)) => cv_schema::validate(doc, schema),
            _ => Vec::new(),
        };
        self.warnings.clear();
        if let (Some(doc), Some(check)) = (&self.doc, self.local_check) {
            let (errs, warns) = check(&self.path, &doc.to_string());
            self.errors
                .extend(errs.into_iter().map(|message| FieldError {
                    path: cv_schema::Path::root(),
                    message,
                }));
            self.warnings = warns;
        }
    }

    /// Applies an edit to the doc (marks dirty, revalidates). No doc (malformed) → an error.
    pub fn edit(
        &mut self,
        f: impl FnOnce(&mut ConfigDoc) -> Result<(), DocError>,
    ) -> Result<(), String> {
        let Some(doc) = &mut self.doc else {
            return Err(format!("{}: not editable (not loaded)", self.path));
        };
        f(doc).map_err(|e| e.to_string())?;
        self.edit_seq += 1;
        self.notice = None;
        self.revalidate();
        Ok(())
    }

    /// A malformed file: start over from an empty doc (dirty; the next Save replaces the file
    /// through CAS on the malformed content's hash).
    pub fn start_over(&mut self) {
        if matches!(self.state, LoadState::Malformed(_)) {
            self.doc = Some(ConfigDoc::new());
            self.edit_seq += 1;
            self.revalidate();
        }
    }

    /// Whether the current doc should be written (dirty, loaded, no blocking errors), whether
    /// or not a write is in flight.
    pub fn wants_save(&self) -> bool {
        self.dirty() && self.doc.is_some() && self.errors.is_empty()
    }

    /// Whether a write can start now.
    pub fn can_save(&self) -> bool {
        self.wants_save() && !self.saving()
    }

    /// The one-line save status (`None` when there is nothing to say) and whether it is a
    /// warning.
    pub fn save_status(&self) -> Option<(String, bool)> {
        if self.dirty() && self.doc.is_some() && !self.errors.is_empty() {
            let n = self.errors.len();
            let s = if n == 1 { "" } else { "s" };
            return Some((format!("not saved: {n} error{s}"), true));
        }
        if self.saving() || self.dirty() {
            return Some(("saving…".into(), false));
        }
        matches!(self.state, LoadState::Loaded).then(|| ("saved".into(), false))
    }

    /// The save job for the current doc (`None` when Save is disabled).
    pub fn begin_save(&mut self) -> Option<SaveJob> {
        if !self.can_save() {
            return None;
        }
        let content = self.doc.as_ref()?.to_string();
        self.phase = Phase::Writing {
            seq: self.edit_seq,
            requeue: false,
        };
        Some(SaveJob {
            path: self.path.clone(),
            content,
            base: self.base.clone(),
        })
    }

    /// A save request: the job to run now, or — behind an in-flight write — `None`, with the
    /// request remembered (re-run once when that write lands) when there is something to write.
    pub fn request_save(&mut self) -> Option<SaveJob> {
        let wants = self.wants_save();
        if let Phase::Writing { requeue, .. } = &mut self.phase {
            *requeue |= wants;
            return None;
        }
        self.begin_save()
    }

    /// Lands a save outcome. A conflict (or, with `reload_on_error`, a write error) reloads
    /// from disk when a daemon is given — the edit is never forced over the newer file. The
    /// outcome says whether the write failed and whether the requested follow-up is due.
    pub fn finish_save(
        &mut self,
        r: Result<WriteOutcome, String>,
        d: Option<&dyn DaemonClient>,
        reload_on_error: bool,
    ) -> FinishOutcome {
        let (seq, mut requeue) = match std::mem::take(&mut self.phase) {
            Phase::Writing { seq, requeue } => (seq, requeue),
            Phase::Idle => (self.saved_seq, false),
        };
        let failed = !matches!(r, Ok(WriteOutcome::Saved(_)));
        match r {
            Ok(WriteOutcome::Saved(h)) => {
                self.base = Some(h);
                // An edit made while the write was in flight stays dirty (it re-queues).
                self.saved_seq = seq;
                self.state = LoadState::Loaded;
                self.notice = None;
            }
            Ok(WriteOutcome::Conflict) => {
                if let Some(d) = d {
                    self.load(d);
                    requeue = false;
                }
                self.notice = Some(CHANGED_ON_DISK.to_owned());
            }
            Err(e) => {
                self.notice = Some(format!("save failed: {e}"));
                if reload_on_error && let Some(d) = d {
                    self.load(d);
                    requeue = false;
                }
            }
        }
        FinishOutcome {
            failed,
            requeue: requeue && self.can_save(),
        }
    }

    /// A `file_changed` for this path: reload when clean and the hash differs from ours;
    /// a dirty doc keeps its edits and gets the "changed on disk" notice (its Save will hit
    /// the CAS conflict). Returns whether the doc was reloaded.
    pub fn file_changed(&mut self, hash: Option<&str>, d: &dyn DaemonClient) -> bool {
        if self.saving() || self.state == LoadState::NotLoaded {
            return false;
        }
        if hash.is_some() && hash == self.base.as_ref().map(ContentHash::as_str) {
            return false;
        }
        if self.dirty() {
            self.notice = Some("changed on disk; the pending save will conflict and reload".into());
            return false;
        }
        self.load(d);
        true
    }
}

/// Every editable file, in rail order.
pub(super) fn new_files() -> BTreeMap<String, ConfigFile> {
    Section::ALL
        .into_iter()
        .filter_map(|s| {
            let path = s.file()?;
            let schema = cv_schema::schema_for(s.file_name()?);
            Some((path.to_owned(), ConfigFile::new(path, schema)))
        })
        .collect()
}

impl Settings {
    /// Every file whose latest edit is valid and has no debounce timer yet, as
    /// `(path, edit_seq)`; marks each as scheduled.
    pub fn due_autosaves(&mut self) -> Vec<(String, u64)> {
        self.files
            .values_mut()
            .filter(|f| f.wants_save() && f.scheduled_seq != f.edit_seq)
            .map(|f| {
                f.scheduled_seq = f.edit_seq;
                (f.path.clone(), f.edit_seq)
            })
            .collect()
    }

    /// Whether any file page has unsaved edits.
    pub fn any_dirty(&self) -> bool {
        self.files.values().any(ConfigFile::dirty)
    }

    /// Routes a `ConfigReloaded { errors }` batch: each error to the page of the file it names,
    /// the rest to Status. Replaces the previous batch.
    pub fn route_reload_errors(&mut self, errors: &[String]) {
        for f in self.files.values_mut() {
            f.reload_errors.clear();
        }
        self.status.unrouted.clear();
        for e in errors {
            let path = super::attribute(e).and_then(Section::file);
            match path.and_then(|p| self.files.get_mut(p)) {
                Some(f) => f.reload_errors.push(e.clone()),
                None => self.status.unrouted.push(e.clone()),
            }
        }
    }

    /// Tests: the auto-save message for `path`'s latest edit, i.e. what its debounce timer
    /// would deliver now.
    #[cfg(test)]
    pub(crate) fn due_now(&self, path: impl AsRef<str>) -> super::SettingsMsg {
        let path = path.as_ref();
        let seq = self.file(path).map_or(0, |f| f.edit_seq);
        super::SettingsMsg::AutoSaveDue(path.to_owned(), seq)
    }
}
