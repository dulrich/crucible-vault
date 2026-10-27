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
//! Every page that edits a file owns one [`ConfigFile`] (`save_loop.rs`) in [`Settings::files`], keyed by its
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
//!   timer that fires during a write sets the in-flight [`Phase::Writing`]'s `requeue`
//!   ([`ConfigFile::request_save`]) and the write re-runs once the first lands
//!   ([`ConfigFile::finish_save`] → [`FinishOutcome`]). Closing the Settings tab and quitting flush pending writes.
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
//! `settings/{form.rs,widgets.rs,pages/*.rs}` and replaces the body of [`view::page`]'s placeholder
//! arm for its section with a form that reads `file.doc()` and emits
//! [`SettingsMsg::Edit`]`(path, DocEdit)`; the save loop, the save status line ([`view::save_bar`]),
//! the load-state / error header ([`view::file_header`]) and the routing stay as they are.
//! Editor prefs (and the `[keys]` table the WP-4 keymap applies) are [`editor_prefs::Prefs`], held on the
//! app as `App::prefs`.

pub mod captures;
pub mod edit;
pub mod editor_prefs;
pub mod form;
mod form_state;
pub mod input;
pub mod lookups;
pub mod pages;
pub mod pickers;
pub mod save_loop;
pub mod section;
pub mod sections;
pub mod status;
pub mod view;
pub mod view_files;
pub mod widgets;

use std::collections::BTreeMap;

pub use captures::{CAPTURES_PATH, period_folders};
pub use edit::{DocEdit, ScalarValue};
pub use input::{FormMsg, traverse_key_event, typeahead_key_event};
pub use save_loop::{
    AUTOSAVE_DELAY, CHANGED_ON_DISK, ConfigFile, FinishOutcome, LoadState, LocalCheck, Phase,
    SaveJob, WriteOutcome, write_config,
};
pub use section::{attribute, body_scroll_id};
pub use sections::{CONFIG_DIR, SECTIONS, Section, SectionInfo};
pub use view::{file_header, page, save_bar, view};
pub use view_files::{VIEWS_DIR, is_view_path, views_check};

use crate::daemon::DaemonClient;

/// The Settings tab's model (lives on the app whether or not the tab is open).
#[derive(Debug)]
pub struct Settings {
    pub section: Section,
    /// Every file page's save loop, keyed by vault path.
    pub files: BTreeMap<String, ConfigFile>,
    pub status: status::StatusPage,
    /// Malformed-`editor.toml` / migration notes shown on the Editor page.
    pub editor_note: Option<String>,
    /// Form UI state (expansion, drafts) of every file page.
    pub form: form::FormState,
    /// The Views page's selected file (vault path).
    pub view_selected: Option<String>,
    /// The "New view file" name field.
    pub new_view: String,
    /// The Hotkeys page (WP-4): capture state and keymap / action snapshots.
    pub hotkeys: pages::hotkeys::HotkeyState,
    /// Plan round-5 WP-1: each section's last body scroll offset (px), re-applied on section /
    /// tab activation — iced scrollable state is positional, never retained across them.
    pub body_offsets: std::collections::HashMap<Section, f32>,
    /// Plan round-5 WP-6: the network-backed pickers' session cache.
    pub lookups: lookups::Lookups,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            section: Section::Editor,
            files: save_loop::new_files(),
            status: status::StatusPage::default(),
            editor_note: None,
            form: form::FormState::default(),
            view_selected: None,
            new_view: String::new(),
            hotkeys: pages::hotkeys::HotkeyState::default(),
            body_offsets: std::collections::HashMap::new(),
            lookups: lookups::Lookups::default(),
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
            if *f.state() == LoadState::NotLoaded {
                f.load(d);
            }
        }
    }
}

/// Settings messages (wrapped in [`crate::message::Message::Settings`]).
#[derive(Debug, Clone)]
pub enum SettingsMsg {
    /// Open (or focus) the Settings tab (Ctrl+,).
    Open,
    /// Plan round-5 WP-4: a key for the active form typeahead.
    Typeahead(crate::widget::typeahead::Key),
    /// Plan round-13 WP-2: Tab (`false`) / Shift+Tab (`true`) moves focus to the next / previous
    /// input of the Settings page.
    Traverse(bool),
    /// Select a rail section.
    Section(Section),
    /// Plan round-5 WP-1: a section's body scrolled to vertical offset `.1` (px).
    BodyScrolled(Section, f32),
    /// A generic document edit on a file page (WP-3 forms emit these).
    Edit(String, DocEdit),
    /// A form UI message on a file page.
    Form(String, FormMsg),
    /// Plan round-5 WP-6: a picker lookup result (or the geocode debounce tick).
    Lookup(lookups::LookupMsg),
    /// Views page: select a file.
    ViewSelect(String),
    /// Views page: the new-file name field.
    NewViewName(String),
    /// Views page: create the named file (absent until saved).
    NewViewCreate,
    /// An auto-save debounce timer fired for `(path, edit_seq)`: writes `path` (queued behind
    /// an in-flight write); stale sequences are ignored.
    AutoSaveDue(String, u64),
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
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod form_tests;
