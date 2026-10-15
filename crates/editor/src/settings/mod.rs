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
//! - **Edit**: [`ConfigFile::edit`] applies a `cv_schema` document edit, marks the file dirty and
//!   re-runs the form-level [`cv_schema::validate`]; any error blocks Save.
//! - **Save / Revert**: [`ConfigFile::begin_save`] yields a [`SaveJob`] that the app runs off
//!   the UI thread through [`write_config`] — the one exact-name config write (whole file,
//!   `write_note` CAS on the last-read hash, or the empty-hash absence-CAS when the file was
//!   absent; never `create_note`, so a fixed config name never gets a collision suffix).
//!   [`ConfigFile::finish_save`] lands the outcome; a conflict reloads the file and sets the
//!   "changed on disk" notice — it never overwrites. Revert is a reload.
//! - **Daemon feedback**: `Notification::ConfigReloaded { errors }` is routed per file by
//!   [`Settings::route_reload_errors`] ([`attribute`]); unattributable errors go to Status.
//!
//! # Extension point (WP-3)
//!
//! A page is a [`Section`] plus, for file pages, its [`ConfigFile`]. WP-3 adds
//! `settings/{form.rs,widgets.rs,pages/*.rs}` and replaces the body of [`page`]'s placeholder
//! arm for its section with a form that reads `file.doc` and emits
//! [`SettingsMsg::Edit`]`(path, DocEdit)`; the save loop, the Save/Revert bar ([`save_bar`]),
//! the load-state / error header ([`file_header`]) and the routing stay as they are.
//! Editor prefs (and the `[keys]` table WP-4 consumes) are [`editor_prefs::Prefs`], held on the
//! app as `App::prefs`.

pub mod editor_prefs;
pub mod status;

use std::collections::BTreeMap;

use cv_schema::{ConfigDoc, DocError, FieldError, Node};
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
    pub path: &'static str,
    pub content: String,
    pub base: Option<ContentHash>,
}

impl SaveJob {
    /// Runs the write (blocking).
    pub fn run(&self, d: &dyn DaemonClient) -> Result<WriteOutcome, String> {
        write_config(d, self.path, &self.content, self.base.as_ref())
    }
}

/// The notice set when a save hit a CAS conflict (the file was reloaded).
pub const CHANGED_ON_DISK: &str = "changed on disk; reloaded (your edits were not saved)";

/// One config file under edit: the generic per-file save loop.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    /// Vault-relative path.
    pub path: &'static str,
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
}

impl ConfigFile {
    /// An unloaded file.
    pub fn new(path: &'static str, schema: Option<&'static Node>) -> Self {
        Self {
            path,
            schema,
            doc: None,
            base: None,
            state: LoadState::NotLoaded,
            dirty: false,
            errors: Vec::new(),
            reload_errors: Vec::new(),
            notice: None,
            saving: false,
        }
    }

    /// (Re)loads from disk, dropping unsaved edits.
    pub fn load(&mut self, d: &dyn DaemonClient) {
        self.apply_read(d.read_file(self.path));
    }

    /// Installs a read result (pure; [`ConfigFile::load`] feeds it).
    pub fn apply_read(&mut self, r: Result<(Vec<u8>, ContentHash), DaemonError>) {
        self.dirty = false;
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
            self.revalidate();
        }
    }

    /// Whether Save is enabled.
    pub fn can_save(&self) -> bool {
        self.dirty && !self.saving && self.doc.is_some() && self.errors.is_empty()
    }

    /// The save job for the current doc (`None` when Save is disabled).
    pub fn begin_save(&mut self) -> Option<SaveJob> {
        if !self.can_save() {
            return None;
        }
        let content = self.doc.as_ref()?.to_string();
        self.saving = true;
        Some(SaveJob {
            path: self.path,
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
                self.dirty = false;
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
            self.notice = Some("changed on disk; saving will conflict (revert to reload)".into());
            return false;
        }
        self.load(d);
        true
    }
}

/// Every editable file, in rail order.
fn new_files() -> BTreeMap<&'static str, ConfigFile> {
    Section::ALL
        .into_iter()
        .filter_map(|s| {
            let path = s.file()?;
            let schema = cv_schema::schema_for(s.file_name()?);
            Some((path, ConfigFile::new(path, schema)))
        })
        .collect()
}

/// The Settings tab's model (lives on the app whether or not the tab is open).
#[derive(Debug)]
pub struct Settings {
    pub section: Section,
    /// Every file page's save loop, keyed by vault path.
    pub files: BTreeMap<&'static str, ConfigFile>,
    pub status: status::StatusPage,
    /// Malformed-`editor.toml` / migration notes shown on the Editor page.
    pub editor_note: Option<String>,
    /// A pending confirm-before-Trash (the explorer path), when `confirm_trash` is on.
    pub confirm_trash: Option<String>,
    /// The hidden-commands field as typed (reset on load/revert).
    pub hidden_draft: Option<String>,
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

