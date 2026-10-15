//! The Settings tab (plan `editor-settings-tab` WP-2): a non-file tab (Ctrl+,) with a section
//! rail and one page per section, over the vault config files in `_custom/crucible-vault/`.
//!
//! Crucible reference: the plugin settings tab grouped Configure / Automate / AI / Orchestrate /
//! Lint / Commands (`https://github.com/dulrich/obsidian-crucible/blob/master/src/settings.ts:250-272`). Here the settings are the
//! vault's TOML files themselves (D18/D20): the tab is a typed, comment-preserving editor over
//! them, never a second store. The Obsidian-induced part — a plugin `data.json` store and its
//! settings chrome — is dropped.
//!
//! # The generic per-file save loop
//!
//! Every page that edits a file owns one [`ConfigFile`] in [`Settings::files`], keyed by its
//! vault-relative path ([`Section::file`]):
//!
//! - **Load**: [`ConfigFile::load`] reads the file from disk (a local read, like bookmarks).
//!   Absent → an empty [`ConfigDoc`] with `base = None`; unparseable → [`LoadState::Malformed`]
//!   (no doc; the page shows the error; nothing is ever written until the user starts over and
//!   saves).
//! - **Edit**: [`ConfigFile::edit`] applies a `cv_schema` document edit, marks the file dirty,
//!   bumps its edit sequence and re-runs the form-level [`cv_schema::validate`]; any error
//!   blocks the automatic write (the page shows it; the file keeps its last valid content).
//! - **Auto-save** (plan `editor-feedback-round-2` WP-2, replacing the explicit Save of D20):
//!   [`Settings::due_autosaves`] yields every file whose latest edit left it valid and has no
//!   debounce timer yet; the app arms an [`AUTOSAVE_DELAY`] timer per edit (coalescing: only the
//!   timer of the latest edit sequence writes). At most one write is in flight per file; a
//!   timer that fires during a write sets [`ConfigFile::queued`] and the write re-runs once the
//!   first lands. Closing the Settings tab and quitting flush pending writes.
//! - **Write**: [`ConfigFile::begin_save`] yields a [`SaveJob`] that the app runs off
//!   the UI thread through [`write_config`] — the one exact-name config write (whole file,
//!   `write_note` CAS on the last-read hash, or the empty-hash absence-CAS when the file was
//!   absent; never `create_note`, so a fixed config name never gets a collision suffix).
//!   [`ConfigFile::finish_save`] lands the outcome; a conflict reloads the file and sets the
//!   "changed on disk" notice — it never overwrites. Revert (no longer on the page) is a reload.
//! - **Daemon feedback**: `Notification::ConfigReloaded { errors }` is routed per file by
//!   [`Settings::route_reload_errors`] ([`attribute`]); unattributable errors go to Status.
//!
//! # Extension point (WP-3)
//!
//! A page is a [`Section`] plus, for file pages, its [`ConfigFile`]. WP-3 adds
//! `settings/{form.rs,widgets.rs,pages/*.rs}` and replaces the body of [`page`]'s placeholder
//! arm for its section with a form that reads `file.doc` and emits
//! [`SettingsMsg::Edit`]`(path, DocEdit)`; the save loop, the save status line ([`save_bar`]),
//! the load-state / error header ([`file_header`]) and the routing stay as they are.
//! Editor prefs (and the `[keys]` table the WP-4 keymap applies) are [`editor_prefs::Prefs`], held on the
//! app as `App::prefs`.

pub mod editor_prefs;
pub mod form;
pub mod pages;
pub mod status;
pub mod widgets;

use std::collections::BTreeMap;

use cv_schema::{ConfigDoc, DocError, FieldError, Node, Seg};
use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Element, Length, Theme};

use crate::daemon::{ContentHash, DaemonClient, DaemonError};
use crate::icons::Icon;
use crate::message::Message;
use crate::theme::Tokens;

/// The Settings tab's persisted tab "path": starts with `/`, which `cv-vault`'s
/// `validate_rel_path` rejects, so it can never collide with a vault note; old `UiState` files
/// (plain vault paths) load unchanged.
pub const SETTINGS_TAB: &str = "/:settings";

/// The vault config folder (the daemon's `config_dir`, `crates/cv/src/config.rs:100`).
pub const CONFIG_DIR: &str = "_custom/crucible-vault";

/// One settings section (rail entry), in rail order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Section {
    Editor,
    Hotkeys,
    Actions,
    Schedules,
    Captures,
    Kinds,
    Agents,
    Providers,
    Views,
    Bookmarks,
    Status,
}

impl Section {
    /// Rail order.
    pub const ALL: [Self; 11] = [
        Self::Editor,
        Self::Hotkeys,
        Self::Actions,
        Self::Schedules,
        Self::Captures,
        Self::Kinds,
        Self::Agents,
        Self::Providers,
        Self::Views,
        Self::Bookmarks,
        Self::Status,
    ];

