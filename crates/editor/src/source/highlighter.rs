//! iced `Highlighter` adapter over `editor_model::highlight` (plan WP-2).
//!
//! iced re-feeds lines from `current_line` after `change_line(n)`; the adapter caches the
//! [`LineState`] at the start of every line so re-entry resumes from the saved state. Settings
//! carry the resolved colours plus an `epoch` the buffer bumps whenever it replaces its
//! `Content` wholesale (undo/redo/reload): iced only calls `update` when settings change, which
//! is how a replaced buffer drops the stale per-line cache.

use std::ops::Range;
use std::sync::Arc;

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
    /// The find bar's current match (plan editor-feedback-round-2 WP-4; painted bold).
    pub find: Color,
    /// Every other find match (plan editor-ui-round-3 WP-5; the light highlight).
    pub find_all: Color,
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
            find: t.find_strong().to_color(),
            find_all: t.find_light().to_color(),
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

/// A find match to paint: (line, byte column) `from` .. `to` (WP-4).
pub type FindSpan = ((usize, usize), (usize, usize));

/// One painted span: a colour, optionally bold (the find match).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Paint {
    pub color: Color,
    pub bold: bool,
}

impl Paint {
    fn plain(color: Color) -> Self {
        Self { color, bold: false }
    }
}

/// Highlighter settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub colors: KindColors,
    /// Bumped on wholesale content replacement.
    pub epoch: u64,
    /// The find match painted over the markdown spans (iced re-highlights when it changes).
    pub find: Option<FindSpan>,
    /// Every find match (plan editor-ui-round-3 WP-5), painted light under the current one.
    pub find_all: Arc<[FindSpan]>,
}

/// The byte range of `line` (index `ix`, length `len`) covered by `find`, if non-empty.
pub fn find_cols(find: Option<FindSpan>, ix: usize, len: usize) -> Option<Range<usize>> {
    let ((l0, c0), (l1, c1)) = find?;
    if ix < l0 || ix > l1 {
        return None;
    }
    let a = if ix == l0 { c0.min(len) } else { 0 };
    let b = if ix == l1 { c1.min(len) } else { len };
    (a < b).then_some(a..b)
}

/// The adapter.
#[derive(Debug)]
pub struct MarkdownHighlighter {
    colors: KindColors,
    find: Option<FindSpan>,
    find_all: Arc<[FindSpan]>,
    /// `states[i]` = state at the start of line `i`; always non-empty.
    states: Vec<LineState>,
    current: usize,
}

impl Highlighter for MarkdownHighlighter {
    type Settings = Settings;
    type Highlight = Paint;
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, Paint)>;

    fn new(settings: &Settings) -> Self {
        Self {
            colors: settings.colors,
            find: settings.find,
            find_all: settings.find_all.clone(),
            states: vec![LineState::default()],
            current: 0,
        }
    }

    fn update(&mut self, settings: &Settings) {
        self.colors = settings.colors;
        self.find = settings.find;
        self.find_all = settings.find_all.clone();
        self.change_line(0);
    }

    fn change_line(&mut self, line: usize) {
        let keep = line.min(self.states.len() - 1);
        self.states.truncate(keep + 1);
        self.current = keep;
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        let ix = self.current;
        let st = self.states.get(ix).copied().unwrap_or_default();
        let (spans, next) = scan_line(st, line);
        self.states.truncate(ix + 1);
        self.states.push(next);
        self.current += 1;
        let colors = self.colors;
        let mut out: Vec<_> = spans
            .into_iter()
            .map(|(r, k)| (r, Paint::plain(colors.of(k))))
            .collect();
        // Pushed last: overlapping spans resolve to the later one (cosmic-text `add_span`), so
        // the light all-match spans go first and the strong current match over them.
        for m in self.find_all.iter() {
            if let Some(r) = find_cols(Some(*m), ix, line.len()) {
                out.push((r, Paint::plain(colors.find_all)));
            }
        }
        if let Some(r) = find_cols(self.find, ix, line.len()) {
            out.push((
                r,
                Paint {
                    color: colors.find,
                    bold: true,
                },
            ));
        }
        out.into_iter()
    }

    fn current_line(&self) -> usize {
        self.current
    }
}

/// iced `to_format`.
pub fn to_format(p: &Paint, _theme: &iced::Theme) -> highlighter::Format<iced::Font> {
    highlighter::Format {
        color: Some(p.color),
        // The app's default family (main.rs `default_font`), not `Font::DEFAULT` (generic
        // sans-serif), so the bold match keeps the editor's typeface.
        font: p.bold.then_some(iced::Font {
            weight: iced::font::Weight::Bold,
            ..iced::Font::with_name(crate::theme::FONT_FAMILY)
        }),
    }
}
