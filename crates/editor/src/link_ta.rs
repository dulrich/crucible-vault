//! Plan round-5-reading-pickers-intake WP-9: the `[[` note typeahead in the Source editor.
//!
//! One [`LinkTa`] lives on the app, tied to the focused pane's active tab. Typing the second `[`
//! of `[[` opens it; the query is the line text between `[[` and the caret. Candidates are the
//! Quick Switch ranking (`editor_model::rank` over `explorer.paths` + `ui.recent`, as
//! `crate::switcher::Switcher::refresh`). While open, `crate::source` `key_binding` routes
//! Up/Down/Enter/Tab/Esc here as [`crate::message::LinkKey`]s (they never reach the buffer).
//!
//! Anchor estimate (iced exposes no pixel caret): row = caret line − the buffer's tracked top
//! line, height = 1.3 × font size (iced's default relative line height, as
//! `crate::source::viewport_lines`); x ≈ 0.6 × font size per character of the line before `[[`,
//! capped at [`MAX_X`]. The list goes below the caret line, flipped above it when it would leave
//! the measured viewport — it never covers the typed line.

use iced::widget::{Space, column, container, row};
use iced::{Element, Length, Theme};

use crate::message::{LinkKey, Message, SourceMsg};
use crate::theme::Tokens;
use crate::widget::typeahead::{self, Candidate, Status};

/// The list card width (px).
const WIDTH: f32 = 340.0;
/// The left offset cap (px), keeping the card inside narrow panes.
const MAX_X: f32 = 240.0;
/// Rendered height per row (px; 13 px label + padding).
const ROW_H: f32 = 22.0;

/// The open `[[` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTa {
    /// The tab (note path + tab id) whose source editor it belongs to.
    pub path: String,
    pub tab: u64,
    /// The source line of the `[[`.
    pub line: usize,
    /// The byte column right after `[[`.
    pub start: usize,
    /// The query (text between `[[` and the caret).
    pub query: String,
    /// The ranked candidates shown.
    pub visible: Vec<Candidate>,
    pub highlight: usize,
}

/// What the caret context says after an edit / motion.
enum Sync {
    Open { start: usize, query: String },
    Keep { query: String },
    Close,
}

impl crate::app::App {
    /// Re-evaluates the list after a source action / key; `typed_bracket` = the action inserted
    /// `[` (the only way the list opens).
    pub(crate) fn link_ta_sync(&mut self, typed_bracket: bool) {
        let Some(tab) = self.active_note().map(|t| (t.id, t.path.to_owned())) else {
            self.link_ta = None;
            return;
        };
        let Some(doc) = self.doc(&tab.1) else {
            self.link_ta = None;
            return;
        };
        let cur = doc.cursor();
        let (line, col) = (cur.position.line, cur.position.column);
        let text = if cur.selection.is_none() {
            doc.line_text(line)
        } else {
            None
        };
        let sync = match (text, &self.link_ta) {
            (None, _) => Sync::Close,
            (Some(t), ta) => {
                let before = t.get(..col).unwrap_or("");
                let same = ta
                    .as_ref()
                    .is_some_and(|a| a.path == tab.1 && a.tab == tab.0 && a.line == line);
                if typed_bracket && before.ends_with("[[") && !before.ends_with("[[[") {
                    Sync::Open {
                        start: col,
                        query: String::new(),
                    }
                } else if let Some(a) = ta.as_ref().filter(|_| same) {
                    match before.get(a.start..) {
                        Some(q)
                            if a.start >= 2
                                && before.get(a.start - 2..a.start) == Some("[[")
                                && !q.contains("]]") =>
                        {
                            Sync::Keep {
                                query: q.to_owned(),
                            }
                        }
                        _ => Sync::Close,
                    }
                } else {
                    Sync::Close
                }
            }
        };
        match sync {
            Sync::Close => self.link_ta = None,
            Sync::Open { start, query } => {
                self.link_ta = Some(LinkTa {
                    path: tab.1,
                    tab: tab.0,
                    line,
                    start,
                    query,
                    visible: Vec::new(),
                    highlight: 0,
                });
                self.link_ta_rank();
            }
            Sync::Keep { query } => {
                if let Some(a) = self.link_ta.as_mut()
                    && a.query != query
                {
                    a.query = query;
                    a.highlight = 0;
                    self.link_ta_rank();
                }
            }
        }
    }

    /// Re-ranks the open list (the Quick Switch ranking).
    fn link_ta_rank(&mut self) {
        let Some(a) = self.link_ta.as_mut() else {
            return;
        };
        a.visible = editor_model::rank::rank(
            &a.query,
            &self.explorer.paths,
            &self.ui.recent,
            crate::switcher::deranked,
            typeahead::LIMIT,
        )
        .into_iter()
        .map(|r| Candidate::plain(r.path))
        .collect();
        a.highlight = a.highlight.min(a.visible.len().saturating_sub(1));
    }

