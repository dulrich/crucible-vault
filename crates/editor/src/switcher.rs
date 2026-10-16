//! Quick switcher (WP-4, Ctrl+P).
//!
//! Ranking is `editor_model::rank` (nucleo fuzzy score + recency from `ui.recent` + a derank for
//! `_resources/` and skipped folders); top [`LIMIT`]. Enter opens the selection; Shift+Enter, or
//! Enter / "Create <query>" on a miss, runs `note.create {path: "<query>.md"}` and opens the
//! returned `open_path`.

use cv_protocol::{ActionContext, Mode, RunActionParams};
use editor_model::rank::{Ranked, rank};
use iced::keyboard::{self, Key, Modifiers};
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
    pub results: Vec<Ranked>,
    pub selected: usize,
    pub shift: bool,
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
        self.results = rank(&self.query, paths, recent, deranked, LIMIT);
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
    }

    /// The highlighted result.
    pub fn current(&self) -> Option<&str> {
        self.results.get(self.selected).map(|r| r.path.as_str())
    }

    /// Whether the query names an existing result exactly (no "Create" offer then).
    pub fn exact_hit(&self) -> bool {
        create_path(&self.query).is_some_and(|p| self.results.iter().any(|r| r.path == p))
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
        keyboard::Event::ModifiersChanged(m) => Some(Message::Switcher(SwitcherMsg::Modifiers(m))),
        keyboard::Event::KeyReleased { .. } => None,
    }
}

/// Whether Shift is held in `m`.
pub fn shift(m: Modifiers) -> bool {
    m.shift()
}

/// The switcher panel (shown above the panes while open).
pub fn view<'a>(s: &'a Switcher, tokens: Tokens, mode: Mode) -> Element<'a, Message> {
    let mut col = column![
        text_input("Find or create a note…", &s.query)
            .id(INPUT_ID)
            .on_input(|q| Message::Switcher(SwitcherMsg::Query(q)))
            .on_submit(Message::Switcher(SwitcherMsg::Submit))
    ]
    .spacing(2)
    .width(Length::Fill);
    for (i, r) in s.results.iter().take(12).enumerate() {
        let mut t = text(r.path.as_str()).size(14);
        if i == s.selected {
            t = t.color(tokens.accent_text.to_color());
        }
        col = col.push(
            button(t)
                .style(button::text)
                .width(Length::Fill)
                .on_press(Message::Switcher(SwitcherMsg::Pick(r.path.clone()))),
        );
    }
    if let Some(p) = create_path(&s.query).filter(|_| !s.exact_hit()) {
        let label = format!("Create {p}");
        col = col.push(if mode == Mode::Shadow {
            column![
                button(text(label).size(14)).style(button::text),
                text("creating notes is disabled in shadow mode")
                    .size(12)
                    .color(tokens.warn_text.to_color()),
            ]
            .into()
        } else {
            Element::from(
                button(text(label).size(14))
                    .style(button::text)
                    .on_press(Message::Switcher(SwitcherMsg::Create)),
            )
        });
    }
    col.into()
}