    /// Whether any file page has unsaved edits.
    pub fn any_dirty(&self) -> bool {
        self.files.values().any(|f| f.dirty)
    }
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
    Edit(&'static str, DocEdit),
    Save(&'static str),
    /// Reload from disk, dropping edits.
    Revert(&'static str),
    /// A malformed file: start from an empty doc.
    StartOver(&'static str),
    /// A save finished.
    Saved(&'static str, Result<WriteOutcome, String>),
    /// Refresh the Status page.
    RefreshStatus,
    /// The `status` RPC returned.
    StatusLoaded(Result<cv_protocol::StatusResult, String>),
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

impl DocEdit {
    /// Applies the edit to `doc`.
    pub fn apply(&self, doc: &mut ConfigDoc) -> Result<(), DocError> {
        use cv_schema::toml_edit::{Array, Value};
        match self {
            DocEdit::Set(p, v) => {
                let value: Value = match v {
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
                };
                doc.set(p, value)
            }
            DocEdit::Remove(p) => doc.remove(p).map(|_| ()),
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
    let body = scrollable(container(page(s, prefs, t, mode)).padding([0, 16]))
        .width(Length::Fill)
        .height(Length::Fill);
    row![rail, body].spacing(12).height(Length::Fill).into()
}

/// One section's page.
pub fn page<'a>(
    s: &'a Settings,
    prefs: &'a editor_prefs::Prefs,
    t: Tokens,
    mode: cv_protocol::Mode,
) -> Element<'a, Message> {
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
        Section::Hotkeys => placeholder(t, "Hotkey configuration arrives with the Hotkeys page."),
        Section::Views => placeholder(
            t,
            "One page per _custom/crucible-vault/views/*.toml arrives with the Views forms.",
        ),
        // WP-3: the config-file forms replace this arm per section.
        sec => match sec.file().and_then(|p| s.file(p)) {
            Some(f) => column![
                file_header(f, t),
                placeholder(
                    t,
                    "The typed form for this file arrives with the config forms."
                ),
                save_bar(f, t, mode),
            ]
            .spacing(10)
            .into(),
            None => Space::new().into(),
        },
    };
    column![heading, body].spacing(12).into()
}

fn placeholder(t: Tokens, why: &str) -> Element<'_, Message> {
    text(why).size(14).color(t.text_faint.to_color()).into()
}

/// The file path, load state, form errors, reload errors and notice of a file page.
pub fn file_header(f: &ConfigFile, t: Tokens) -> Element<'_, Message> {
    let state = match &f.state {
        LoadState::NotLoaded => "not loaded".to_owned(),
        LoadState::Absent => "absent (Save creates it)".to_owned(),
        LoadState::Loaded => "loaded".to_owned(),
        LoadState::Malformed(e) => format!("does not parse: {e}"),
        LoadState::Error(e) => format!("read failed: {e}"),
    };
    let bad = matches!(f.state, LoadState::Malformed(_) | LoadState::Error(_));
    let mut col = column![
        text(f.path).size(13).color(t.text_dim.to_color()),
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

/// Save / Revert (and Start over for a malformed file). Shadow mode disables Save.
pub fn save_bar(f: &ConfigFile, t: Tokens, mode: cv_protocol::Mode) -> Element<'_, Message> {
    let shadow = mode == cv_protocol::Mode::Shadow;
    let style = move |on: bool| crate::theme::icon_button_style(t, on);
    let save = button(text(if f.saving { "Saving…" } else { "Save" }).size(14))
        .height(crate::theme::CONTROL)
        .padding([0, 14])
        .style(style(f.can_save() && !shadow))
        .on_press_maybe((f.can_save() && !shadow).then_some(msg(SettingsMsg::Save(f.path))));
    let revert = button(text("Revert").size(14))
        .height(crate::theme::CONTROL)
        .padding([0, 14])
        .style(style(false))
        .on_press_maybe((!f.saving).then_some(msg(SettingsMsg::Revert(f.path))));
    let mut bar = row![save, revert]
        .spacing(8)
        .align_y(iced::Alignment::Center);
    if matches!(f.state, LoadState::Malformed(_)) && f.doc.is_none() {
        bar = bar.push(
            button(text("Start from defaults").size(14))
                .height(crate::theme::CONTROL)
                .padding([0, 14])
                .style(style(false))
                .on_press(msg(SettingsMsg::StartOver(f.path))),
        );
    }
    if f.dirty {
        bar = bar.push(text("unsaved").size(13).color(t.warn_text.to_color()));
    }
    if shadow {
        bar = bar.push(
            text("saving is disabled in shadow mode")
                .size(13)
                .color(t.warn_text.to_color()),
        );
    }
    container(bar)
        .style(|_: &Theme| container::Style::default())
        .into()
}

#[cfg(test)]
mod tests;