    /// The rail label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Editor => "Editor",
            Self::Hotkeys => "Hotkeys",
            Self::Actions => "Actions",
            Self::Schedules => "Schedules",
            Self::Captures => "Captures & periods",
            Self::Kinds => "Kinds",
            Self::Agents => "Agents",
            Self::Providers => "Providers",
            Self::Views => "Views",
            Self::Bookmarks => "Bookmarks",
            Self::Status => "Status",
        }
    }

    /// The rail glyph.
    pub fn icon(self) -> Icon {
        match self {
            Self::Editor => Icon::Type,
            Self::Hotkeys => Icon::Keyboard,
            Self::Actions => Icon::Zap,
            Self::Schedules => Icon::Timer,
            Self::Captures => Icon::CalendarDays,
            Self::Kinds => Icon::Tags,
            Self::Agents => Icon::Bot,
            Self::Providers => Icon::Server,
            Self::Views => Icon::Table,
            Self::Bookmarks => Icon::Bookmark,
            Self::Status => Icon::Activity,
        }
    }

    /// The rail group heading this section opens (Crucible's grouping), if any.
    pub fn group(self) -> Option<&'static str> {
        match self {
            Self::Actions => Some("Automate"),
            Self::Agents => Some("AI"),
            _ => None,
        }
    }

    /// The file name inside [`CONFIG_DIR`] this section edits (`None` for Hotkeys, Views — a
    /// folder, WP-3 — and Status).
    pub fn file_name(self) -> Option<&'static str> {
        Some(match self {
            Self::Editor => "editor.toml",
            Self::Actions => "actions.toml",
            Self::Schedules => "schedules.toml",
            Self::Captures => "captures.toml",
            Self::Kinds => "kinds.toml",
            Self::Agents => "agents.toml",
            Self::Providers => "providers.toml",
            Self::Bookmarks => "bookmarks.toml",
            Self::Hotkeys | Self::Views | Self::Status => return None,
        })
    }

    /// The vault-relative path this section edits.
    pub fn file(self) -> Option<&'static str> {
        Some(match self {
            Self::Editor => editor_prefs::EDITOR_PATH,
            Self::Actions => "_custom/crucible-vault/actions.toml",
            Self::Schedules => "_custom/crucible-vault/schedules.toml",
            Self::Captures => "_custom/crucible-vault/captures.toml",
            Self::Kinds => "_custom/crucible-vault/kinds.toml",
            Self::Agents => "_custom/crucible-vault/agents.toml",
            Self::Providers => "_custom/crucible-vault/providers.toml",
            Self::Bookmarks => crate::bookmarks::BOOKMARKS_PATH,
            Self::Hotkeys | Self::Views | Self::Status => return None,
        })
    }

    /// The section whose file is `name` (a bare file name such as `actions.toml`).
    pub fn for_file_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.file_name() == Some(name))
    }
}

/// The section a daemon config error names, from the error formats of
/// `cv-actions/src/config.rs` (`"<file>: …"`, `"action \"id\" disarmed: …"`) as surfaced by
/// `Runner::reload_config` (`crates/cv/src/runner/mod.rs:319-332`). `None` → Status.
pub fn attribute(error: &str) -> Option<Section> {
    if error.starts_with("action ") {
        return Some(Section::Actions);
    }
    let (head, _) = error.split_once(':')?;
    let head = head.trim();
    let name = head.rsplit('/').next().unwrap_or(head);
    Section::for_file_name(name)
}

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

/// One config file under edit: the generic per-file save loop.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    /// Vault-relative path.
    pub path: String,
    /// Form-level schema (`None` = no checks).
    pub schema: Option<&'static Node>,
    /// The document (`None` = not loaded / malformed / read error).
    pub doc: Option<ConfigDoc>,
    /// The hash the doc was read at (`None` = the file was absent).
    pub base: Option<ContentHash>,
    pub state: LoadState,
    /// Unsaved edits.
    pub dirty: bool,
    /// Form-level errors of the current doc (non-empty blocks Save).
    pub errors: Vec<FieldError>,
    /// The daemon's last `ConfigReloaded` errors naming this file.
    pub reload_errors: Vec<String>,
    /// A one-off notice (e.g. [`CHANGED_ON_DISK`], a write error).
    pub notice: Option<String>,
    /// A save is in flight.
    pub saving: bool,
    /// Local non-blocking diagnostics (views compile notices).
    pub warnings: Vec<String>,
    /// A local check over the candidate text: `(blocking errors, warnings)` (views carve-out).
    pub local_check: Option<LocalCheck>,
    /// Bumped by every edit (auto-save coalescing key).
    pub edit_seq: u64,
    /// The edit sequence the last debounce timer was armed for.
    pub scheduled_seq: u64,
    /// The edit sequence the in-flight write carries.
    pub saving_seq: u64,
    /// A write was requested while one was in flight: re-run it when the first lands.
    pub queued: bool,
}

/// A local check of a file's candidate text: `(blocking errors, warnings)`.
pub type LocalCheck = fn(&str, &str) -> (Vec<String>, Vec<String>);

