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
use std::sync::Arc;

use editor_model::visible::{self, Occurrence, VisRun};

use iced::widget::pane_grid::Pane;
use iced::widget::{Space, row, text, text_input};
use iced::{Element, Length, Subscription};

use crate::icons::Icon;
use crate::message::{FindMsg, Message};
use crate::source::FindSpan;
use crate::theme::{Tokens, icon_button};

/// One pane's find bar — the only home of find state (WP-R17 SP6): the Source highlighter and
/// the Reading view derive their find paint from the focused pane's bar at view time.
#[derive(Debug, Clone, Default)]
pub struct FindBar {
    pub query: String,
    /// Byte ranges into the buffer text at the last search.
    pub matches: Vec<Range<usize>>,
    /// `matches` as (line, byte column) spans, painted light by the Source highlighter.
    pub spans: Arc<[FindSpan]>,
    /// The buffer `edit_seq` of the last search: the Source side paints only while it holds (an
    /// edit hides the stale spans until the next search).
    pub seq: u64,
    pub current: Option<usize>,
    /// plan editor-round-4 WP-3: in Reading mode, the visible occurrences the bar steps through
    /// instead of `matches` (`None` = Source/Split, or no render).
    pub visible: Option<Vec<Occurrence>>,
    /// Source/Split: the occurrence index (over the rendered runs) of the current source match's
    /// Reading counterpart — drawn strong; `None` when it is invisible (audit E2).
    pub counterpart: Option<usize>,
}

impl FindBar {
    /// Recomputes matches over `text` (buffer `edit_seq` `seq`), keeping the current index when
    /// still valid; when `reading` holds a Reading-mode render's runs, also the visible
    /// occurrences the bar then counts and steps through (plan editor-round-4 WP-3).
    pub fn search_in(&mut self, text: &str, seq: u64, reading: Option<&[VisRun]>) {
        self.matches = editor_model::find::matches(text, &self.query);
        self.spans = line_cols(text, &self.matches).into();
        self.seq = seq;
        self.visible = reading.map(|r| visible::occurrences(r, &self.query));
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
    pub fn occurrence(&self) -> Option<&Occurrence> {
        self.visible.as_ref()?.get(self.current?)
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

    /// The Reading view's find paint: the query, and the strong occurrence (the current one in
    /// Reading mode, the source match's counterpart in Split). `None` for an empty query.
    pub fn reading_paint(&self) -> Option<ReadingPaint<'_>> {
        (!self.query.is_empty()).then(|| ReadingPaint {
            query: &self.query,
            active: if self.visible.is_some() {
                self.current
            } else {
                self.counterpart
            },
        })
    }

    /// The Source highlighter's find paint for a buffer at `edit_seq`: (current match, every
    /// match) — nothing once the buffer was edited after the last search.
    pub fn source_paint(&self, edit_seq: u64) -> (Option<FindSpan>, Arc<[FindSpan]>) {
        if self.seq != edit_seq {
            return (None, Arc::from([]));
        }
        let current = self
            .range()
            .and(self.current.and_then(|i| self.spans.get(i).copied()));
        (current, self.spans.clone())
    }
}

/// What the Reading view paints for find: every occurrence of `query` light, occurrence index
/// `active` (over [`visible::occurrences`]) strong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadingPaint<'a> {
    pub query: &'a str,
    pub active: Option<usize>,
}

/// Byte ranges → (line, byte column) spans, in one pass over `text` (`ranges` in order).
fn line_cols(text: &str, ranges: &[Range<usize>]) -> Vec<FindSpan> {
    let mut line = 0;
    let mut line_start = 0;
    let mut scanned = 0;
    let mut at = |byte: usize| {
        let byte = byte.min(text.len());
        if byte > scanned {
            for (i, _) in text[scanned..byte].match_indices('\n') {
                line += 1;
                line_start = scanned + i + 1;
            }
            scanned = byte;
        }
        (line, byte - line_start)
    };
    ranges.iter().map(|r| (at(r.start), at(r.end))).collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// WP-R17: the one-pass span mapping agrees with `editor_model::find::line_col`.
    #[test]
    fn line_cols_match_line_col() {
        let text = "ab foo\nfoo é foo\n\nfoo";
        let ms = editor_model::find::matches(text, "foo");
        let want: Vec<FindSpan> = ms
            .iter()
            .map(|m| {
                (
                    editor_model::find::line_col(text, m.start),
                    editor_model::find::line_col(text, m.end),
                )
            })
            .collect();
        assert_eq!(line_cols(text, &ms), want);
    }
}
