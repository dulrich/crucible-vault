//! comrak AST → [`RenderModel`] lowering.

use comrak::nodes::{AstNode, ListType, NodeValue, TableAlignment};
use comrak::{Arena, parse_document};

use super::model::{
    Align, Block, Cell, Fold, FootnoteDef, Inline, ListItem, LowerOptions, RenderModel, Style,
};

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
    let properties = meta
        .as_deref()
        .map(crate::props::from_yaml)
        .unwrap_or_default();
    let mut options = ofm::comrak_options();
    options.extension.footnotes = true;
    let arena = Arena::new();
    let root = parse_document(&arena, body, &options);
    let mut l = Lower {
        opts,
        line_offset,
        footnotes: Vec::new(),
    };
    let mut blocks = Vec::new();
    let mut block_lines = Vec::new();
    for c in root.children() {
        let before = blocks.len();
        l.block(c, &mut blocks);
        let sp = c.data().sourcepos;
        let start = line_offset + sp.start.line.saturating_sub(1);
        let end = (line_offset + sp.end.line).max(start + 1);
        block_lines.resize(blocks.len(), start..end);
        debug_assert!(block_lines.len() >= before);
    }
    RenderModel {
        properties,
        blocks,
        block_lines,
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
            NodeValue::Paragraph => match inline_html_media(n) {
                Some(card) => out.push(card),
                None => self.paragraph(n.children(), out),
            },
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
            NodeValue::HtmlBlock(h) => {
                out.push(html_media_block(&h.literal).unwrap_or_else(|| {
                    Block::Paragraph(vec![Inline::Text {
                        text: h.literal.trim_end().to_owned(),
                        style: Style::default(),
                    }])
                }))
            }
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
            NodeValue::Link(l)
                if n.children()
                    .any(|c| matches!(c.data().value, NodeValue::Image(_))) =>
            {
                // `[![alt](src)](href)`: each child image renders as an image carrying the
                // href; non-empty text children stay a plain link.
                for c in n.children() {
                    if matches!(c.data().value, NodeValue::Image(_)) {
                        let start = out.len();
                        self.inline(c, style, st, out);
                        for p in &mut out[start..] {
                            if let Piece::Block(Block::Image { link, .. }) = p {
                                *link = Some(l.url.clone());
                            }
                        }
                    } else {
                        let label = plain_text(std::iter::once(c));
                        if !label.trim().is_empty() {
                            out.push(Piece::Span(Inline::Link {
                                url: l.url.clone(),
                                label,
                            }));
                        }
                    }
                }
            }
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
                    link: None,
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
        // Plan round-14 WP-3 (audit M2): a file the editor cannot show (pdf, svg, avif, other
        // binaries) embeds as a file card that opens externally. Plan round-15 WP-3 (audit B1):
        // WP-4: a PDF embed is its own block (`PdfEmbed`, the page rendered inline).
        let kind = cv_text::file_kind::by_extension(&target);
        if embed && kind == Some(cv_text::file_kind::FileKind::External) {
            out.push(Piece::Block(Block::FileEmbed { target }));
        } else if embed && kind == Some(cv_text::file_kind::FileKind::Pdf) {
            out.push(Piece::Block(Block::PdfEmbed {
                target,
                page: fragment.as_deref().map_or(1, parse_page),
                width: alias.as_deref().and_then(parse_width).filter(|w| *w > 0),
            }));
        } else if embed && is_image_target(&target) {
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
                link: None,
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

/// Plan round-15 WP-4: the 1-based page of a `page=N` embed fragment (also inside
/// `page=N&…`); a missing, non-numeric or `0` page is 1.
fn parse_page(fragment: &str) -> u32 {
    fragment
        .split('&')
        .find_map(|p| p.trim().strip_prefix("page="))
        .and_then(|n| n.trim().parse::<u32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(1)
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

/// Plan remote-media-sprint WP-5: `<video …></video>` / `<audio …></audio>` on one line is not a
/// CommonMark HTML block (only `<iframe>` is) but a paragraph of inline HTML. A paragraph made
/// only of inline HTML, text and breaks whose joined source is exactly one media element is the
/// same card as the block form.
fn inline_html_media<'a>(n: N<'a>) -> Option<Block> {
    let mut src = String::new();
    let mut html = false;
    for c in n.children() {
        match &c.data().value {
            NodeValue::HtmlInline(h) => {
                html = true;
                src.push_str(h);
            }
            NodeValue::Text(t) => src.push_str(t),
            NodeValue::SoftBreak | NodeValue::LineBreak => src.push('\n'),
            _ => return None,
        }
    }
    if html { html_media_block(&src) } else { None }
}

/// Plan remote-media-sprint WP-5: an HTML block whose trimmed literal is exactly one media
/// element (per `cv_text::scan_html_media`) lowers to a card block; anything else is `None`
/// (raw text). An `<img>` is a remote [`Block::Image`]; `<video>`/`<audio>`/`<iframe>` are a
/// [`Block::RemoteMedia`] by the source's kind, falling back to the tag (an `<iframe>` whose
/// source is not otherwise classified is an embed).
fn html_media_block(literal: &str) -> Option<Block> {
    use super::model::RemoteCardKind;
    use cv_text::HtmlMediaTag as T;
    let t = literal.trim();
    let [m] = cv_text::scan_html_media(t).try_into().ok()?;
    if m.span.start != 0 || m.span.end != t.len() {
        return None;
    }
    if m.tag == T::Img {
        return Some(Block::Image {
            src: m.src,
            alt: String::new(),
            width: None,
            remote: true,
            link: None,
        });
    }
    let kind = RemoteCardKind::from_url(&m.src).unwrap_or(match m.tag {
        T::Video => RemoteCardKind::Video,
        T::Audio => RemoteCardKind::Audio,
        _ => RemoteCardKind::Embed,
    });
    Some(Block::RemoteMedia { url: m.src, kind })
}
