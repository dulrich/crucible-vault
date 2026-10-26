//! The render-model tree (no iced, no I/O).

use crate::props::Property;

/// Inline style flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub strong: bool,
    pub em: bool,
    pub strike: bool,
    pub highlight: bool,
}

/// An inline span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text {
        text: String,
        style: Style,
    },
    Code(String),
    WikiLink {
        target: String,
        fragment: Option<String>,
        alias: Option<String>,
    },
    Link {
        url: String,
        label: String,
    },
    Tag(String),
    FootnoteRef {
        name: String,
        ix: u32,
    },
    LineBreak,
}

/// Column alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

/// Callout fold marker: `+` (foldable, open) or `-` (foldable, folded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    Open,
    Folded,
}

/// One list item; `task` is the status char (`' '`, `x`, `>`, `-`, `/`, …) for task items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    pub task: Option<char>,
    /// The 0-based line of a task item in the lowered note's full source (frontmatter
    /// included) — the toggle target. `None` for non-tasks and for every item inside
    /// transcluded content (display-only, audit M1).
    pub line: Option<usize>,
    pub children: Vec<Block>,
}

/// A table cell's inlines.
pub type Cell = Vec<Inline>;

/// A block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        inlines: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    List {
        ordered: bool,
        start: usize,
        items: Vec<ListItem>,
    },
    Table {
        header: Vec<Cell>,
        rows: Vec<Vec<Cell>>,
        alignments: Vec<Align>,
    },
    Callout {
        kind: String,
        title: Option<String>,
        fold: Option<Fold>,
        children: Vec<Block>,
    },
    /// Fenced/indented code; `$$…$$` display math lowers to `lang = "math"`.
    CodeBlock {
        lang: String,
        text: String,
    },
    BlockQuote(Vec<Block>),
    ThematicBreak,
    /// `![[img.png|300]]` or `![alt|300](src)`. `remote` = a URI with a scheme (never fetched).
    Image {
        src: String,
        alt: String,
        width: Option<u32>,
        remote: bool,
        /// Outer link href for `[![alt](src)](href)`; `None` for a bare image.
        link: Option<String>,
    },
    /// `![[note#frag]]`; `content` is filled by [`transclude`] (`None` = not loaded/unresolved).
    Transclusion {
        target: String,
        fragment: Option<String>,
        content: Option<Vec<Block>>,
    },
    /// Plan round-14 WP-3: `![[file.pdf]]` (any embed classified external) — a file card that
    /// opens the file with the system opener.
    FileEmbed {
        target: String,
    },
    /// Plan round-15 WP-4: `![[file.pdf#page=N|width]]` — page `page` (1-based) rendered
    /// inline; `width` is the `|<width>` alias, as for an image embed.
    PdfEmbed {
        target: String,
        page: u32,
        width: Option<u32>,
    },
    /// A `crucible-view` fence (one-line body `<file>#<name>`), rendered by WP-7's hook.
    ViewFence {
        file: String,
        name: String,
    },
}

/// A footnote definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FootnoteDef {
    pub name: String,
    pub blocks: Vec<Block>,
}

/// A lowered note.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderModel {
    /// The parsed frontmatter, in key order (empty when absent or unparseable).
    pub properties: Vec<Property>,
    pub blocks: Vec<Block>,
    /// Source provenance (plan editor-ui-round-3 WP-5, block granularity): `block_lines[i]` is
    /// the 0-based line range (end exclusive) of `blocks[i]` in the lowered note's full source
    /// (frontmatter included). Blocks split out of one source block share its range.
    pub block_lines: Vec<std::ops::Range<usize>>,
    pub footnotes: Vec<FootnoteDef>,
}

impl RenderModel {
    /// The index of the top-level block whose source lines contain `line`, if any.
    pub fn block_at_line(&self, line: usize) -> Option<usize> {
        self.block_lines.iter().position(|r| r.contains(&line))
    }

    /// plan editor-ui-round-3 WP-6 (place-keeping): the block to show at the top for source
    /// `line` — the one containing it ([`Self::block_at_line`]), else the first block after it
    /// (blank lines, frontmatter), else the last block.
    pub fn block_for_line(&self, line: usize) -> Option<usize> {
        self.block_at_line(line)
            .or_else(|| self.block_lines.iter().position(|r| r.start > line))
            .or_else(|| self.block_lines.len().checked_sub(1))
    }
}

/// Lowering options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LowerOptions {
    /// `![[note]]` becomes a [`Block::Transclusion`]; when false it lowers to a wiki-link (the
    /// depth cap inside transcluded content).
    pub transclude: bool,
}

impl Default for LowerOptions {
    fn default() -> Self {
        Self { transclude: true }
    }
}
