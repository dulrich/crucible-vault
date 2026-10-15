//! OFM (comrak AST) → render-model lowering for the reading pane (WP-3).
//!
//! One parser (D12): the body is parsed with `ofm::comrak_options()` plus
//! `extension.footnotes = true` (plan "Dependency posture", audit M3), and wiki-links are matched
//! with `ofm::wikilink::match_wikilink` (the shared grammar). Frontmatter is split with
//! `ofm::frontmatter::split_frontmatter` and parsed into [`RenderModel::properties`] (the
//! Reading properties block), never into the body. OFM forms comrak does not model
//! (callouts, `==highlight==`, `#tags`, `$$` display math, `![[embeds]]`) are recognised on the
//! lowered tree, the same way `ofm::parse` recognises them over comrak's output.
//!
//! The model is a pure tree: no iced, no I/O. Transclusion content is filled by
//! [`transclude`] with a caller-supplied fetch (the editor resolves + reads), depth 1 only.

use comrak::nodes::{AstNode, ListType, NodeValue, TableAlignment};
use comrak::{Arena, parse_document};

#[cfg(test)]
mod tests;

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

/// One frontmatter value: plain text or a `[[wikilink]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropValue {
    Text(String),
    Link {
        target: String,
        fragment: Option<String>,
        alias: Option<String>,
    },
}

/// One frontmatter key with its values (a scalar has one value, a list one per element).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub key: String,
    pub values: Vec<PropValue>,
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
    },
    /// `![[note#frag]]`; `content` is filled by [`transclude`] (`None` = not loaded/unresolved).
    Transclusion {
        target: String,
        fragment: Option<String>,
        content: Option<Vec<Block>>,
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
    pub footnotes: Vec<FootnoteDef>,
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

/// Extensions treated as images when embedded.
const IMAGE_EXTS: [&str; 8] = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "avif"];

