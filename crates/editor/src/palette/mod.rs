//! Command palette (WP-5, Ctrl+P): daemon actions merged with client commands, typed parameter
//! prompts, action invocation with context (save-first), key chords and the job toast.
//!
//! Entries: [`ClientCmd`]s first, then `actions_list` (fetched off the UI thread on first open or
//! first chord, cached until invalidated — config reload or a change under
//! [`CONFIG_PREFIX`]). Ranking is `editor_model::rank` over `"title group"`. Invocation of an
//! action: prompt each `ParamSpec` ([`prompt::Prompt`]), then — if the active buffer is dirty and
//! the daemon is live — save it and dispatch `run_action` only on a successful save with the new
//! hash (the app's `run_call`/`resume_after`).

pub mod chord;
pub mod commands;
pub mod hints;
pub mod jobs;
pub mod keymap;
pub mod prompt;
#[cfg(test)]
mod tests;
mod view;

use std::time::Duration;

use cv_protocol::{ActionContext, ActionInfo, ParamKind};
use editor_model::rank::{RankMode, rank};
use iced::Subscription;
use iced::keyboard::{self, Key, key::Named};
use iced::widget::text_editor;
use serde_json::{Map, Value};

use crate::message::{Message, PaletteMsg, PanelsMsg, PanesMsg};
use crate::source::SourceBuffer;
use chord::Chord;
pub use commands::{ClientCmd, CmdSpec, DEFAULT_ACTIONS, PERIOD_OPEN, PeriodSel, Preset, keymap};
pub use jobs::{Jobs, TOAST_TTL, Toast, state_label, terminal};
pub use keymap::{Bindable, Bound, KeySource, Keymap, apply_keys, bindables, resolve, resolve_all};
use prompt::Prompt;
pub use view::view;

/// The query / prompt field's widget id.
pub const INPUT_ID: &str = "palette-input";
/// Result rows shown.
pub const SHOWN: usize = 14;
/// Changes under this prefix invalidate the cached action list (definitions are hot-reloaded).
pub const CONFIG_PREFIX: &str = cv_schema::CONFIG_DIR;
/// Path suggestions shown under a Path prompt.
pub const SUGGESTIONS: usize = 8;

/// Param-prompt inputs.
#[derive(Debug, Clone)]
pub enum PromptMsg {
    Text(String),
    Bool(bool),
    Choice(String),
    Editor(text_editor::Action),
    /// A path suggestion was picked.
    Suggest(String),
    Back,
}

/// What an entry runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryId {
    Client(ClientCmd),
    Action(String),
    /// Plan editor-ui-round-3 WP-9: "Open bookmark: <name>" (the bookmark's path).
    Bookmark(String),
}

/// The stable bindable id of the bookmark at `path` (WP-9): keyed by the stored path, so it
/// survives reorders and renames of other bookmarks.
pub fn bookmark_id(path: &str) -> String {
    format!("bookmark:{path}")
}

/// The palette rows of `bookmarks` (WP-9), with their stored chords.
pub fn bookmark_entries(bookmarks: &[crate::bookmarks::Bookmark]) -> Vec<Entry> {
    bookmarks
        .iter()
        .map(|b| Entry {
            id: EntryId::Bookmark(b.path.clone()),
            title: format!("Open bookmark: {}", b.name),
            group: "Bookmarks".into(),
            keys: b.keys.clone(),
        })
        .collect()
}

/// One palette row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: EntryId,
    pub title: String,
    pub group: String,
    pub keys: Vec<String>,
}

/// An action ready to run (params collected).
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub action_id: String,
    pub title: String,
    pub params: Map<String, Value>,
    pub needs: Vec<cv_protocol::ContextNeed>,
}

/// Client commands then actions.
pub fn build_entries(actions: &[ActionInfo]) -> Vec<Entry> {
    let has = |id: &str| actions.iter().any(|a| a.id == id);
    let mut v: Vec<Entry> = ClientCmd::all()
        .into_iter()
        .filter(|c| !matches!(c, ClientCmd::OpenPeriod(_)) || has(PERIOD_OPEN))
        .map(|c| {
            let (title, group) = c.title();
            Entry {
                id: EntryId::Client(c),
                title,
                group: group.to_owned(),
                keys: c.keys(),
            }
        })
        .collect();
    v.extend(actions.iter().map(|a| {
        Entry {
            id: EntryId::Action(a.id.clone()),
            title: a.title.clone(),
            group: a.group.clone().unwrap_or_else(|| "Actions".into()),
            keys: DEFAULT_ACTIONS
                .iter()
                .filter(|(_, p)| p.period.is_none() && p.action == a.id)
                .map(|(k, _)| (*k).to_owned())
                .chain(a.keys.iter().cloned())
                .collect(),
        }
    }));
    v
}