impl ConfigFile {
    /// An unloaded file.
    pub fn new(path: impl Into<String>, schema: Option<&'static Node>) -> Self {
        Self {
            path: path.into(),
            schema,
            doc: None,
            base: None,
            state: LoadState::NotLoaded,
            dirty: false,
            errors: Vec::new(),
            reload_errors: Vec::new(),
            notice: None,
            saving: false,
            warnings: Vec::new(),
            local_check: None,
            edit_seq: 0,
            scheduled_seq: 0,
            saving_seq: 0,
            queued: false,
        }
    }

    /// (Re)loads from disk, dropping unsaved edits.
    pub fn load(&mut self, d: &dyn DaemonClient) {
        self.apply_read(d.read_file(&self.path));
    }

    /// Installs a read result (pure; [`ConfigFile::load`] feeds it).
    pub fn apply_read(&mut self, r: Result<(Vec<u8>, ContentHash), DaemonError>) {
        self.dirty = false;
        self.queued = false;
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
        self.dirty = true;
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
            self.dirty = true;
            self.edit_seq += 1;
            self.revalidate();
        }
    }

    /// Whether the current doc should be written (dirty, loaded, no blocking errors), whether
    /// or not a write is in flight.
    pub fn wants_save(&self) -> bool {
        self.dirty && self.doc.is_some() && self.errors.is_empty()
    }

    /// Whether a write can start now.
    pub fn can_save(&self) -> bool {
        self.wants_save() && !self.saving
    }

    /// The one-line save status (`None` when there is nothing to say) and whether it is a
    /// warning.
    pub fn save_status(&self) -> Option<(String, bool)> {
        if self.dirty && self.doc.is_some() && !self.errors.is_empty() {
            let n = self.errors.len();
            let s = if n == 1 { "" } else { "s" };
            return Some((format!("not saved: {n} error{s}"), true));
        }
        if self.saving || self.dirty {
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
        self.saving = true;
        self.saving_seq = self.edit_seq;
        self.queued = false;
        Some(SaveJob {
            path: self.path.clone(),
            content,
            base: self.base.clone(),
        })
    }

    /// Lands a save outcome. A conflict (or a write error) reloads from disk when a daemon is
    /// given — the edit is never forced over the newer file.
    pub fn finish_save(&mut self, r: Result<WriteOutcome, String>, d: Option<&dyn DaemonClient>) {
        self.saving = false;
        match r {
            Ok(WriteOutcome::Saved(h)) => {
                self.base = Some(h);
                // An edit made while the write was in flight stays dirty (it re-queues).
                self.dirty = self.edit_seq != self.saving_seq;
                self.state = LoadState::Loaded;
                self.notice = None;
            }
            Ok(WriteOutcome::Conflict) => {
                if let Some(d) = d {
                    self.load(d);
                }
                self.notice = Some(CHANGED_ON_DISK.to_owned());
            }
            Err(e) => self.notice = Some(format!("save failed: {e}")),
        }
    }

    /// A `file_changed` for this path: reload when clean and the hash differs from ours;
    /// a dirty doc keeps its edits and gets the "changed on disk" notice (its Save will hit
    /// the CAS conflict). Returns whether the doc was reloaded.
    pub fn file_changed(&mut self, hash: Option<&str>, d: &dyn DaemonClient) -> bool {
        if self.saving || self.state == LoadState::NotLoaded {
            return false;
        }
        if hash.is_some() && hash == self.base.as_ref().map(ContentHash::as_str) {
            return false;
        }
        if self.dirty {
            self.notice = Some("changed on disk; the pending save will conflict and reload".into());
            return false;
        }
        self.load(d);
        true
    }
}

/// Every editable file, in rail order.
fn new_files() -> BTreeMap<String, ConfigFile> {
    Section::ALL
        .into_iter()
        .filter_map(|s| {
            let path = s.file()?;
            let schema = cv_schema::schema_for(s.file_name()?);
            Some((path.to_owned(), ConfigFile::new(path, schema)))
        })
        .collect()
}

/// The Settings tab's model (lives on the app whether or not the tab is open).
#[derive(Debug)]
pub struct Settings {
    pub section: Section,
    /// Every file page's save loop, keyed by vault path.
    pub files: BTreeMap<String, ConfigFile>,
    pub status: status::StatusPage,
    /// Malformed-`editor.toml` / migration notes shown on the Editor page.
    pub editor_note: Option<String>,
    /// A pending confirm-before-Trash (the explorer path), when `confirm_trash` is on.
    pub confirm_trash: Option<String>,
    /// The hidden-commands field as typed (reset on load/revert).
    pub hidden_draft: Option<String>,
    /// Form UI state (expansion, drafts) of every file page.
    pub form: form::FormState,
    /// The Views page's selected file (vault path).
    pub view_selected: Option<String>,
    /// The "New view file" name field.
    pub new_view: String,
    /// The Hotkeys page (WP-4): capture state and keymap / action snapshots.
    pub hotkeys: pages::hotkeys::HotkeyState,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            section: Section::Editor,
            files: new_files(),
            status: status::StatusPage::default(),
            editor_note: None,
            confirm_trash: None,
            hidden_draft: None,
            form: form::FormState::default(),
            view_selected: None,
            new_view: String::new(),
            hotkeys: pages::hotkeys::HotkeyState::default(),
        }
    }
}

