//! iced `Highlighter` adapter over `editor_model::highlight` (plan WP-2).
//!
//! iced re-feeds lines from `current_line` after `change_line(n)`; the adapter caches the
//! [`LineState`] at the start of every line so re-entry resumes from the saved state. Settings
//! carry the resolved colours plus an `epoch` the buffer bumps whenever it replaces its
//! `Content` wholesale (undo/redo/reload): iced only calls `update` when settings change, which
//! is how a replaced buffer drops the stale per-line cache.

use std::ops::Range;

use editor_model::highlight::{Kind, LineState, scan_line};
use iced::Color;
use iced::advanced::text::highlighter::{self, Highlighter};

use crate::theme::Tokens;

/// Colour per [`Kind`] (resolved from theme tokens).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KindColors {
    pub frontmatter_key: Color,
    pub frontmatter_value: Color,
    pub fence_marker: Color,
    pub code: Color,
    pub heading: Color,
    pub wikilink: Color,
    pub embed: Color,
    pub md_link: Color,
    pub tag: Color,
    pub emphasis: Color,
    pub strong: Color,
    pub highlight: Color,
    pub task_done: Color,
    pub task_forwarded: Color,
    pub task_other: Color,
    pub callout: Color,
    pub footnote: Color,
}

impl KindColors {
    /// The token map (documented in the WP-2 report).
    pub fn from_tokens(t: &Tokens) -> Self {
        Self {
            frontmatter_key: t.cyan.to_color(),
            frontmatter_value: t.text_dim.to_color(),
            fence_marker: t.muted.to_color(),
            code: t.green.to_color(),
            heading: t.heading.to_color(),
            wikilink: t.accent.to_color(),
            embed: t.purple.to_color(),
            md_link: t.blue.to_color(),
            tag: t.orange.to_color(),
            emphasis: t.pink.to_color(),
            strong: t.red.to_color(),
            highlight: t.yellow.to_color(),
            task_done: t.ok_text.to_color(),
            task_forwarded: t.warn_text.to_color(),
            task_other: t.accent_ui.to_color(),
            callout: t.info_text.to_color(),
            footnote: t.text_faint.to_color(),
        }
    }

    /// The colour of one kind.
    pub fn of(&self, k: Kind) -> Color {
        match k {
            Kind::FrontmatterKey => self.frontmatter_key,
            Kind::FrontmatterValue => self.frontmatter_value,
            Kind::FenceMarker => self.fence_marker,
            Kind::Code => self.code,
            Kind::Heading(_) => self.heading,
            Kind::Wikilink => self.wikilink,
            Kind::Embed => self.embed,
            Kind::MdLink => self.md_link,
            Kind::Tag => self.tag,
            Kind::Emphasis => self.emphasis,
            Kind::Strong => self.strong,
            Kind::Highlight => self.highlight,
            Kind::TaskMarker('x' | 'X') => self.task_done,
            Kind::TaskMarker('>') => self.task_forwarded,
            Kind::TaskMarker(_) => self.task_other,
            Kind::CalloutHeader => self.callout,
            Kind::Footnote => self.footnote,
        }
    }
}

/// Highlighter settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub colors: KindColors,
    /// Bumped on wholesale content replacement.
    pub epoch: u64,
}

/// The adapter.
#[derive(Debug)]
pub struct MarkdownHighlighter {
    colors: KindColors,
    /// `states[i]` = state at the start of line `i`; always non-empty.
    states: Vec<LineState>,
    current: usize,
}

impl Highlighter for MarkdownHighlighter {
    type Settings = Settings;
    type Highlight = Color;
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, Color)>;

    fn new(settings: &Settings) -> Self {
        Self {
            colors: settings.colors,
            states: vec![LineState::default()],
            current: 0,
        }
    }

    fn update(&mut self, settings: &Settings) {
        self.colors = settings.colors;
        self.change_line(0);
    }

    fn change_line(&mut self, line: usize) {
        let keep = line.min(self.states.len() - 1);
        self.states.truncate(keep + 1);
        self.current = keep;
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        let st = self.states.get(self.current).copied().unwrap_or_default();
        let (spans, next) = scan_line(st, line);
        self.states.truncate(self.current + 1);
        self.states.push(next);
        self.current += 1;
        let colors = self.colors;
        spans
            .into_iter()
            .map(|(r, k)| (r, colors.of(k)))
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn current_line(&self) -> usize {
        self.current
    }
}

/// iced `to_format`.
pub fn to_format(c: &Color, _theme: &iced::Theme) -> highlighter::Format<iced::Font> {
    highlighter::Format {
        color: Some(*c),
        font: None,
    }
}