    /// Closes the list when its tab is no longer the focused editor (focus / tab / mode change).
    pub(crate) fn link_ta_validate(&mut self) {
        let ok = self.link_ta.as_ref().is_some_and(|a| {
            self.active_note()
                .is_some_and(|t| t.path == a.path && t.id == a.tab)
                && self.reading.mode(&a.path) != crate::reading::ViewMode::Reading
        });
        if !ok {
            self.link_ta = None;
        }
    }

    /// An intercepted key. Returns a follow-up message when the key falls through (Enter / Tab
    /// with zero candidates: the list closes and the key acts as usual).
    pub(crate) fn link_ta_key(&mut self, k: LinkKey) -> Option<Message> {
        let a = self.link_ta.as_mut()?;
        let n = a.visible.len();
        match k {
            LinkKey::Up => a.highlight = a.highlight.checked_sub(1).unwrap_or(n.saturating_sub(1)),
            LinkKey::Down => a.highlight = if n == 0 { 0 } else { (a.highlight + 1) % n },
            LinkKey::Escape => self.link_ta = None,
            LinkKey::Enter | LinkKey::Tab => {
                let Some(c) = a.visible.get(a.highlight).cloned() else {
                    self.link_ta = None;
                    let key = if k == LinkKey::Enter {
                        crate::message::SourceKey::Enter
                    } else {
                        crate::message::SourceKey::Indent
                    };
                    return Some(Message::Source(SourceMsg::Key(key)));
                };
                self.link_ta_accept(&c.value);
            }
        }
        None
    }

    /// Replaces `[[query` (plus a `]]` directly after the caret) with the link to `path`.
    pub(crate) fn link_ta_accept(&mut self, path: &str) {
        let Some(a) = self.link_ta.take() else {
            return;
        };
        let link = editor_model::link_form::wikilink(path, &self.explorer.paths);
        let Some(doc) = self.doc_mut(&a.path) else {
            return;
        };
        let Some(line) = doc.line_text(a.line) else {
            return;
        };
        let caret = a.start + a.query.len();
        let end = if line.get(caret..).is_some_and(|r| r.starts_with("]]")) {
            caret + 2
        } else {
            caret
        };
        let from = a.start - 2;
        if doc.replace_in_line(a.line, from..end, &link, std::time::Instant::now()) {
            doc.move_to(iced::widget::text_editor::Cursor {
                position: iced::widget::text_editor::Position {
                    line: a.line,
                    column: from + link.len(),
                },
                selection: None,
            });
        }
    }

    /// The positioned list overlay for tab `id` showing `path`, when open there.
    pub(crate) fn link_ta_overlay(&self, path: &str, id: u64) -> Option<Element<'_, Message>> {
        let a = self.link_ta.as_ref()?;
        if a.path != path || a.tab != id || self.active_note().is_none_or(|t| t.id != id) {
            return None;
        }
        let doc = self.doc(path)?;
        Some(overlay(
            a,
            doc.top_line(),
            doc.visible_lines(),
            self.prefs.font_size,
            self.tokens,
        ))
    }
}

/// The card, placed by the anchor estimate (module docs).
#[allow(clippy::cast_precision_loss)] // line / column counts are small
fn overlay<'a>(
    a: &LinkTa,
    top: usize,
    visible_lines: usize,
    font_size: f32,
    t: Tokens,
) -> Element<'a, Message> {
    let lh = (font_size * 1.3).max(1.0);
    let pad = crate::source::EDITOR_PADDING;
    let row_i = a
        .line
        .saturating_sub(top)
        .min(visible_lines.saturating_sub(1)) as f32;
    let rows = a.visible.len().max(1) as f32;
    let card_h = rows * ROW_H + 10.0;
    let below = pad + (row_i + 1.0) * lh;
    let pane_h = pad + visible_lines as f32 * lh;
    let y = if below + card_h <= pane_h || pad + row_i * lh < card_h {
        below
    } else {
        pad + row_i * lh - card_h
    };
    let x = (pad + a.start.saturating_sub(2) as f32 * font_size * 0.6).min(MAX_X);
    let body: Element<'a, Message> = if a.visible.is_empty() {
        iced::widget::text("No matching notes")
            .size(13)
            .color(t.text_faint.to_color())
            .into()
    } else {
        typeahead::list(
            &a.visible,
            a.highlight,
            &Status::Ready,
            |v| Message::Source(SourceMsg::LinkPick(v)),
            t,
        )
    };
    let card = container(body)
        .padding(4)
        .width(WIDTH)
        .style(move |_: &Theme| container::Style {
            background: Some(t.panel.to_color().into()),
            text_color: Some(t.text.to_color()),
            border: iced::Border {
                color: t.line.to_color(),
                width: 1.0,
                radius: crate::theme::RADIUS.into(),
            },
            ..container::Style::default()
        });
    column![
        Space::new().height(y.max(0.0)),
        row![Space::new().width(x), iced::widget::opaque(card)],
    ]
    .width(Length::Fill)
    .into()
}
