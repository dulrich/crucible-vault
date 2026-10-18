//! Plan round-6-feedback-todos WP-6: the one task-status glyph map and status set, shared by
//! Reading (note-body tasks, frontmatter booleans), views (intake `status` cells, `Tasks` rows),
//! the sidebar and the status context menu. Moved verbatim from `reading/view.rs` /
//! `context_menu.rs`; Reading output is unchanged.

use iced::widget::{Space, container, text};
use iced::{Border, Element, Theme};

use crate::icons::Icon;
use crate::message::Message;
use crate::theme::{Rgb, Tokens};

/// The themed task status set (plan editor-feedback-round-2 WP-5's glyph map, [`task_mark`]): status char + short label.
pub const TASK_STATUSES: &[(char, &str)] = &[
    (' ', "To do"),
    ('x', "Done"),
    ('/', "In progress"),
    ('-', "Cancelled"),
    ('>', "Forwarded"),
    ('<', "Scheduled"),
    ('?', "Question"),
    ('!', "Important"),
    ('"', "Quote"),
    ('*', "Star"),
    ('l', "Location"),
    ('i', "Information"),
    ('S', "Savings"),
    ('I', "Idea"),
    ('f', "Fire"),
    ('k', "Key"),
    ('u', "Up"),
    ('d', "Down"),
    ('w', "Win"),
    ('p', "Pro"),
    ('c', "Con"),
    ('b', "Bookmark"),
];

/// The colour role of a task marker, resolved against `Tokens` at draw time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskRole {
    Dim,
    Faint,
    Accent,
    Red,
    Orange,
    Yellow,
    Green,
    Cyan,
    Blue,
    Purple,
}

impl TaskRole {
    fn color(self, t: Tokens) -> Rgb {
        match self {
            Self::Dim => t.text_dim,
            Self::Faint => t.text_faint,
            Self::Accent => t.accent,
            Self::Red => t.red,
            Self::Orange => t.orange,
            Self::Yellow => t.yellow,
            Self::Green => t.green,
            Self::Cyan => t.cyan,
            Self::Blue => t.blue,
            Self::Purple => t.purple,
        }
    }
}

/// The glyph and colour role of a task status char (plan editor-feedback-round-2 WP-5).
///
/// Source: Crucible theme alternate checkboxes, `https://github.com/dulrich/obsidian-crucible/blob/master/theme/theme.css`
/// lines 447-744 (Block 4); the CSS `--color-*` / `--text-faint` roles map to the same-named
/// `Tokens`, Obsidian's `--interactive-accent` (`x`, `/`) to `accent`. The inline SVG masks are
/// replaced by the nearest lucide glyph. Obsidian-induced part dropped: the `input:checked` +
/// `mask-image` / `::after` selector machinery (Obsidian renders tasks as HTML checkboxes); the
/// box-filled variants (`?`, `"`, `i`, `S`) draw as a coloured glyph without the filled box.
/// `' '` is an empty box; an unknown char (`None` glyph, not `' '`) is a box showing the char.
pub(crate) fn task_mark(c: char) -> (Option<Icon>, TaskRole) {
    match c {
        ' ' => (None, TaskRole::Dim),
        'x' | 'X' => (Some(Icon::Check), TaskRole::Accent),
        '>' => (Some(Icon::ArrowRight), TaskRole::Faint),
        '<' => (Some(Icon::Calendar), TaskRole::Faint),
        '?' => (Some(Icon::CircleHelp), TaskRole::Yellow),
        '/' => (Some(Icon::SquareSlash), TaskRole::Accent),
        '!' => (Some(Icon::TriangleAlert), TaskRole::Orange),
        '"' | '\u{201C}' => (Some(Icon::Quote), TaskRole::Cyan),
        '-' => (Some(Icon::Minus), TaskRole::Faint),
        '*' => (Some(Icon::Star), TaskRole::Yellow),
        'l' => (Some(Icon::MapPin), TaskRole::Red),
        'i' => (Some(Icon::Info), TaskRole::Blue),
        'S' => (Some(Icon::DollarSign), TaskRole::Green),
        'I' => (Some(Icon::Lightbulb), TaskRole::Yellow),
        'f' => (Some(Icon::Flame), TaskRole::Red),
        'k' => (Some(Icon::Key), TaskRole::Yellow),
        'u' => (Some(Icon::TrendingUp), TaskRole::Green),
        'd' => (Some(Icon::TrendingDown), TaskRole::Red),
        'w' => (Some(Icon::Trophy), TaskRole::Purple),
        'p' => (Some(Icon::ThumbsUp), TaskRole::Green),
        'c' => (Some(Icon::ThumbsDown), TaskRole::Orange),
        'b' => (Some(Icon::Bookmark), TaskRole::Orange),
        _ => (None, TaskRole::Dim),
    }
}

/// The drawn checkbox for status char `c`, `side` px square.
pub(crate) fn task_box<'a>(c: char, side: f32, t: Tokens) -> Element<'a, Message> {
    let (glyph, role) = task_mark(c);
    let col = role.color(t);
    let boxed_char = |inner: Element<'a, Message>, fill: Option<Rgb>, edge: Rgb| {
        container(inner)
            .width(side)
            .height(side)
            .center_x(side)
            .center_y(side)
            .style(move |_: &Theme| container::Style {
                background: fill.map(|f| f.to_color().into()),
                border: Border {
                    color: edge.to_color(),
                    width: 1.5,
                    radius: 4.0.into(),
                },
                ..container::Style::default()
            })
            .into()
    };
    match (c, glyph) {
        ('x' | 'X', Some(icon)) => boxed_char(
            text(icon.glyph())
                .font(crate::icons::FONT)
                .size(side * 0.8)
                .color(t.on_accent.to_color())
                .into(),
            Some(col),
            col,
        ),
        (_, Some(icon)) => container(
            text(icon.glyph())
                .font(crate::icons::FONT)
                .size(side)
                .color(col.to_color()),
        )
        .center_x(side)
        .center_y(side)
        .into(),
        (' ', None) => boxed_char(Space::new().into(), None, t.text_faint),
        (_, None) => boxed_char(
            text(c.to_string())
                .size(side * 0.75)
                .color(col.to_color())
                .into(),
            None,
            t.text_faint,
        ),
    }
}

/// The click toggle of a free-form status: `x`/`X` → open (`None`), anything else → `x`.
#[must_use]
pub fn toggled(status: Option<char>) -> Option<char> {
    match status {
        Some('x' | 'X') => None,
        _ => Some('x'),
    }
}