impl Settings {
    /// A file page's loop.
    pub fn file(&self, path: &str) -> Option<&ConfigFile> {
        self.files.get(path)
    }

    /// A file page's loop, mutably.
    pub fn file_mut(&mut self, path: &str) -> Option<&mut ConfigFile> {
        self.files.get_mut(path)
    }

    /// Loads every file not yet loaded (the tab opening).
    pub fn load_unloaded(&mut self, d: &dyn DaemonClient) {
        for f in self.files.values_mut() {
            if f.state == LoadState::NotLoaded {
                f.load(d);
            }
        }
    }

    /// Routes a `ConfigReloaded { errors }` batch: each error to the page of the file it names,
    /// the rest to Status. Replaces the previous batch.
    pub fn route_reload_errors(&mut self, errors: &[String]) {
        for f in self.files.values_mut() {
            f.reload_errors.clear();
        }
        self.status.unrouted.clear();
        for e in errors {
            let path = attribute(e).and_then(Section::file);
            match path.and_then(|p| self.files.get_mut(p)) {
                Some(f) => f.reload_errors.push(e.clone()),
                None => self.status.unrouted.push(e.clone()),
            }
        }
    }

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
        self.files.values().any(|f| f.dirty)
    }

    /// Applies a document edit to `file` (an error becomes its notice) and keeps the form UI
    /// state attached to the items it names.
    pub fn edit(&mut self, file: &str, edit: &DocEdit) -> Result<(), String> {
        let Some(f) = self.files.get_mut(file) else {
            return Err(format!("{file}: no such settings file"));
        };
        if let Err(e) = f.edit(|doc| edit.apply(doc)) {
            f.notice = Some(e.clone());
            return Err(e);
        }
        let fs = &mut self.form;
        let idx = |s: &Seg| match s {
            Seg::Index(i) => Some(*i),
            Seg::Key(_) => None,
        };
        match edit {
            DocEdit::Set(..) => {}
            DocEdit::Remove(p) => {
                fs.clear_drafts(file);
                if let Some((Seg::Index(gone), parent)) = p.0.split_last() {
                    let parent = cv_schema::Path(parent.to_vec());
                    let gone = *gone;
                    fs.remap(file, &parent, |s| match idx(s) {
                        Some(i) if i == gone => None,
                        Some(i) if i > gone => Some(Seg::Index(i - 1)),
                        _ => Some(s.clone()),
                    });
                } else {
                    let p = p.clone();
                    fs.expanded.retain(|(fl, q)| {
                        fl != file || q.0.len() < p.0.len() || q.0[..p.0.len()] != p.0[..]
                    });
                }
            }
            DocEdit::InsertRecord(list, at, _) => {
                fs.clear_drafts(file);
                let at = *at;
                fs.remap(file, list, |s| match idx(s) {
                    Some(i) if i >= at => Some(Seg::Index(i + 1)),
                    _ => Some(s.clone()),
                });
                fs.expanded.insert((file.to_owned(), list.index(at)));
            }
            DocEdit::Move(list, from, to) => {
                fs.clear_drafts(file);
                let (from, to) = (*from, *to);
                fs.remap(file, list, |s| match idx(s) {
                    Some(i) if i == from => Some(Seg::Index(to)),
                    Some(i) if from < to && i > from && i <= to => Some(Seg::Index(i - 1)),
                    Some(i) if to < from && i >= to && i < from => Some(Seg::Index(i + 1)),
                    _ => Some(s.clone()),
                });
            }
            DocEdit::InsertKey(map, key, _) => {
                fs.clear_drafts(file);
                fs.expanded.insert((file.to_owned(), map.key(key)));
            }
            DocEdit::RenameKey(map, from, to) => {
                fs.clear_drafts(file);
                let (from, to) = (from.clone(), to.clone());
                fs.remap(file, map, |s| match s {
                    Seg::Key(k) if *k == from => Some(Seg::Key(to.clone())),
                    _ => Some(s.clone()),
                });
            }
            DocEdit::SwitchVariant(..) => fs.clear_drafts(file),
        }
        Ok(())
    }

    /// A form UI message on `file` (drafts, expansion, typed input that may edit the doc).
    pub fn form_msg(&mut self, file: &str, m: FormMsg) {
        use form::draft_key;
        let fs = &mut self.form;
        let edit = match m {
            FormMsg::Toggle(p) => {
                fs.toggle(file, &p);
                None
            }
            FormMsg::Text {
                path,
                text,
                required,
                path_field,
            } => {
                if path_field {
                    fs.active_path = Some((file.to_owned(), path.clone()));
                }
                Some(text_edit(&self.files, file, path, text, required))
            }
            FormMsg::PickPath { path, text } => {
                fs.active_path = None;
                Some(DocEdit::Set(path, ScalarValue::Str(text)))
            }
            FormMsg::Number { path, text, float } => {
                let key = draft_key("num", &path);
                fs.set_draft(file, key, Some(text.clone()));
                if text.trim().is_empty() {
                    let present = self
                        .files
                        .get(file)
                        .and_then(|f| f.doc.as_ref())
                        .is_some_and(|d| d.get(&path).is_some());
                    present.then_some(DocEdit::Remove(path))
                } else {
                    form::parse_number(&text, float).map(|v| match v.as_integer() {
                        Some(i) => DocEdit::Set(path, ScalarValue::Int(i)),
                        None => DocEdit::Set(path, ScalarValue::Float(v.as_float().unwrap_or(0.0))),
                    })
                }
            }
            FormMsg::ChipDraft { path, text } => {
                fs.set_draft(file, draft_key("chip", &path), Some(text));
                None
            }
            FormMsg::ChipAdd { path, value } => {
                let key = draft_key("chip", &path);
                let v = value.or_else(|| fs.draft(file, &key).map(str::to_owned));
                fs.set_draft(file, key, None);
                let v = v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
                v.map(|v| {
                    let mut items = self.str_list(file, &path);
                    items.push(v);
                    DocEdit::Set(path, ScalarValue::StrList(items))
                })
            }
            FormMsg::ChipRemove { path, index } => {
                let mut items = self.str_list(file, &path);
                (index < items.len()).then(|| {
                    items.remove(index);
                    DocEdit::Set(path, ScalarValue::StrList(items))
                })
            }
            FormMsg::NewKeyDraft { map, text } => {
                fs.set_draft(file, draft_key("newkey", &map), Some(text));
                None
            }
            FormMsg::NewKeyAdd { map, value } => {
                let key = draft_key("newkey", &map);
                let name = fs.draft(file, &key).unwrap_or_default().trim().to_owned();
                if name.is_empty() {
                    None
                } else {
                    fs.set_draft(file, key, None);
                    Some(DocEdit::InsertKey(map, name, value))
                }
            }
            FormMsg::RenameDraft { at, text } => {
                fs.set_draft(file, draft_key("rename", &at), Some(text));
                None
            }
            FormMsg::RenameCommit { map, from } => {
                let key = draft_key("rename", &map.key(&from));
                let to = fs.draft(file, &key).unwrap_or(&from).trim().to_owned();
                fs.set_draft(file, key, None);
                (!to.is_empty() && to != from).then_some(DocEdit::RenameKey(map, from, to))
            }
        };
        if let Some(e) = edit {
            let _ = self.edit(file, &e);
        }
    }

    fn str_list(&self, file: &str, path: &cv_schema::Path) -> Vec<String> {
        self.files
            .get(file)
            .and_then(|f| f.doc.as_ref())
            .and_then(|d| d.get_str_list(path))
            .unwrap_or_default()
    }

    /// Registers a [`ConfigFile`] for every `views/*.toml` among `paths` (the explorer's path
    /// list — no new daemon call), not yet known.
    pub fn sync_views(&mut self, paths: &[String]) {
        for p in paths {
            if is_view_path(p) && !self.files.contains_key(p.as_str()) {
                self.files.insert(p.clone(), view_file(p));
            }
        }
        if self.view_selected.is_none() {
            self.view_selected = self.view_paths().into_iter().next();
        }
    }

    /// Every known view file, sorted.
    pub fn view_paths(&self) -> Vec<String> {
        self.files
            .keys()
            .filter(|k| is_view_path(k))
            .cloned()
            .collect()
    }

    /// "New view file": an absent [`ConfigFile`] at `views/<name>.toml` (dirty, so Save
    /// creates it at the exact name). Returns the path, or why not.
    pub fn new_view_file(&mut self, name: &str) -> Result<String, String> {
        let name = name.trim();
        let stem = name.strip_suffix(".toml").unwrap_or(name);
        if stem.is_empty()
            || !stem
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err("a view file name is letters, digits, '-' or '_'".into());
        }
        let path = format!("{VIEWS_DIR}/{stem}.toml");
        if self.files.contains_key(&path) {
            return Err(format!("{path} already exists"));
        }
        let mut f = view_file(&path);
        f.doc = Some(ConfigDoc::new());
        f.state = LoadState::Absent;
        f.dirty = true;
        f.edit_seq = 1;
        f.revalidate();
        self.files.insert(path.clone(), f);
        self.view_selected = Some(path.clone());
        Ok(path)
    }
}