/// Whether a link target names an image file.
pub fn is_image_target(target: &str) -> bool {
    target
        .rsplit_once('.')
        .is_some_and(|(_, ext)| IMAGE_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Whether a URL has a URI scheme (`https:`, `data:`, …) — never resolved to a vault file.
pub fn is_remote(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    scheme.len() > 1
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
}

/// Lowers a whole note (frontmatter included) with default options.
pub fn lower(source: &str) -> RenderModel {
    lower_with(source, LowerOptions::default())
}

/// Lowers a whole note.
pub fn lower_with(source: &str, opts: LowerOptions) -> RenderModel {
    let (meta, body) = ofm::frontmatter::split_frontmatter(source);
    let body = body.strip_prefix('\u{feff}').unwrap_or(body);
    // `body` is a suffix of `source`: its first line's index in the full note.
    let line_offset = source[..source.len() - body.len()].matches('\n').count();
    let properties = meta.as_deref().map(properties).unwrap_or_default();
    let mut options = ofm::comrak_options();
    options.extension.footnotes = true;
    let arena = Arena::new();
    let root = parse_document(&arena, body, &options);
    let mut l = Lower {
        opts,
        line_offset,
        footnotes: Vec::new(),
    };
    let blocks = l.blocks(root);
    RenderModel {
        properties,
        blocks,
        footnotes: l.footnotes,
    }
}

type N<'a> = &'a AstNode<'a>;

struct Lower {
    opts: LowerOptions,
    /// Lines before the body (frontmatter), added to comrak's body-relative line numbers.
    line_offset: usize,
    footnotes: Vec<FootnoteDef>,
}

/// Inline-walk state shared across a block's inline siblings.
#[derive(Default)]
struct InlineState {
    highlight: bool,
}

/// An intermediate inline: either a span or an embed to be split out to a block.
enum Piece {
    Span(Inline),
    Block(Block),
}

impl Lower {
    fn blocks<'a>(&mut self, parent: N<'a>) -> Vec<Block> {
        let mut out = Vec::new();
        for c in parent.children() {
            self.block(c, &mut out);
        }
        out
    }

    fn block<'a>(&mut self, n: N<'a>, out: &mut Vec<Block>) {
        let value = n.data().value.clone();
        match value {
            NodeValue::Paragraph => self.paragraph(n.children(), out),
            NodeValue::Heading(h) => {
                let inlines = spans_only(self.inlines(n.children()));
                out.push(Block::Heading {
                    level: h.level,
                    inlines,
                });
            }
            NodeValue::List(l) => {
                let mut items = Vec::new();
                for item in n.children() {
                    let task = match &item.data().value {
                        NodeValue::TaskItem(t) => Some(t.symbol.unwrap_or(' ')),
                        _ => None,
                    };
                    let line = task.map(|_| {
                        self.line_offset + item.data().sourcepos.start.line.saturating_sub(1)
                    });
                    items.push(ListItem {
                        task,
                        line,
                        children: self.blocks(item),
                    });
                }
                out.push(Block::List {
                    ordered: l.list_type == ListType::Ordered,
                    start: l.start,
                    items,
                });
            }
            NodeValue::CodeBlock(cb) => {
                let lang = cb.info.split_whitespace().next().unwrap_or("").to_owned();
                if lang == "crucible-view" {
                    let line = cb
                        .literal
                        .lines()
                        .map(str::trim)
                        .find(|l| !l.is_empty())
                        .unwrap_or("");
                    let (file, name) = line.split_once('#').unwrap_or((line, ""));
                    out.push(Block::ViewFence {
                        file: file.trim().to_owned(),
                        name: name.trim().to_owned(),
                    });
                } else {
                    out.push(Block::CodeBlock {
                        lang,
                        text: cb.literal.trim_end_matches('\n').to_owned(),
                    });
                }
            }
            NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
                if let Some(c) = self.callout(n) {
                    out.push(c);
                } else {
                    out.push(Block::BlockQuote(self.blocks(n)));
                }
            }
            NodeValue::Alert(a) => {
                out.push(Block::Callout {
                    kind: format!("{:?}", a.alert_type).to_lowercase(),
                    title: a.title.clone(),
                    fold: None,
                    children: self.blocks(n),
                });
            }
            NodeValue::ThematicBreak => out.push(Block::ThematicBreak),
            NodeValue::Table(t) => {
                let alignments = t
                    .alignments
                    .iter()
                    .map(|a| match a {
                        TableAlignment::None => Align::None,
                        TableAlignment::Left => Align::Left,
                        TableAlignment::Center => Align::Center,
                        TableAlignment::Right => Align::Right,
                    })
                    .collect();
                let mut header = Vec::new();
                let mut rows = Vec::new();
                for row in n.children() {
                    let is_header = matches!(row.data().value, NodeValue::TableRow(true));
                    let cells: Vec<Cell> = row
                        .children()
                        .map(|cell| spans_only(self.inlines(cell.children())))
                        .collect();
                    if is_header {
                        header = cells;
                    } else {
                        rows.push(cells);
                    }
                }
                out.push(Block::Table {
                    header,
                    rows,
                    alignments,
                });
            }
            NodeValue::FootnoteDefinition(f) => {
                let blocks = self.blocks(n);
                self.footnotes.push(FootnoteDef {
                    name: f.name.clone(),
                    blocks,
                });
            }
            NodeValue::HtmlBlock(h) => out.push(Block::Paragraph(vec![Inline::Text {
                text: h.literal.trim_end().to_owned(),
                style: Style::default(),
            }])),
            NodeValue::FrontMatter(_) => {}
            _ => {
                for c in n.children() {
                    self.block(c, out);
                }
            }
        }
    }

    /// A paragraph: `$$…$$` display math, else inlines with embeds split out to blocks.
    fn paragraph<'a>(&mut self, children: impl Iterator<Item = N<'a>>, out: &mut Vec<Block>) {
        let nodes: Vec<N<'a>> = children.collect();
        let plain = plain_text(nodes.iter().copied());
        let t = plain.trim();
        if t.len() >= 4 && t.starts_with("$$") && t.ends_with("$$") {
            out.push(Block::CodeBlock {
                lang: "math".into(),
                text: t[2..t.len() - 2].trim().to_owned(),
            });
            return;
        }
        let pieces = self.inlines(nodes.into_iter());
        let mut cur: Vec<Inline> = Vec::new();
        let flush = |cur: &mut Vec<Inline>, out: &mut Vec<Block>| {
            let blank = cur.iter().all(|i| match i {
                Inline::Text { text, .. } => text.trim().is_empty(),
                Inline::LineBreak => true,
                _ => false,
            });
            if !blank {
                out.push(Block::Paragraph(std::mem::take(cur)));
            }
            cur.clear();
        };
        for p in pieces {
            match p {
                Piece::Span(s) => cur.push(s),
                Piece::Block(b) => {
                    flush(&mut cur, out);
                    out.push(b);
                }
            }
        }
        flush(&mut cur, out);
    }

    /// `> [!kind]± Title` blockquotes.
    fn callout<'a>(&mut self, n: N<'a>) -> Option<Block> {
        let first = n.first_child()?;
        if !matches!(first.data().value, NodeValue::Paragraph) {
            return None;
        }
        let head: Vec<N<'a>> = first
            .children()
            .take_while(|c| !matches!(c.data().value, NodeValue::SoftBreak | NodeValue::LineBreak))
            .collect();
        let head_text = plain_text(head.iter().copied());
        let rest = head_text.trim_start().strip_prefix("[!")?;
        let close = rest.find(']')?;
        let kind = rest[..close].trim().to_lowercase();
        if kind.is_empty() {
            return None;
        }
        let mut after = &rest[close + 1..];
        let fold = if let Some(r) = after.strip_prefix('-') {
            after = r;
            Some(Fold::Folded)
        } else if let Some(r) = after.strip_prefix('+') {
            after = r;
            Some(Fold::Open)
        } else {
            None
        };
        let title = Some(after.trim().to_owned()).filter(|t| !t.is_empty());
        let mut children = Vec::new();
        let body_inlines = first
            .children()
            .skip(head.len())
            .skip_while(|c| matches!(c.data().value, NodeValue::SoftBreak | NodeValue::LineBreak));
        self.paragraph(body_inlines, &mut children);
        for c in n.children().skip(1) {
            self.block(c, &mut children);
        }
        Some(Block::Callout {
            kind,
            title,
            fold,
            children,
        })
    }

    fn inlines<'a>(&mut self, nodes: impl Iterator<Item = N<'a>>) -> Vec<Piece> {
        let mut out = Vec::new();
        let mut st = InlineState::default();
        for n in nodes {
            self.inline(n, Style::default(), &mut st, &mut out);
        }
        merge_text(out)
    }

    fn inline<'a>(&mut self, n: N<'a>, style: Style, st: &mut InlineState, out: &mut Vec<Piece>) {
        let value = n.data().value.clone();
        match value {
            NodeValue::Text(t) => self.text(&t, style, st, out),
            NodeValue::SoftBreak => text_piece(out, " ", style, st),
            NodeValue::LineBreak => out.push(Piece::Span(Inline::LineBreak)),
            NodeValue::Code(c) => out.push(Piece::Span(Inline::Code(c.literal.clone()))),
            NodeValue::HtmlInline(h) | NodeValue::Raw(h) => text_piece(out, &h, style, st),
            NodeValue::Strong => self.children(
                n,
                Style {
                    strong: true,
                    ..style
                },
                st,
                out,
            ),
            NodeValue::Emph => self.children(n, Style { em: true, ..style }, st, out),
            NodeValue::Strikethrough => self.children(
                n,
                Style {
                    strike: true,
                    ..style
                },
                st,
                out,
            ),
            NodeValue::Highlight => self.children(
                n,
                Style {
                    highlight: true,
                    ..style
                },
                st,
                out,
            ),
            NodeValue::Link(l) => out.push(Piece::Span(Inline::Link {
                url: l.url.clone(),
                label: plain_text(n.children()),
            })),
            NodeValue::Image(l) => {
                let alt_raw = plain_text(n.children());
                let (alt, width) = split_width(&alt_raw);
                out.push(Piece::Block(Block::Image {
                    remote: is_remote(&l.url),
                    src: l.url.clone(),
                    alt: alt.to_owned(),
                    width,
                }));
            }
            NodeValue::WikiLink(w) => {
                let label = plain_text(n.children());
                let raw = if label.is_empty() || label == w.url {
                    format!("[[{}]]", w.url)
                } else {
                    format!("[[{}|{label}]]", w.url)
                };
                // `![[x|a]]` sometimes reaches comrak's wikilink rule after a literal `!`.
                let embed = pop_bang(out);
                let wiki = ofm::wikilink::match_wikilink(&raw).map(|(_, w)| w);
                let (target, fragment, alias) = match wiki {
                    Some(w) => (w.slug, w.fragment, w.alias),
                    None => (w.url.clone(), None, Some(label).filter(|l| !l.is_empty())),
                };
                self.wiki(target, fragment, alias, embed, out);
            }
            NodeValue::FootnoteReference(f) => out.push(Piece::Span(Inline::FootnoteRef {
                name: f.name.clone(),
                ix: f.ix,
            })),
            _ => self.children(n, style, st, out),
        }
    }

    fn children<'a>(&mut self, n: N<'a>, style: Style, st: &mut InlineState, out: &mut Vec<Piece>) {
        for c in n.children() {
            self.inline(c, style, st, out);
        }
    }

    fn wiki(
        &mut self,
        target: String,
        fragment: Option<String>,
        alias: Option<String>,
        embed: bool,
        out: &mut Vec<Piece>,
    ) {
        if embed && is_image_target(&target) {
            let width = alias.as_deref().and_then(parse_width);
            let alt = match (&alias, width) {
                (Some(a), None) => a.clone(),
                _ => target.rsplit('/').next().unwrap_or(&target).to_owned(),
            };
            out.push(Piece::Block(Block::Image {
                src: target,
                alt,
                width,
                remote: false,
            }));
        } else if embed && self.opts.transclude {
            out.push(Piece::Block(Block::Transclusion {
                target,
                fragment,
                content: None,
            }));
        } else {
            out.push(Piece::Span(Inline::WikiLink {
                target,
                fragment,
                alias,
            }));
        }
    }

    /// A text run: wiki-links (`ofm` grammar), `==` highlight toggles and `#tags`.
    fn text(&mut self, t: &str, style: Style, st: &mut InlineState, out: &mut Vec<Piece>) {
        let mut buf = String::new();
        let mut i = 0;
        let b = t.as_bytes();
        while i < t.len() {
            let rest = &t[i..];
            if (rest.starts_with("[[") || rest.starts_with("![["))
                && let Some((len, w)) = ofm::wikilink::match_wikilink(rest)
            {
                text_piece(out, &std::mem::take(&mut buf), style, st);
                self.wiki(w.slug, w.fragment, w.alias, w.embed, out);
                i += len;
                continue;
            }
            if rest.starts_with("==") {
                text_piece(out, &std::mem::take(&mut buf), style, st);
                st.highlight = !st.highlight;
                i += 2;
                continue;
            }
            if b[i] == b'#' && (i == 0 || tag_boundary(t[..i].chars().next_back())) {
                let name_len = rest[1..]
                    .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '/')))
                    .unwrap_or(rest.len() - 1);
                let name = &rest[1..1 + name_len];
                if !name.is_empty() && !name.chars().all(|c| c.is_ascii_digit()) {
                    text_piece(out, &std::mem::take(&mut buf), style, st);
                    out.push(Piece::Span(Inline::Tag(name.to_owned())));
                    i += 1 + name_len;
                    continue;
                }
            }
            let ch = rest.chars().next().unwrap_or(' ');
            buf.push(ch);
            i += ch.len_utf8();
        }
        text_piece(out, &buf, style, st);
    }
}