/// `entries` without the ids in `hidden` (client command ids or action ids).
pub fn visible_entries(entries: Vec<Entry>, hidden: &[String]) -> Vec<Entry> {
    if hidden.is_empty() {
        return entries;
    }
    entries
        .into_iter()
        .filter(|e| {
            let id = match &e.id {
                EntryId::Client(c) => c.id(),
                EntryId::Action(a) => a.clone(),
                EntryId::Bookmark(p) => bookmark_id(p),
            };
            !hidden.contains(&id)
        })
        .collect()
}

/// Ranks `entries` against `query` (fuzzy over `"title group"`), best first, as indices.
pub fn rank_entries(query: &str, entries: &[Entry]) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..entries.len()).collect();
    }
    let hays = haystacks(entries);
    rank(query, &hays, RankMode::Plain, &[], |_| false, entries.len())
        .into_iter()
        .map(|r| r.index)
        .collect()
}

/// The ranking haystacks of `entries` (parallel, `"title group"`), ranked in
/// [`RankMode::Plain`] — results are indices, so duplicates need no disambiguation.
pub(crate) fn haystacks(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .map(|e| editor_model::hints::haystack(&e.title, &e.group))
        .collect()
}

/// The invocation context for the active tab: path, 1-based cursor line and buffer hash always;
/// the selection when non-empty. Plan round-5 WP-8: `reading_sel` (the focused tab's Reading
/// selection, when Reading is the most recent selecting side) wins over the buffer's.
pub fn context(tab: Option<&SourceBuffer>, reading_sel: Option<String>) -> ActionContext {
    let Some(tab) = tab else {
        return ActionContext::default();
    };
    let line = u32::try_from(tab.cursor().position.line + 1).ok();
    ActionContext {
        active_path: Some(tab.path().to_owned()),
        selection: reading_sel
            .filter(|s| !s.is_empty())
            .or_else(|| tab.selection().filter(|s| !s.is_empty())),
        value: None,
        line,
        buffer_hash: Some(tab.base_hash().as_str().to_owned()),
    }
}

/// The palette model.
pub struct Palette {
    pub open: bool,
    pub query: String,
    /// Ranked indices into `entries`.
    pub results: Vec<usize>,
    pub selected: usize,
    pub entries: Vec<Entry>,
    /// Cached `actions_list` (`None` = not fetched / invalidated).
    pub actions: Option<Vec<ActionInfo>>,
    pub loading: bool,
    /// The effective keymap (WP-4): rebuilt on action reload and `[keys]` change; shared with
    /// the source editors' `key_binding` closures.
    pub keymap: std::sync::Arc<Keymap>,
    /// The `editor.toml` `[keys]` overrides the keymap was built from.
    pub overrides: std::collections::BTreeMap<String, Vec<String>>,
    /// Key-binding conflicts, shown in the palette.
    pub notice: Vec<String>,
    pub prompt: Option<Prompt>,
    pub prompt_error: Option<String>,
    /// Backing store for a Multiline prompt.
    pub multiline: text_editor::Content,
    /// Path suggestions for a Path prompt.
    pub suggestions: Vec<String>,
    /// A chord pressed before the action list arrived.
    pub pending_chord: Option<Chord>,
    /// WP-R19: the job toast and origin bookkeeping.
    pub jobs: Jobs,
    /// `editor.toml` `palette.hidden`: ids left out of the list (chords still work).
    pub hidden: Vec<String>,
    /// WP-8: the cached minimum-subsequence hint per entry (parallel to `entries`), recomputed
    /// in `rebuild` (entry-set change) only.
    pub hints: Vec<Option<String>>,
    /// WP-9: the bookmarks whose "Open bookmark" commands and chords join the list / keymap.
    pub bookmarks: Vec<crate::bookmarks::Bookmark>,
}

