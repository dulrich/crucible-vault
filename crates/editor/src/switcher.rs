//! Quick switcher (WP-4, Ctrl+P).
//!
//! Ranking is `editor_model::rank` (nucleo fuzzy score + recency from `ui.recent` + a derank for
//! `_resources/` and skipped folders); top [`LIMIT`]. Enter opens the selection; Shift+Enter, or
//! Enter / "Create <query>" on a miss, runs `note.create {path: "<query>.md"}` and opens the
//! returned `open_path`.

use cv_protocol::{ActionContext, RunActionParams};
use editor_model::rank::{RankMode, rank};
use iced::keyboard::{self, Key};
use iced::widget::{button, column, text, text_input};
use iced::{Element, Length};

use crate::message::{Message, SwitcherMsg};
use crate::theme::Tokens;

/// Result cap.
pub const LIMIT: usize = 50;

/// The query field's widget id.
pub const INPUT_ID: &str = "switcher-input";

/// The switcher model.
#[derive(Debug, Default)]
pub struct Switcher {
    pub open: bool,
    pub query: String,
    pub results: Vec<String>,
    pub selected: usize,
    /// WP-6 (usability-sprint-captures-links): folder mode — the note being moved; results rank
    /// [`Self::folders`] instead of note paths, and a pick moves the note there.
    pub move_from: Option<String>,
    /// Folder mode candidates ([`folder_candidates`]).
    pub folders: Vec<String>,
}

/// How the vault root shows (and ranks) in folder mode.
pub const ROOT_LABEL: &str = "/";

/// The parent folder of a vault path (`""` at the root).
pub fn parent_folder(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// WP-6: "Move note…" destinations — every distinct folder holding (or above) an indexed path,
/// plus the vault root as [`ROOT_LABEL`], minus `current`'s own folder; sorted.
pub fn folder_candidates(paths: &[String], current: &str) -> Vec<String> {
    let mut set = std::collections::BTreeSet::new();
    for p in paths {
        let mut d = parent_folder(p);
        while !d.is_empty() && set.insert(d.to_owned()) {
            d = parent_folder(d);
        }
    }
    let here = parent_folder(current);
    let mut v: Vec<String> = Vec::with_capacity(set.len() + 1);
    if !here.is_empty() {
        v.push(ROOT_LABEL.to_owned());
    }
    v.extend(set.into_iter().filter(|d| d != here));
    v
}

/// WP-6: the rename target for moving `from` into the picked folder label.
pub fn move_target(from: &str, folder: &str) -> String {
    let base = from.rsplit_once('/').map_or(from, |(_, b)| b);
    let folder = folder.trim_matches('/');
    if folder.is_empty() {
        base.to_owned()
    } else {
        format!("{folder}/{base}")
    }
}

/// Paths the switcher ranks below equal matches elsewhere.
pub fn deranked(path: &str) -> bool {
    cv_index::is_target_only_path(path) || cv_index::is_skipped_path(path)
}

/// The note path `note.create` gets for a query.
pub fn create_path(query: &str) -> Option<String> {
    let q = query.trim().trim_matches('/');
    if q.is_empty() {
        return None;
    }
    Some(if q.ends_with(".md") {
        q.to_owned()
    } else {
        format!("{q}.md")
    })
}

/// The `note.create` call for `path`.
pub fn create_params(path: &str) -> RunActionParams {
    RunActionParams {
        action_id: "note.create".into(),
        params: serde_json::Map::from_iter([(
            "path".to_owned(),
            serde_json::Value::String(path.to_owned()),
        )]),
        context: ActionContext::default(),
    }
}

impl Switcher {
    /// Re-ranks against `paths`.
    pub fn refresh(&mut self, paths: &[String], recent: &[String]) {
        let (list, recent): (&[String], &[String]) = if self.move_from.is_some() {
            (&self.folders, &[])
        } else {
            (paths, recent)
        };
        let derank = self.move_from.is_none();
        self.results = rank(
            &self.query,
            list,
            RankMode::Path,
            recent,
            |p| derank && deranked(p),
            LIMIT,
        )
        .into_iter()
        .map(|r| list[r.index].clone())
        .collect();
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
    }

    /// The highlighted result.
    pub fn current(&self) -> Option<&str> {
        self.results.get(self.selected).map(String::as_str)
    }

    /// Whether the query names an existing result exactly (no "Create" offer then).
    pub fn exact_hit(&self) -> bool {
        create_path(&self.query).is_some_and(|p| self.results.contains(&p))
    }
}

/// Global keys (Ctrl+P opens via the palette chord path): Escape clears the query then closes (WP-5), modifier changes feed Shift+Enter.
pub fn key_event(ev: keyboard::Event) -> Option<Message> {
    match ev {
        keyboard::Event::KeyPressed { key, .. } => match key.as_ref() {
            Key::Named(keyboard::key::Named::Escape) => {
                Some(Message::Switcher(SwitcherMsg::Escape))
            }
            Key::Named(keyboard::key::Named::ArrowDown) => {
                Some(Message::Switcher(SwitcherMsg::Down))
            }
            Key::Named(keyboard::key::Named::ArrowUp) => Some(Message::Switcher(SwitcherMsg::Up)),
            _ => None,
        },
        keyboard::Event::ModifiersChanged(m) => Some(Message::Modifiers(m)),
        keyboard::Event::KeyReleased { .. } => None,
    }
}

/// plan editor-round-4 WP-1: a widget-captured Escape (the focused query input) for the
/// open switcher; ignored presses reach it through [`key_event`] only.
pub fn captured_escape(
    ev: iced::Event,
    status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    crate::palette::is_captured_escape(&ev, status)
        .then_some(Message::Switcher(SwitcherMsg::Escape))
}

/// The switcher panel (shown above the panes while open).
pub fn view<'a>(s: &'a Switcher, tokens: Tokens) -> Element<'a, Message> {
    let hint = if s.move_from.is_some() {
        "Move note to folder…"
    } else {
        "Find or create a note…"
    };
    let mut col = column![
        text_input(hint, &s.query)
            .id(INPUT_ID)
            .on_input(|q| Message::Switcher(SwitcherMsg::Query(q)))
            .on_submit(Message::Switcher(SwitcherMsg::Submit))
    ]
    .spacing(2)
    .width(Length::Fill);
    for (i, r) in s.results.iter().take(12).enumerate() {
        let mut t = text(r.as_str()).size(14);
        if i == s.selected {
            t = t.color(tokens.accent_text.to_color());
        }
        col = col.push(
            button(t)
                .style(button::text)
                .width(Length::Fill)
                .on_press(Message::Switcher(SwitcherMsg::Pick(r.clone()))),
        );
    }
    if let Some(p) = create_path(&s.query).filter(|_| s.move_from.is_none() && !s.exact_hit()) {
        let label = format!("Create {p}");
        col = col.push(
            button(text(label).size(14))
                .style(button::text)
                .on_press(Message::Switcher(SwitcherMsg::Create)),
        );
    }
    col.into()
}