fn tag_boundary(prev: Option<char>) -> bool {
    prev.is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '[' | ',' | ';'))
}

fn text_piece(out: &mut Vec<Piece>, s: &str, style: Style, st: &InlineState) {
    if s.is_empty() {
        return;
    }
    let style = Style {
        highlight: style.highlight || st.highlight,
        ..style
    };
    out.push(Piece::Span(Inline::Text {
        text: s.to_owned(),
        style,
    }));
}

/// Strips a trailing unescaped `!` off the last text span (an embed marker).
fn pop_bang(out: &mut [Piece]) -> bool {
    if let Some(Piece::Span(Inline::Text { text, .. })) = out.last_mut()
        && text.ends_with('!')
        && !text.ends_with("\\!")
    {
        text.pop();
        return true;
    }
    false
}

/// Merges adjacent same-style text spans.
fn merge_text(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::with_capacity(pieces.len());
    for p in pieces {
        if let Piece::Span(Inline::Text { text, style }) = &p
            && let Some(Piece::Span(Inline::Text {
                text: prev,
                style: ps,
            })) = out.last_mut()
            && ps == style
        {
            prev.push_str(text);
            continue;
        }
        if let Piece::Span(Inline::Text { text, .. }) = &p
            && text.is_empty()
        {
            continue;
        }
        out.push(p);
    }
    out
}

