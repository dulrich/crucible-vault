//! Command palette (WP-5, Ctrl+P): daemon actions merged with client commands, typed parameter
//! prompts, action invocation with context (save-first), key chords and the job toast.
//!
//! Entries: [`ClientCmd`]s first, then `actions_list` (fetched off the UI thread on first open or
//! first chord, cached until invalidated — config reload or a change under
//! [`CONFIG_PREFIX`]). Ranking is `editor_model::rank` over `"title group"`. Invocation of an
//! action: prompt each `ParamSpec` ([`prompt::Prompt`]), then — if the active buffer is dirty and
//! the daemon is live — save it and dispatch `run_action` only on a successful save with the new
//! hash (the app's `run_call`/`after_save_first`).

pub mod chord;
pub mod prompt;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use cv_protocol::{ActionContext, ActionInfo, JobState, JobStatus, ParamKind};
use editor_model::rank::rank;
use iced::keyboard::{self, Key, key::Named};
use iced::widget::{button, column, pick_list, row, text, text_editor, text_input, toggler};
use iced::{Element, Length, Subscription};
use serde_json::{Map, Value};

use crate::app::Tab;
use crate::message::{
    Message, PaletteMsg, PanelsMsg, PanesMsg, ReadingMsg, SourceMsg, SwitcherMsg, ThemeMsg,
};
use crate::panels::PanelKind;
use crate::theme::Tokens;
use chord::Chord;
use prompt::{ParamValue, Prompt};

/// The query / prompt field's widget id.
pub const INPUT_ID: &str = "palette-input";
/// Result rows shown.
pub const SHOWN: usize = 14;
/// A terminal job's toast stays this long.
pub const TOAST_TTL: Duration = Duration::from_secs(5);
/// Changes under this prefix invalidate the cached action list (definitions are hot-reloaded).
pub const CONFIG_PREFIX: &str = "_custom/crucible-vault/";
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

/// A client-side command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientCmd {
    CycleReading,
    Surround,
    SplitRight,
    SplitDown,
    ClosePane,
    QuickSwitcher,
    Save,
    Undo,
    Redo,
    TogglePanels,
    Panel(PanelKind),
}

impl ClientCmd {
    /// Every client command, in palette order.
    pub fn all() -> Vec<ClientCmd> {
        let mut v = vec![
            ClientCmd::CycleReading,
            ClientCmd::Surround,
            ClientCmd::SplitRight,
            ClientCmd::SplitDown,
            ClientCmd::ClosePane,
            ClientCmd::QuickSwitcher,
            ClientCmd::Save,
            ClientCmd::Undo,
            ClientCmd::Redo,
            ClientCmd::TogglePanels,
        ];
        v.extend(PanelKind::ALL.iter().map(|&k| ClientCmd::Panel(k)));
        v
    }

    /// Title and group.
    pub fn title(self) -> (String, &'static str) {
        let t = match self {
            ClientCmd::CycleReading => "Toggle reading / split view",
            ClientCmd::Surround => "Cycle surround (theme)",
            ClientCmd::SplitRight => "Split pane right",
            ClientCmd::SplitDown => "Split pane down",
            ClientCmd::ClosePane => "Close pane",
            ClientCmd::QuickSwitcher => "Quick switcher",
            ClientCmd::Save => "Save note",
            ClientCmd::Undo => "Undo",
            ClientCmd::Redo => "Redo",
            ClientCmd::TogglePanels => "Toggle side panels",
            ClientCmd::Panel(k) => return (format!("Show {} panel", k.label()), "Panels"),
        };
        (t.to_owned(), "Editor")
    }

    /// The editor's default chord, if any.
    pub fn keys(self) -> Vec<String> {
        let k = match self {
            ClientCmd::CycleReading => "Ctrl+E",
            ClientCmd::QuickSwitcher => "Ctrl+O",
            ClientCmd::Save => "Ctrl+S",
            ClientCmd::Undo => "Ctrl+Z",
            ClientCmd::Redo => "Ctrl+Shift+Z",
            _ => return Vec::new(),
        };
        vec![k.to_owned()]
    }