impl Default for Palette {
    fn default() -> Self {
        let entries = build_entries(&[]);
        Self {
            hints: hints::compute(&entries),
            open: false,
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            entries,
            actions: None,
            loading: false,
            keymap: std::sync::Arc::new(Keymap::default()),
            overrides: std::collections::BTreeMap::new(),
            notice: Vec::new(),
            prompt: None,
            prompt_error: None,
            multiline: text_editor::Content::new(),
            suggestions: Vec::new(),
            pending_chord: None,
            jobs: Jobs::default(),
            hidden: Vec::new(),
            bookmarks: Vec::new(),
        }
    }
}

impl Palette {
    /// Opens the list with an empty query.
    pub fn show(&mut self) {
        self.open = true;
        self.prompt = None;
        self.prompt_error = None;
        self.query.clear();
        self.selected = 0;
        self.refresh();
    }

    /// Closes and cancels any prompt.
    pub fn close(&mut self) {
        self.open = false;
        self.prompt = None;
        self.prompt_error = None;
        self.suggestions.clear();
    }

    /// Re-ranks.
    pub fn refresh(&mut self) {
        self.results = rank_entries(&self.query, &self.entries);
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
    }

    /// New query.
    pub fn set_query(&mut self, q: String) {
        self.query = q;
        self.selected = 0;
        self.refresh();
    }

    /// The highlighted entry index.
    pub fn current(&self) -> Option<usize> {
        self.results.get(self.selected).copied()
    }

    /// Installs a fetched action list (entries + bindings).
    pub fn set_actions(&mut self, actions: Vec<ActionInfo>) {
        self.actions = Some(actions);
        self.loading = false;
        self.rebuild();
    }

    /// Sets the `[keys]` overrides and rebuilds the keymap (a no-op when unchanged).
    pub fn set_keys(&mut self, overrides: std::collections::BTreeMap<String, Vec<String>>) {
        if overrides == self.overrides {
            return;
        }
        self.overrides = overrides;
        self.rebuild();
    }

    /// Rebuilds keymap, notices and entries from the overrides and the cached actions.
    fn rebuild(&mut self) {
        let actions = self.actions.as_deref().unwrap_or_default();
        let km = resolve_all(&self.overrides, actions, &self.bookmarks);
        self.notice = km.notices.clone();
        let mut all = build_entries(actions);
        all.extend(bookmark_entries(&self.bookmarks));
        apply_keys(&mut all, &km);
        let entries = visible_entries(all, &self.hidden);
        if entries != self.entries || self.hints.len() != entries.len() {
            self.hints = hints::compute(&entries);
        }
        self.entries = entries;
        self.keymap = std::sync::Arc::new(km);
        self.refresh();
    }

    /// Sets the bookmarks (WP-9) and rebuilds keymap + entries (a no-op when unchanged).
    pub fn set_bookmarks(&mut self, bookmarks: Vec<crate::bookmarks::Bookmark>) {
        if bookmarks == self.bookmarks {
            return;
        }
        self.bookmarks = bookmarks;
        self.rebuild();
    }

    /// Sets the hidden ids and rebuilds the list (a no-op when unchanged).
    pub fn set_hidden(&mut self, hidden: Vec<String>) {
        if hidden == self.hidden {
            return;
        }
        self.hidden = hidden;
        self.rebuild();
    }

    /// Drops the cached action list (refetched on next use).
    pub fn invalidate(&mut self) {
        self.actions = None;
    }

    /// An action by id from the cache.
    pub fn action(&self, id: &str) -> Option<&ActionInfo> {
        self.actions.as_ref()?.iter().find(|a| a.id == id)
    }

    /// Starts prompting `info`'s params.
    pub fn begin_prompt(&mut self, info: ActionInfo, today: chrono::NaiveDate) {
        let p = Prompt::new(info, today);
        self.open = true;
        self.prompt_error = None;
        self.sync_step(&p);
        self.prompt = Some(p);
    }

    /// Re-seeds per-step widgets (multiline content) from the prompt's current value.
    pub fn sync_step(&mut self, p: &Prompt) {
        self.multiline = text_editor::Content::with_text(p.text());
        self.suggestions.clear();
    }