fn spans_only(pieces: Vec<Piece>) -> Vec<Inline> {
    pieces
        .into_iter()
        .filter_map(|p| match p {
            Piece::Span(s) => Some(s),
            Piece::Block(Block::Image { alt, .. }) => Some(Inline::Text {
                text: alt,
                style: Style::default(),
            }),
            Piece::Block(Block::Transclusion {
                target, fragment, ..
            }) => Some(Inline::WikiLink {
                target,
                fragment,
                alias: None,
            }),
            Piece::Block(_) => None,
        })
        .collect()
}

/// Concatenated literal text (soft breaks as `\n`).
fn plain_text<'a>(nodes: impl Iterator<Item = N<'a>>) -> String {
    let mut s = String::new();
    for n in nodes {
        for d in n.descendants() {
            match &d.data().value {
                NodeValue::Text(t) => s.push_str(t),
                NodeValue::Code(c) => s.push_str(&c.literal),
                NodeValue::SoftBreak | NodeValue::LineBreak => s.push('\n'),
                NodeValue::WikiLink(w) if d.first_child().is_none() => s.push_str(&w.url),
                _ => {}
            }
        }
    }
    s
}

/// `300` / `300x200` → 300.
fn parse_width(s: &str) -> Option<u32> {
    let w = s.split_once('x').map_or(s, |(w, _)| w);
    w.trim().parse().ok()
}