    /// The message it sends (`focus` = the focused pane for "close pane").
    pub fn message(self, focus: iced::widget::pane_grid::Pane) -> Message {
        match self {
            ClientCmd::CycleReading => Message::Reading(ReadingMsg::CycleMode),
            ClientCmd::Surround => Message::Theme(ThemeMsg::Cycle),
            ClientCmd::SplitRight => Message::Panes(PanesMsg::SplitRight),
            ClientCmd::SplitDown => Message::Panes(PanesMsg::SplitDown),
            ClientCmd::ClosePane => Message::Panes(PanesMsg::Close(focus)),
            ClientCmd::QuickSwitcher => Message::Switcher(SwitcherMsg::Open),
            ClientCmd::Save => Message::Source(SourceMsg::Save),
            ClientCmd::Undo => Message::Source(SourceMsg::Undo),
            ClientCmd::Redo => Message::Source(SourceMsg::Redo),
            ClientCmd::TogglePanels => Message::Panels(PanelsMsg::Toggle),
            ClientCmd::Panel(k) => Message::Panels(PanelsMsg::Show(k)),
        }
    }
}

/// What an entry runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryId {
    Client(ClientCmd),
    Action(String),
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
    pub needs: Vec<String>,
}

/// The job toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub title: String,
    pub state: JobState,
    /// When the job reached a terminal state (expiry clock).
    pub ended: Option<Instant>,
}

impl Toast {
    /// The status-bar text.
    pub fn line(&self) -> String {
        let s = serde_json::to_value(self.state)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        format!("{}: {s}", self.title)
    }
}

/// Whether a job state is final.
pub fn terminal(s: JobState) -> bool {
    matches!(
        s,
        JobState::Done | JobState::Failed | JobState::Cancelled | JobState::Deferred
    )
}

/// Client commands then actions.
pub fn build_entries(actions: &[ActionInfo]) -> Vec<Entry> {
    let mut v: Vec<Entry> = ClientCmd::all()
        .into_iter()
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
    v.extend(actions.iter().map(|a| Entry {
        id: EntryId::Action(a.id.clone()),
        title: a.title.clone(),
        group: a.group.clone().unwrap_or_else(|| "Actions".into()),
        keys: a.keys.clone(),
    }));
    v
}

/// Ranks `entries` against `query` (fuzzy over `"title group"`), best first, as indices.
pub fn rank_entries(query: &str, entries: &[Entry]) -> Vec<usize> {
    // `rank` scores path-like strings; '/' would split a "basename", so it is neutralized, and a
    // per-entry suffix keeps haystacks unique.
    let mut by_hay: HashMap<String, usize> = HashMap::new();
    let hays: Vec<String> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let mut h = format!("{} {}", e.title, e.group).replace('/', " ");
            if by_hay.contains_key(&h) {
                h.push_str(&" ".repeat(i + 1));
            }
            by_hay.insert(h.clone(), i);
            h
        })
        .collect();
    if query.trim().is_empty() {
        return (0..entries.len()).collect();
    }
    rank(query, &hays, &[], |_| false, entries.len())
        .into_iter()
        .filter_map(|r| by_hay.get(&r.path).copied())
        .collect()
}

/// Chord → action id for every parseable, non-conflicting `ActionInfo.keys`, plus a notice per
/// conflict (editor defaults win; between actions the first registered wins).
pub fn bindings(actions: &[ActionInfo]) -> (HashMap<Chord, String>, Vec<String>) {
    let mut map: HashMap<Chord, String> = HashMap::new();
    let mut notes = Vec::new();
    for a in actions {
        for k in &a.keys {
            let Some(c) = chord::parse(k) else {
                notes.push(format!("{}: unreadable key \"{k}\"", a.id));
                continue;
            };
            if chord::reserved(&c) {
                notes.push(format!("{}: {c} is an editor default (not bound)", a.id));
            } else if let Some(other) = map.get(&c) {
                notes.push(format!("{}: {c} already bound to {other}", a.id));
            } else {
                map.insert(c, a.id.clone());
            }
        }
    }
    (map, notes)
}