    /// Applies a prompt input. `paths` feeds Path suggestions.
    pub fn prompt_input(&mut self, m: PromptMsg, paths: &[String], recent: &[String]) {
        let Some(p) = &mut self.prompt else { return };
        self.prompt_error = None;
        match m {
            PromptMsg::Text(s) | PromptMsg::Choice(s) | PromptMsg::Suggest(s) => {
                let is_path = p.current().is_some_and(|s| s.kind == ParamKind::Path);
                if is_path {
                    self.suggestions = rank(
                        &s,
                        paths,
                        RankMode::Path,
                        recent,
                        crate::switcher::deranked,
                        SUGGESTIONS,
                    )
                    .into_iter()
                    .map(|r| paths[r.index].clone())
                    .collect();
                }
                p.set_text(s);
            }
            PromptMsg::Bool(b) => p.set_bool(b),
            PromptMsg::Editor(a) => {
                self.multiline.perform(a);
                p.set_text(self.multiline.text());
            }
            PromptMsg::Back => {
                p.back();
                let snapshot = p.clone();
                self.sync_step(&snapshot);
            }
        }
    }
}

/// Global keys: Esc closes/cancels, Up/Down move, Ctrl+Enter submits a multiline
/// prompt, any other Ctrl/Alt chord is offered to the action bindings.
pub fn key_event(ev: keyboard::Event) -> Option<Message> {
    let keyboard::Event::KeyPressed { key, modifiers, .. } = ev else {
        return None;
    };
    let p = |m| Some(Message::Palette(m));
    match key.as_ref() {
        Key::Named(Named::Escape) => p(PaletteMsg::Escape),
        Key::Named(Named::ArrowUp) => p(PaletteMsg::Up),
        Key::Named(Named::ArrowDown) => p(PaletteMsg::Down),
        Key::Named(Named::Enter) if modifiers.command() => p(PaletteMsg::CtrlEnter),
        Key::Named(Named::Enter) if !modifiers.alt() && !modifiers.shift() => p(PaletteMsg::Enter),
        // editor-usability-round WP-3: focused-pane back/forward (`chord` has no arrow names,
        // so these never reach action bindings).
        Key::Named(Named::ArrowLeft) if modifiers.command() && modifiers.alt() => {
            Some(Message::Panes(PanesMsg::BackFocused))
        }
        Key::Named(Named::ArrowRight) if modifiers.command() && modifiers.alt() => {
            Some(Message::Panes(PanesMsg::ForwardFocused))
        }
        // plan editor-round-4 WP-2: plain PageUp/PageDown (uncaptured: no editor or input
        // took it) pages the focused pane; Ctrl+PageUp/PageDown stay chords (go to top/bottom).
        Key::Named(Named::PageUp | Named::PageDown)
            if !modifiers.command() && !modifiers.alt() && !modifiers.logo() =>
        {
            Some(Message::Panes(PanesMsg::Page(matches!(
                key.as_ref(),
                Key::Named(Named::PageDown)
            ))))
        }
        // Ctrl+P / Ctrl+Shift+P and the other client chords resolve in `on_chord` against the
        // effective `Keymap`.
        _ if modifiers.command() || modifiers.alt() => {
            chord::from_event(&key, modifiers).map(|c| Message::Palette(PaletteMsg::Key(c)))
        }
        _ => None,
    }
}

/// plan editor-round-4 WP-1: an Escape press a widget captured (iced's `text_input` takes
/// Escape to unfocus itself, and `keyboard::listen` only yields ignored events). Only
/// captured presses map, so an ignored press still goes through [`key_event`] alone — one
/// press, one stage.
pub fn captured_escape(
    ev: iced::Event,
    status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    is_captured_escape(&ev, status).then_some(Message::Palette(PaletteMsg::Escape))
}

/// Whether `ev` is an Escape key press with `Captured` status.
pub fn is_captured_escape(ev: &iced::Event, status: iced::event::Status) -> bool {
    status == iced::event::Status::Captured
        && matches!(
            ev,
            iced::Event::Keyboard(keyboard::Event::KeyPressed { key, .. })
                if key.as_ref() == Key::Named(Named::Escape)
        )
}

/// The palette/panels subscription: the key listener, the toast expiry tick while a toast is
/// up, and the panels recompute tick while the column is visible.
pub fn subscription(palette: &Palette, panels_visible: bool) -> Subscription<Message> {
    let mut subs = vec![iced::keyboard::listen().filter_map(key_event)];
    if palette.open {
        subs.push(iced::event::listen_with(captured_escape));
    }
    if palette.jobs.toast.is_some() {
        subs.push(
            iced::time::every(Duration::from_secs(1)).map(|_| Message::Palette(PaletteMsg::Tick)),
        );
    }
    if panels_visible {
        subs.push(
            iced::time::every(Duration::from_millis(300)).map(|_| Message::Panels(PanelsMsg::Tick)),
        );
    }
    Subscription::batch(subs)
}