/// `alt|300` → (`alt`, Some(300)).
fn split_width(alt: &str) -> (&str, Option<u32>) {
    match alt.rsplit_once('|') {
        Some((a, w)) => match parse_width(w) {
            Some(n) => (a, Some(n)),
            None => (alt, None),
        },
        None => (alt, None),
    }
}

/// Fills every top-level-reachable [`Block::Transclusion`] with the fetched note's blocks,
/// lowered with `transclude = false` (depth 1: embeds inside become links). `fetch` maps a link
/// target to the embedded note's source; `None` leaves the block unloaded.
pub fn transclude(model: &mut RenderModel, fetch: &mut dyn FnMut(&str) -> Option<String>) {
    fill(&mut model.blocks, fetch);
}

fn fill(blocks: &mut [Block], fetch: &mut dyn FnMut(&str) -> Option<String>) {
    for b in blocks {
        match b {
            Block::Transclusion {
                target,
                fragment,
                content,
            } => {
                *content = fetch(target).map(|src| {
                    let m = lower_with(&src, LowerOptions { transclude: false });
                    let mut blocks = match fragment.as_deref() {
                        Some(f) if !f.starts_with('^') => section(m.blocks, f),
                        _ => m.blocks,
                    };
                    // Lines index the embedded note, not the host: display-only (audit M1).
                    clear_lines(&mut blocks);
                    blocks
                });
            }
            Block::List { items, .. } => {
                for i in items {
                    fill(&mut i.children, fetch);
                }
            }
            Block::Callout { children, .. } | Block::BlockQuote(children) => fill(children, fetch),
            _ => {}
        }
    }
}

fn clear_lines(blocks: &mut [Block]) {
    for b in blocks {
        match b {
            Block::List { items, .. } => {
                for i in items {
                    i.line = None;
                    clear_lines(&mut i.children);
                }
            }
            Block::Callout { children, .. } | Block::BlockQuote(children) => {
                clear_lines(children);
            }
            _ => {}
        }
    }
}

/// Every host-note task line with its status, in document order (transclusions excluded).
pub fn task_lines(model: &RenderModel) -> Vec<(usize, char)> {
    let mut out = Vec::new();
    collect_tasks(&model.blocks, &mut out);
    out
}

fn collect_tasks(blocks: &[Block], out: &mut Vec<(usize, char)>) {
    for b in blocks {
        match b {
            Block::List { items, .. } => {
                for i in items {
                    if let (Some(l), Some(c)) = (i.line, i.task) {
                        out.push((l, c));
                    }
                    collect_tasks(&i.children, out);
                }
            }
            Block::Callout { children, .. } | Block::BlockQuote(children) => {
                collect_tasks(children, out);
            }
            _ => {}
        }
    }
}