/// The views folder.
pub const VIEWS_DIR: &str = "_custom/crucible-vault/views";

/// A direct `views/*.toml` child.
pub fn is_view_path(p: &str) -> bool {
    p.strip_prefix(VIEWS_DIR)
        .and_then(|r| r.strip_prefix('/'))
        .is_some_and(|n| !n.contains('/') && n.ends_with(".toml") && n.len() > 5)
}

fn view_file(path: &str) -> ConfigFile {
    let mut f = ConfigFile::new(path, Some(cv_schema::view_schema()));
    f.local_check = Some(views_check);
    f
}

/// The Views carve-out's local diagnostics: the `views` crate's own parse (blocking) and
/// compile (notices → warnings), as the daemon's `views::def::load_dir` runs them.
pub fn views_check(path: &str, text: &str) -> (Vec<String>, Vec<String>) {
    let stem = path
        .rsplit('/')
        .next()
        .and_then(|n| n.strip_suffix(".toml"))
        .unwrap_or("view");
    match toml::from_str::<::views::def::BaseDef>(text) {
        Err(e) => (vec![format!("views: {}", e.message().trim())], Vec::new()),
        Ok(def) => {
            let mut notices = Vec::new();
            let base = ::views::def::Base::compile(stem, &def, &mut notices);
            for v in &base.views {
                notices.extend(v.errors.iter().map(|e| format!("view `{}`: {e}", v.name)));
            }
            (Vec::new(), notices)
        }
    }
}