/// The invocation context for the active tab: path, 1-based cursor line and buffer hash always;
/// the selection when non-empty.
pub fn context(tab: Option<&Tab>) -> ActionContext {
    let Some(tab) = tab else {
        return ActionContext::default();
    };
    let line = u32::try_from(tab.source.cursor().position.line + 1).ok();
    ActionContext {
        active_path: Some(tab.path.clone()),
        selection: tab.source.selection().filter(|s| !s.is_empty()),
        value: None,
        line,
        buffer_hash: Some(tab.source.base_hash().as_str().to_owned()),
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
    pub bindings: HashMap<Chord, String>,
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
    pub toast: Option<Toast>,
    /// job id → title of the action that started it.
    pub job_titles: HashMap<String, String>,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            entries: build_entries(&[]),
            actions: None,
            loading: false,
            bindings: HashMap::new(),
            notice: Vec::new(),
            prompt: None,
            prompt_error: None,
            multiline: text_editor::Content::new(),
            suggestions: Vec::new(),
            pending_chord: None,
            toast: None,
            job_titles: HashMap::new(),
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
        let (map, notes) = bindings(&actions);
        self.bindings = map;
        self.notice = notes;
        self.entries = build_entries(&actions);
        self.actions = Some(actions);
        self.loading = false;
        self.refresh();
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
                    self.suggestions =
                        rank(&s, paths, recent, crate::switcher::deranked, SUGGESTIONS)
                            .into_iter()
                            .map(|r| r.path)
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

    /// A `job_updated`: refreshes the toast (title from the invoking action when known).
    pub fn job_updated(&mut self, j: &JobStatus, now: Instant) {
        let title = self
            .job_titles
            .get(&j.job_id)
            .cloned()
            .unwrap_or_else(|| j.job_id.clone());
        self.toast = Some(Toast {
            title,
            state: j.status,
            ended: terminal(j.status).then_some(now),
        });
    }

    /// Clears a terminal toast older than [`TOAST_TTL`].
    pub fn expire_toast(&mut self, now: Instant) {
        if self
            .toast
            .as_ref()
            .and_then(|t| t.ended)
            .is_some_and(|e| now.duration_since(e) >= TOAST_TTL)
        {
            self.toast = None;
        }
    }
}

/// Global keys: Ctrl+P opens, Esc closes/cancels, Up/Down move, Ctrl+Enter submits a multiline
/// prompt, any other Ctrl/Alt chord is offered to the action bindings.
pub fn key_event(ev: keyboard::Event) -> Option<Message> {
    let keyboard::Event::KeyPressed { key, modifiers, .. } = ev else {
        return None;
    };
    let p = |m| Some(Message::Palette(m));
    match key.as_ref() {
        Key::Named(Named::Escape) => p(PaletteMsg::Close),
        Key::Named(Named::ArrowUp) => p(PaletteMsg::Up),
        Key::Named(Named::ArrowDown) => p(PaletteMsg::Down),
        Key::Named(Named::Enter) if modifiers.command() => p(PaletteMsg::CtrlEnter),
        Key::Character(c)
            if modifiers.command()
                && !modifiers.shift()
                && !modifiers.alt()
                && c.eq_ignore_ascii_case("p") =>
        {
            p(PaletteMsg::Open)
        }
        _ if modifiers.command() || modifiers.alt() => {
            chord::from_event(&key, modifiers).map(|c| Message::Palette(PaletteMsg::Key(c)))
        }
        _ => None,
    }
}

/// The palette/panels subscription: the key listener, the toast expiry tick while a toast is
/// up, and the panels recompute tick while the column is visible.
pub fn subscription(palette: &Palette, panels_visible: bool) -> Subscription<Message> {
    let mut subs = vec![iced::keyboard::listen().filter_map(key_event)];
    if palette.toast.is_some() {
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

fn prompt_view<'a>(p: &'a Palette, pr: &'a Prompt, t: Tokens) -> Element<'a, Message> {
    let pm = |m| Message::Palette(PaletteMsg::Prompt(m));
    let Some(spec) = pr.current() else {
        return text("").into();
    };
    let header = text(format!(
        "{} — {} ({}/{}){}",
        pr.action.title,
        spec.label,
        pr.step + 1,
        pr.action.params.len(),
        if spec.required { " *" } else { "" }
    ))
    .size(14);
    let input: Element<'a, Message> = match spec.kind {
        ParamKind::Bool => {
            let on = matches!(pr.value(), Some(ParamValue::Bool(true)));
            toggler(on)
                .label(spec.label.as_str())
                .on_toggle(move |b| pm(PromptMsg::Bool(b)))
                .into()
        }
        ParamKind::Enum => pick_list(
            spec.choices.as_slice(),
            Some(pr.text().to_owned()).filter(|s| !s.is_empty()),
            move |c: String| pm(PromptMsg::Choice(c)),
        )
        .into(),
        ParamKind::Multiline => text_editor(&p.multiline)
            .height(120)
            .on_action(move |a| pm(PromptMsg::Editor(a)))
            .into(),
        ParamKind::Text | ParamKind::Path | ParamKind::Date => {
            let ph = if spec.kind == ParamKind::Date {
                "YYYY-MM-DD"
            } else {
                ""
            };
            text_input(ph, pr.text())
                .id(INPUT_ID)
                .on_input(move |s| pm(PromptMsg::Text(s)))
                .on_submit(Message::Palette(PaletteMsg::Submit))
                .into()
        }
    };
    let mut col = column![header, input].spacing(4).width(Length::Fill);
    for s in &p.suggestions {
        col = col.push(
            button(text(s.as_str()).size(13))
                .style(button::text)
                .on_press(pm(PromptMsg::Suggest(s.clone()))),
        );
    }
    if let Some(e) = &p.prompt_error {
        col = col.push(text(e.as_str()).size(12).color(t.warn_text.to_color()));
    }
    let hint = if spec.kind == ParamKind::Multiline {
        "Ctrl+Enter: next · Esc: cancel"
    } else {
        "Enter: next · Esc: cancel"
    };
    col.push(
        row![
            button(text("Back").size(13))
                .style(button::text)
                .on_press(pm(PromptMsg::Back)),
            button(text("Next").size(13))
                .style(button::text)
                .on_press(Message::Palette(PaletteMsg::Submit)),
            text(hint).size(12).color(t.text_faint.to_color()),
        ]
        .spacing(8),
    )
    .into()
}

/// The palette overlay (shown above the panes while open).
pub fn view<'a>(p: &'a Palette, t: Tokens) -> Element<'a, Message> {
    if let Some(pr) = &p.prompt {
        return prompt_view(p, pr, t);
    }
    let mut col = column![
        text_input("Run a command…", &p.query)
            .id(INPUT_ID)
            .on_input(|q| Message::Palette(PaletteMsg::Query(q)))
            .on_submit(Message::Palette(PaletteMsg::Submit))
    ]
    .spacing(2)
    .width(Length::Fill);
    if p.loading {
        col = col.push(
            text("loading actions…")
                .size(12)
                .color(t.text_faint.to_color()),
        );
    }
    for (row_i, &i) in p.results.iter().take(SHOWN).enumerate() {
        let Some(e) = p.entries.get(i) else { continue };
        let mut title = text(e.title.as_str()).size(14);
        if row_i == p.selected {
            title = title.color(t.accent_text.to_color());
        }
        col = col.push(
            button(
                row![
                    title.width(Length::Fill),
                    text(e.group.as_str())
                        .size(12)
                        .color(t.text_faint.to_color()),
                    text(e.keys.join(" ")).size(12).color(t.text_dim.to_color()),
                ]
                .spacing(12),
            )
            .style(button::text)
            .width(Length::Fill)
            .on_press(Message::Palette(PaletteMsg::Pick(i))),
        );
    }
    for n in &p.notice {
        col = col.push(text(n.as_str()).size(12).color(t.warn_text.to_color()));
    }
    col.into()
}
