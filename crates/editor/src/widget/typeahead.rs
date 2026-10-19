//! A reusable typeahead (plan round-5-reading-pickers-intake WP-4, "Typeahead widget contract").
//!
//! Data-agnostic: a host feeds [`Candidate`]s (display label, secondary line, stored value) and
//! an optional [`Status`] line (WP-6's async lookups report loading / failure through it), keeps
//! one [`Typeahead`] per active field, and applies the [`Outcome`] its events return:
//!
//! - fuzzy ranking via `editor_model::rank` ([`filter`]);
//! - selection by Enter, click ([`Typeahead::pick`]), or Up/Down + Enter ([`Typeahead::key`]);
//! - the `x` button clears the committed value ([`Typeahead::clear`]);
//! - Escape closes the list and, when the typed text is not a valid committed value, restores
//!   the last committed value; blur behaves like Escape.
//!
//! Enter with no visible candidate commits the typed text as-is (an explicit commit — fields
//! whose daemon accepts free text, e.g. Ignore Shorts channel names or folder templates).

use iced::widget::{button, column, row, text};
use iced::{Alignment, Element, Length};

use crate::icons::Icon;
use crate::theme::{self, Tokens};

/// Rows the list shows at most.
pub const LIMIT: usize = 8;

/// One candidate: what is shown, and what is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The display label (ranked against the query).
    pub label: String,
    /// A faint second line (`""` = none).
    pub secondary: String,
    /// The value written when picked.
    pub value: String,
}

impl Candidate {
    /// A candidate whose label is its value.
    pub fn plain(v: impl Into<String>) -> Self {
        let v = v.into();
        Self {
            label: v.clone(),
            secondary: String::new(),
            value: v,
        }
    }
}

/// An optional status line under the list (async sources).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Ready,
    Loading,
    Failed(String),
}

/// A key routed to the active typeahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Enter,
    Escape,
}

/// What the host should do after an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to write.
    None,
    /// Write this value (it is now the committed value).
    Commit(String),
    /// Write back the last committed value (the typed text was not valid).
    Restore(String),
}

/// One field's typeahead state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Typeahead {
    /// The last committed value (restored by Escape / blur).
    pub committed: String,
    /// The highlighted row of the visible list.
    pub highlight: usize,
    /// The list is showing.
    pub open: bool,
}

impl Typeahead {
    /// Starts editing a field whose committed value is `committed`.
    pub fn begin(committed: impl Into<String>) -> Self {
        Self {
            committed: committed.into(),
            highlight: 0,
            open: false,
        }
    }

    /// The text changed: open the list at the top row.
    pub fn typed(&mut self) {
        self.open = true;
        self.highlight = 0;
    }

    /// A key on the field. `text` is the field's current text, `visible` the filtered list
    /// shown, `valid` whether `text` is a valid committed value (equals some candidate value).
    pub fn key(&mut self, key: Key, text: &str, visible: &[Candidate], valid: bool) -> Outcome {
        match key {
            Key::Up => {
                self.open = true;
                self.highlight = self.highlight.saturating_sub(1);
                Outcome::None
            }
            Key::Down => {
                if self.open {
                    self.highlight = (self.highlight + 1).min(visible.len().saturating_sub(1));
                }
                self.open = true;
                Outcome::None
            }
            Key::Enter => {
                let v = match visible.get(self.highlight).filter(|_| self.open) {
                    Some(c) => c.value.clone(),
                    None => text.to_owned(),
                };
                self.commit(v)
            }
            Key::Escape => self.blur(text, valid),
        }
    }

    /// Escape / focus loss: close; restore the committed value unless `text` is valid.
    pub fn blur(&mut self, text: &str, valid: bool) -> Outcome {
        self.open = false;
        self.highlight = 0;
        if text == self.committed {
            Outcome::None
        } else if valid {
            self.committed = text.to_owned();
            Outcome::None
        } else {
            Outcome::Restore(self.committed.clone())
        }
    }

    /// A candidate was clicked.
    pub fn pick(&mut self, value: impl Into<String>) -> Outcome {
        self.commit(value.into())
    }

    /// The `x` button: clear the committed value.
    pub fn clear(&mut self) -> Outcome {
        self.commit(String::new())
    }

    fn commit(&mut self, v: String) -> Outcome {
        self.open = false;
        self.highlight = 0;
        self.committed = v.clone();
        Outcome::Commit(v)
    }
}

/// `cands` ranked against `query` (fuzzy over the label, best first; empty query keeps the
/// given order), then the candidates whose stored value alone matches (a model shown by its
/// label still matches its id), at most `limit`.
pub fn filter(query: &str, cands: &[Candidate], limit: usize) -> Vec<Candidate> {
    let mut used = vec![false; cands.len()];
    let mut out = Vec::new();
    for key in [0, 1] {
        let keys: Vec<String> = cands
            .iter()
            .map(|c| if key == 0 { &c.label } else { &c.value }.clone())
            .collect();
        let ranked = editor_model::rank::rank(
            query,
            &keys,
            editor_model::rank::RankMode::Path,
            &[],
            |_| false,
            cands.len(),
        );
        for r in ranked {
            if out.len() >= limit {
                return out;
            }
            if !used[r.index] {
                used[r.index] = true;
                out.push(cands[r.index].clone());
            }
        }
    }
    out
}

/// Whether `text` is a candidate's stored value.
pub fn is_value(text: &str, cands: &[Candidate]) -> bool {
    cands.iter().any(|c| c.value == text)
}

/// The list under an open field: one button per visible candidate (label + faint secondary
/// line, the highlighted row in the accent colour), then the status line.
pub fn list<'a, M: Clone + 'a>(
    visible: &[Candidate],
    highlight: usize,
    status: &Status,
    on_pick: impl Fn(String) -> M,
    t: Tokens,
) -> Element<'a, M> {
    let mut col = column![].spacing(0);
    for (i, c) in visible.iter().enumerate() {
        let mut label = text(c.label.clone()).size(13);
        if i == highlight {
            label = label.color(t.accent_text.to_color());
        }
        let mut body = column![label];
        if !c.secondary.is_empty() {
            body = body.push(
                text(c.secondary.clone())
                    .size(11)
                    .color(t.text_faint.to_color()),
            );
        }
        col = col.push(
            button(body)
                .style(button::text)
                .padding([2, 10])
                .width(Length::Fill)
                .on_press(on_pick(c.value.clone())),
        );
    }
    let line = match status {
        Status::Ready => None,
        Status::Loading => Some("loading…".to_owned()),
        Status::Failed(e) => Some(e.clone()),
    };
    if let Some(l) = line {
        col = col.push(text(l).size(12).color(t.text_faint.to_color()));
    }
    col.into()
}

/// The `x` clear button beside a field (`None` = disabled).
pub fn clear_button<'a, M: Clone + 'a>(msg: Option<M>, t: Tokens) -> Element<'a, M> {
    row![theme::icon_button(Icon::X, msg, t)]
        .align_y(Alignment::Center)
        .into()
}