fn text_edit(
    files: &BTreeMap<String, ConfigFile>,
    file: &str,
    path: cv_schema::Path,
    text: String,
    required: bool,
) -> DocEdit {
    let present = files
        .get(file)
        .and_then(|f| f.doc.as_ref())
        .is_some_and(|d| d.get(&path).is_some());
    if text.is_empty() && !required && present {
        DocEdit::Remove(path)
    } else {
        DocEdit::Set(path, ScalarValue::Str(text))
    }
}

/// Form UI messages (typing, expansion); see [`Settings::form_msg`].
#[derive(Debug, Clone)]
pub enum FormMsg {
    /// Expand / collapse a list item or map entry.
    Toggle(cv_schema::Path),
    /// A text field changed (empty + optional removes the key).
    Text {
        path: cv_schema::Path,
        text: String,
        required: bool,
        path_field: bool,
    },
    /// A vault-path suggestion was picked.
    PickPath {
        path: cv_schema::Path,
        text: String,
    },
    /// A number field changed (applied when it parses).
    Number {
        path: cv_schema::Path,
        text: String,
        float: bool,
    },
    ChipDraft {
        path: cv_schema::Path,
        text: String,
    },
    /// Add a chip (`None`: the draft).
    ChipAdd {
        path: cv_schema::Path,
        value: Option<String>,
    },
    ChipRemove {
        path: cv_schema::Path,
        index: usize,
    },
    NewKeyDraft {
        map: cv_schema::Path,
        text: String,
    },
    NewKeyAdd {
        map: cv_schema::Path,
        value: form::NewValue,
    },
    RenameDraft {
        at: cv_schema::Path,
        text: String,
    },
    RenameCommit {
        map: cv_schema::Path,
        from: String,
    },
}

/// Settings messages (wrapped in [`Message::Settings`]).
#[derive(Debug, Clone)]
pub enum SettingsMsg {
    /// Open (or focus) the Settings tab (Ctrl+,).
    Open,
    /// Select a rail section.
    Section(Section),
    /// An Editor-page control changed.
    Pref(editor_prefs::PrefEdit),
    /// A generic document edit on a file page (WP-3 forms emit these).
    Edit(String, DocEdit),
    /// A form UI message on a file page.
    Form(String, FormMsg),
    /// Views page: select a file.
    ViewSelect(String),
    /// Views page: the new-file name field.
    NewViewName(String),
    /// Views page: create the named file (absent until saved).
    NewViewCreate,
    /// Write `path` now (a due auto-save or a flush); queued behind an in-flight write.
    Save(String),
    /// An auto-save debounce timer fired for `(path, edit_seq)`; stale sequences are ignored.
    AutoSaveDue(String, u64),
    /// Reload from disk, dropping edits.
    Revert(String),
    /// A malformed file: start from an empty doc.
    StartOver(String),
    /// A save finished.
    Saved(String, Result<WriteOutcome, String>),
    /// Refresh the Status page.
    RefreshStatus,
    /// The `status` RPC returned.
    StatusLoaded(Result<cv_protocol::StatusResult, String>),
    /// A Hotkeys page control (WP-4).
    Hotkey(pages::hotkeys::HotkeyMsg),
    /// Confirm-before-Trash: go ahead / cancel.
    TrashConfirm,
    TrashCancel,
}