/// The frontmatter YAML as property rows; a parse failure yields no rows.
fn properties(meta: &str) -> Vec<Property> {
    use ofm::frontmatter::{JsValue, parse_frontmatter};
    let Ok(JsValue::Obj(obj)) = parse_frontmatter(meta) else {
        return Vec::new();
    };
    obj.entries()
        .into_iter()
        .map(|(k, v)| Property {
            key: k.to_owned(),
            values: match v {
                JsValue::Arr(a) => a.iter().map(prop_value).collect(),
                JsValue::Null => Vec::new(),
                v => vec![prop_value(v)],
            },
        })
        .collect()
}

fn prop_value(v: &ofm::frontmatter::JsValue) -> PropValue {
    use ofm::frontmatter::{JsValue, js_number_to_string, json_stringify};
    let s = match v {
        JsValue::Str(s) => s.clone(),
        JsValue::Num(n) => js_number_to_string(*n),
        JsValue::Bool(b) => b.to_string(),
        JsValue::Null => String::new(),
        other => json_stringify(other),
    };
    let t = s.trim();
    if let Some((len, w)) = ofm::wikilink::match_wikilink(t)
        && len == t.len()
        && !w.embed
    {
        return PropValue::Link {
            target: w.slug,
            fragment: w.fragment,
            alias: w.alias,
        };
    }
    PropValue::Text(s)
}

/// The toggle for the task at `line` of `source`: `[x]`/`[X]` → `[ ]`; `[ ]` and custom
/// statuses (`[/]`, `[-]`, …) → `[x]`. Returns the status char's byte-column range within
/// that line and its replacement, or `None` if the line holds no task box.
pub fn task_toggle(source: &str, line: usize) -> Option<(std::ops::Range<usize>, char)> {
    let start: usize = source.split_inclusive('\n').take(line).map(str::len).sum();
    let text = source[start..].split('\n').next()?;
    let open = text.find('[')?;
    let prefix = text[..open].trim_start();
    // Only a list marker (and quote/callout `>` prefixes) may precede the box.
    let marker_ok = prefix
        .trim_start_matches(|c: char| c == '>' || c.is_whitespace())
        .trim_end()
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '-' | '*' | '+' | '.' | ')'));
    let rest = &text[open + 1..];
    let status = rest.chars().next()?;
    if !marker_ok || !rest[status.len_utf8()..].starts_with(']') {
        return None;
    }
    let at = open + 1;
    let next = if matches!(status, 'x' | 'X') {
        ' '
    } else {
        'x'
    };
    Some((at..at + status.len_utf8(), next))
}

/// The blocks under heading `name` (until the next heading of the same or higher level).
fn section(blocks: Vec<Block>, name: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut level = None;
    for b in blocks {
        if let Block::Heading { level: l, inlines } = &b {
            if let Some(cur) = level {
                if *l <= cur {
                    break;
                }
            } else if inline_text(inlines)
                .trim()
                .eq_ignore_ascii_case(name.trim())
            {
                level = Some(*l);
            }
        }
        if level.is_some() {
            out.push(b);
        }
    }
    out
}

/// The plain text of inlines (for headings, titles, alt text).
pub fn inline_text(inlines: &[Inline]) -> String {
    let mut s = String::new();
    for i in inlines {
        match i {
            Inline::Text { text, .. } | Inline::Code(text) => s.push_str(text),
            Inline::WikiLink { target, alias, .. } => {
                s.push_str(alias.as_deref().unwrap_or(target))
            }
            Inline::Link { label, .. } => s.push_str(label),
            Inline::Tag(t) => {
                s.push('#');
                s.push_str(t);
            }
            Inline::FootnoteRef { ix, .. } => s.push_str(&format!("[{ix}]")),
            Inline::LineBreak => s.push('\n'),
        }
    }
    s
}

/// Every image block's `(src, remote)`, in document order, including inside transclusions.
pub fn images(model: &RenderModel) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    collect_images(&model.blocks, &mut out);
    out
}

fn collect_images(blocks: &[Block], out: &mut Vec<(String, bool)>) {
    for b in blocks {
        match b {
            Block::Image { src, remote, .. } => out.push((src.clone(), *remote)),
            Block::List { items, .. } => {
                for i in items {
                    collect_images(&i.children, out);
                }
            }
            Block::Callout { children, .. } | Block::BlockQuote(children) => {
                collect_images(children, out);
            }
            Block::Transclusion {
                content: Some(c), ..
            } => collect_images(c, out),
            _ => {}
        }
    }
}
