//! Find in note (editor-usability-round WP-4): the per-pane find bar (Ctrl+F). Match logic is
//! `editor_model::find`; this module holds the bar's state, its view and the key listener.
//!
//! Source/Split: the current match becomes the buffer selection (the shaped `text_editor`
//! scrolls to its cursor) and is painted by the highlighter, since the unfocused editor draws no
//! selection (plan editor-feedback-round-2 WP-4). Reading: render-model blocks carry only
//! block-granular source line ranges (`RenderModel::block_lines`, plan editor-ui-round-3 WP-5),
//! not inline spans, so the query's occurrences are painted in the rich text and the scrollable
//! snaps to the match's relative line position (`line / lines`).

use std::ops::Range;

use iced::widget::pane_grid::Pane;
use iced::widget::{Space, row, text, text_input};
use iced::{Element, Length, Subscription};

use crate::icons::Icon;
use crate::message::{FindMsg, Message};
use crate::theme::{Tokens, icon_button};

/// One pane's find bar.
#[derive(Debug, Clone, Default)]
pub struct FindBar {
    pub query: String,
    /// Byte ranges into the buffer text at the last search.
    pub matches: Vec<Range<usize>>,
    pub current: Option<usize>,
    /// plan editor-round-4 WP-3: in Reading mode, the visible occurrences (top-level block, nth
    /// in it) the bar steps through instead of `matches` (`None` = Source/Split, or no render).
    pub visible: Option<Vec<(usize, usize)>>,
}

impl FindBar {
    /// Recomputes matches over `text`, keeping the current index when still valid.
    pub fn search(&mut self, text: &str) {
        self.search_in(text, None);
    }

    /// [`Self::search`], plus — when `reading` is a rendered Reading-mode model — the visible
    /// occurrences the bar then counts and steps through (plan editor-round-4 WP-3).
    pub fn search_in(&mut self, text: &str, reading: Option<&editor_model::render::RenderModel>) {
        self.matches = editor_model::find::matches(text, &self.query);
        self.visible = reading.map(|m| crate::reading::visible_occurrences(m, &self.query));
        let n = self.len();
        self.current = self.current.filter(|&i| i < n);
    }

    /// The number of matches the bar steps through.
    pub fn len(&self) -> usize {
        self.visible.as_ref().map_or(self.matches.len(), Vec::len)
    }

    /// No match to step through.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The current visible occurrence (Reading mode).
    pub fn occurrence(&self) -> Option<(usize, usize)> {
        self.current
            .and_then(|i| self.visible.as_ref()?.get(i).copied())
    }

    /// The "n/m" label.
    pub fn count(&self) -> String {
        match (self.current, self.len()) {
            (_, 0) if self.query.is_empty() => String::new(),
            (_, 0) => "0/0".into(),
            (Some(i), m) => format!("{}/{m}", i + 1),
            (None, m) => format!("0/{m}"),
        }
    }

    /// The current match range.
    pub fn range(&self) -> Option<Range<usize>> {
        if self.visible.is_some() {
            return None;
        }
        self.current.and_then(|i| self.matches.get(i).cloned())
    }
}

/// The find input's widget id for `pane`.
pub fn input_id(pane: Pane) -> iced::widget::Id {
    iced::widget::Id::from(format!("find-input:{pane:?}"))
}

/// The reading scrollable's widget id for `path` in tab `view` — plan round-5 WP-1: per tab,
/// not per path, so paging / probing one pane never also scrolls another pane on the note.
pub fn reading_scroll_id(path: &str, view: u64) -> iced::widget::Id {
    iced::widget::Id::from(format!("reading-scroll:{view}:{path}"))
}

/// The bar (a `Space` slot when closed, so the pane body keeps its tree position).
pub fn view<'a>(pane: Pane, bar: Option<&'a FindBar>, t: Tokens) -> Element<'a, Message> {
    let Some(b) = bar else {
        return Space::new().height(0).into();
    };
    let fm = |m| Message::Find(m);
    // Any press on the bar (input, count, chevrons) makes the input the sole focused widget, so
    // its keys never also reach a still-focused source editor (bug fix editor-find-return-leak).
    crate::widget::press_notify::press_notify(
        row![
            text_input("Find in note…", &b.query)
                .id(input_id(pane))
                .on_input(move |q| fm(FindMsg::Query(q)))
                .on_submit(fm(FindMsg::Submit))
                .width(Length::Fill),
            text(b.count()).size(13).color(t.text_dim.to_color()),
            icon_button(Icon::ChevronUp, Some(fm(FindMsg::Prev)), t),
            icon_button(Icon::ChevronDown, Some(fm(FindMsg::Next)), t),
            icon_button(Icon::X, Some(fm(FindMsg::Close)), t),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
        fm(FindMsg::Focus(pane)),
    )
    .into()
}

/// While a bar is open: Escape and Shift state, captured events included (the focused input
/// captures Escape and Enter).
pub fn subscription(open: bool) -> Subscription<Message> {
    if open {
        iced::event::listen_with(key)
    } else {
        Subscription::none()
    }
}

fn key(e: iced::Event, _: iced::event::Status, _: iced::window::Id) -> Option<Message> {
    use iced::keyboard::{self, Key, key::Named};
    match e {
        iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: Key::Named(Named::Escape),
            ..
        }) => Some(Message::Find(FindMsg::Close)),
        iced::Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => Some(Message::Modifiers(m)),
        _ => None,
    }
}