/// A generic document edit (WP-3 grows the form vocabulary on top of these).
#[derive(Debug, Clone)]
pub enum DocEdit {
    /// Set a scalar at a schema path.
    Set(cv_schema::Path, ScalarValue),
    /// Remove the item at a path.
    Remove(cv_schema::Path),
    /// Insert a table record into a list at an index, with initial scalar fields (a union's
    /// tag among them).
    InsertRecord(cv_schema::Path, usize, Vec<(String, ScalarValue)>),
    /// Move a list item `from` → `to`.
    Move(cv_schema::Path, usize, usize),
    /// Add a map key.
    InsertKey(cv_schema::Path, String, form::NewValue),
    /// Rename a map key (position, value and comments kept).
    RenameKey(cv_schema::Path, String, String),
    /// Switch a tagged union's kind at a path: keys the old kind declares and the new one does
    /// not are dropped; shared and unknown keys stay.
    SwitchVariant(cv_schema::Path, &'static cv_schema::Union, String),
}

/// A scalar value for [`DocEdit::Set`].
#[derive(Debug, Clone, PartialEq)]
pub enum ScalarValue {
    Str(String),
    Bool(bool),
    Int(i64),
    Float(f64),
    StrList(Vec<String>),
}

impl ScalarValue {
    fn value(&self) -> cv_schema::toml_edit::Value {
        use cv_schema::toml_edit::{Array, Value};
        match self {
            ScalarValue::Str(s) => s.as_str().into(),
            ScalarValue::Bool(b) => (*b).into(),
            ScalarValue::Int(i) => (*i).into(),
            ScalarValue::Float(f) => (*f).into(),
            ScalarValue::StrList(l) => {
                let mut a = Array::new();
                for s in l {
                    a.push(s.as_str());
                }
                Value::Array(a)
            }
        }
    }
}

impl DocEdit {
    /// Applies the edit to `doc`.
    pub fn apply(&self, doc: &mut ConfigDoc) -> Result<(), DocError> {
        match self {
            DocEdit::Set(p, v) => doc.set(p, v.value()),
            DocEdit::Remove(p) => doc.remove(p).map(|_| ()),
            DocEdit::InsertRecord(list, at, fields) => {
                let mut t = cv_schema::toml_edit::Table::new();
                for (k, v) in fields {
                    t.insert(k, cv_schema::toml_edit::Item::Value(v.value()));
                }
                doc.insert_record(list, *at, t)
            }
            DocEdit::Move(list, from, to) => doc.move_item(list, *from, *to),
            DocEdit::InsertKey(map, key, v) => doc.insert_key(map, key, v.item()),
            DocEdit::RenameKey(map, from, to) => doc.rename_key(map, from, to),
            DocEdit::SwitchVariant(p, u, tag) => {
                let new = u
                    .variant(tag)
                    .ok_or_else(|| DocError::BadPath(format!("{p}: unknown kind `{tag}`")))?;
                if let Some(old) = doc.get_str(&p.key(u.tag)).and_then(|t| u.variant(t)) {
                    for k in doc.keys(p) {
                        if k != u.tag
                            && old.record.field(&k).is_some()
                            && new.record.field(&k).is_none()
                        {
                            doc.remove(&p.key(&k))?;
                        }
                    }
                }
                doc.set(&p.key(u.tag), tag.as_str())
            }
        }
    }
}

fn msg(m: SettingsMsg) -> Message {
    Message::Settings(m)
}

/// The rail + page.
pub fn view<'a>(
    s: &'a Settings,
    prefs: &'a editor_prefs::Prefs,
    paths: &'a [String],
    t: Tokens,
    mode: cv_protocol::Mode,
) -> Element<'a, Message> {
    let mut rail = column![].spacing(2).width(Length::Fixed(220.0));
    for sec in Section::ALL {
        if let Some(g) = sec.group() {
            rail = rail
                .push(container(text(g).size(12).color(t.text_faint.to_color())).padding([6, 10]));
        }
        let on = s.section == sec;
        let dirty = sec.file().and_then(|p| s.file(p)).is_some_and(|f| f.dirty);
        let label = if dirty {
            format!("{} •", sec.label())
        } else {
            sec.label().to_owned()
        };
        rail = rail.push(
            button(
                row![
                    text(sec.icon().glyph().to_string())
                        .font(crate::icons::FONT)
                        .size(crate::theme::ICON_SIZE),
                    text(label).size(14),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center),
            )
            .height(crate::theme::SEGMENT)
            .width(Length::Fill)
            .padding([0, 10])
            .style(crate::theme::segment_style(t, on))
            .on_press(msg(SettingsMsg::Section(sec))),
        );
    }
    let rail = container(rail)
        .padding(1)
        .style(crate::theme::seg_group_style(t));
    let body = scrollable(container(page(s, prefs, paths, t, mode)).padding([0, 16]))
        .width(Length::Fill)
        .height(Length::Fill);
    row![rail, body].spacing(12).height(Length::Fill).into()
}

/// One section's page (view build timed like `sync_buffers`: `CV_EDITOR_TRACE` debug line
/// when it takes ≥1 ms).
pub fn page<'a>(
    s: &'a Settings,
    prefs: &'a editor_prefs::Prefs,
    paths: &'a [String],
    t: Tokens,
    mode: cv_protocol::Mode,
) -> Element<'a, Message> {
    let t0 = std::time::Instant::now();
    let el = page_inner(s, prefs, paths, t, mode);
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    if ms >= 1.0 {
        tracing::debug!(
            elapsed_ms = ms,
            section = s.section.label(),
            "settings::page"
        );
    }
    el
}

fn page_inner<'a>(
    s: &'a Settings,
    prefs: &'a editor_prefs::Prefs,
    paths: &'a [String],
    t: Tokens,
    mode: cv_protocol::Mode,
) -> Element<'a, Message> {
    let cx = pages::PageCx { s, paths, t, mode };
    let heading = text(s.section.label()).size(20).color(t.heading.to_color());
    let body: Element<'a, Message> = match s.section {
        Section::Editor => match s.file(editor_prefs::EDITOR_PATH) {
            Some(f) => editor_prefs::view(
                f,
                prefs,
                s.editor_note.as_deref(),
                s.hidden_draft.as_deref(),
                t,
                mode,
            ),
            None => Space::new().into(),
        },
        Section::Status => status::view(&s.status, t),
        Section::Hotkeys => pages::hotkeys::view(cx),
        Section::Views => pages::views::view(cx),
        sec => match sec.file().and_then(|p| s.file(p)) {
            Some(f) => match sec {
                Section::Actions => pages::actions::view(cx, f),
                Section::Schedules => pages::schedules::view(cx, f),
                Section::Captures => pages::captures::view(cx, f),
                Section::Kinds => pages::kinds::view(cx, f),
                Section::Agents => pages::agents::view(cx, f),
                Section::Providers => pages::providers::view(cx, f),
                Section::Bookmarks => pages::bookmarks::view(cx, f),
                _ => pages::generic(cx, f, ""),
            },
            None => Space::new().into(),
        },
    };
    column![heading, body].spacing(12).into()
}

/// The file path, load state, form errors, reload errors and notice of a file page.
pub fn file_header(f: &ConfigFile, t: Tokens) -> Element<'_, Message> {
    let state = match &f.state {
        LoadState::NotLoaded => "not loaded".to_owned(),
        LoadState::Absent => "absent (the first valid edit creates it)".to_owned(),
        LoadState::Loaded => "loaded".to_owned(),
        LoadState::Malformed(e) => format!("does not parse: {e}"),
        LoadState::Error(e) => format!("read failed: {e}"),
    };
    let bad = matches!(f.state, LoadState::Malformed(_) | LoadState::Error(_));
    let mut col = column![
        text(f.path.as_str()).size(13).color(t.text_dim.to_color()),
        text(state).size(13).color(if bad {
            t.error_text.to_color()
        } else {
            t.text_faint.to_color()
        }),
    ]
    .spacing(4);
    if let Some(n) = &f.notice {
        col = col.push(text(n.as_str()).size(13).color(t.warn_text.to_color()));
    }
    for e in &f.errors {
        col = col.push(text(e.to_string()).size(13).color(t.error_text.to_color()));
    }
    for w in &f.warnings {
        col = col.push(text(w.as_str()).size(13).color(t.warn_text.to_color()));
    }
    for e in &f.reload_errors {
        col = col.push(
            row![
                text(Icon::TriangleAlert.glyph().to_string())
                    .font(crate::icons::FONT)
                    .size(crate::theme::ICON_SIZE)
                    .color(t.warn_text.to_color()),
                text(format!("daemon: {e}"))
                    .size(13)
                    .color(t.warn_text.to_color()),
            ]
            .spacing(6),
        );
    }
    col.into()
}

/// The save status line (plan `editor-feedback-round-2` WP-2: replaces the Save / Revert bar):
/// "saving…" / "saved" / "not saved: N errors", "Start from defaults" for a malformed file,
/// and the shadow-mode note. Fixed height.
pub fn save_bar(f: &ConfigFile, t: Tokens, mode: cv_protocol::Mode) -> Element<'_, Message> {
    let shadow = mode == cv_protocol::Mode::Shadow;
    let mut bar = row![].spacing(8).align_y(iced::Alignment::Center);
    if matches!(f.state, LoadState::Malformed(_)) && f.doc.is_none() {
        bar = bar.push(
            button(crate::theme::control_label("Start from defaults", 14.0))
                .height(crate::theme::CONTROL)
                .padding([0, 14])
                .style(crate::theme::icon_button_style(t, false))
                .on_press(msg(SettingsMsg::StartOver(f.path.clone()))),
        );
    }
    if shadow {
        bar = bar.push(
            text("saving is disabled in shadow mode")
                .size(13)
                .color(t.warn_text.to_color()),
        );
    } else if let Some((label, warn)) = f.save_status() {
        let c = if warn { t.warn_text } else { t.text_faint };
        bar = bar.push(text(label).size(13).color(c.to_color()));
    }
    container(bar)
        .height(crate::theme::CONTROL)
        .align_y(iced::Alignment::Center)
        .style(|_: &Theme| container::Style::default())
        .into()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod form_tests;

#[cfg(test)]
mod hotkeys_tests;
