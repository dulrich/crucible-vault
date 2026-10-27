//! Read-only walks over a [`RenderModel`]: task lines, link targets, images, inline text.

use super::lower::is_remote;
use super::model::{Block, Inline, ListItem, RenderModel};
use crate::props::PropValue;

/// Every host-note task line with its status, in document order (transclusions excluded).
pub fn task_lines(model: &RenderModel) -> Vec<(usize, char)> {
    let mut out = Vec::new();
    collect_tasks(&model.blocks, &mut out);
    out
}

/// What [`walk_blocks`] hands its visitor.
enum Visit<'a> {
    /// A block, before its children.
    Block(&'a Block),
    /// A list item, before its children.
    Item(&'a ListItem),
}

/// WP-R3 (review E11): the one recursive block walker, in document order. List item children,
/// callout and quote children are always entered; transclusion content only when `transcluded`.
fn walk_blocks<'a>(blocks: &'a [Block], transcluded: bool, visit: &mut impl FnMut(Visit<'a>)) {
    for b in blocks {
        visit(Visit::Block(b));
        match b {
            Block::List { items, .. } => {
                for i in items {
                    visit(Visit::Item(i));
                    walk_blocks(&i.children, transcluded, visit);
                }
            }
            Block::Callout { children, .. } | Block::BlockQuote(children) => {
                walk_blocks(children, transcluded, visit);
            }
            Block::Transclusion {
                content: Some(c), ..
            } if transcluded => walk_blocks(c, transcluded, visit),
            _ => {}
        }
    }
}

fn collect_tasks(blocks: &[Block], out: &mut Vec<(usize, char)>) {
    walk_blocks(blocks, false, &mut |v| {
        if let Visit::Item(i) = v
            && let (Some(l), Some(c)) = (i.line, i.task)
        {
            out.push((l, c));
        }
    });
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

/// Plan round-6 WP-4: every vault link target in render order — `Inline::WikiLink` targets
/// (the fragment is separate; self links `[[#h]]` skipped) and non-remote `Inline::Link` urls / linked-image
/// hrefs, as the Reading view hands them to `OpenLink` — including inside lists, callouts,
/// quotes, tables, transclusions and footnotes. The editor resolves each to mark unresolved
/// links (dimmed, "Create note").
pub fn link_targets(model: &RenderModel) -> Vec<String> {
    let mut out = Vec::new();
    collect_links(&model.blocks, &mut out);
    for f in &model.footnotes {
        collect_links(&f.blocks, &mut out);
    }
    // Plan round-6 WP-5: frontmatter wikilink values are links too (uniform styling).
    for p in &model.properties {
        for v in &p.values {
            if let PropValue::Link { target, .. } = v
                && !self_link(target)
            {
                out.push(target.clone());
            }
        }
    }
    out
}

/// An empty or `#heading`-only target links the note itself (always resolves).
fn self_link(target: &str) -> bool {
    target.is_empty() || target.starts_with('#')
}

fn inline_links(inlines: &[Inline], out: &mut Vec<String>) {
    for i in inlines {
        match i {
            Inline::WikiLink { target, .. } if !self_link(target) => out.push(target.clone()),
            Inline::Link { url, .. } if !is_remote(url) && !self_link(url) => {
                out.push(url.clone());
            }
            _ => {}
        }
    }
}

fn collect_links(blocks: &[Block], out: &mut Vec<String>) {
    walk_blocks(blocks, true, &mut |v| {
        let Visit::Block(b) = v else { return };
        match b {
            Block::Heading { inlines, .. } | Block::Paragraph(inlines) => {
                inline_links(inlines, out);
            }
            Block::Table { header, rows, .. } => {
                for c in header.iter().chain(rows.iter().flatten()) {
                    inline_links(c, out);
                }
            }
            Block::Image {
                link: Some(href), ..
            } if !is_remote(href) && !self_link(href) => out.push(href.clone()),
            _ => {}
        }
    });
}

/// Every image block's `(src, remote)`, in document order, including inside transclusions.
pub fn images(model: &RenderModel) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    collect_images(&model.blocks, &mut out);
    out
}

fn collect_images(blocks: &[Block], out: &mut Vec<(String, bool)>) {
    walk_blocks(blocks, true, &mut |v| {
        if let Visit::Block(Block::Image { src, remote, .. }) = v {
            out.push((src.clone(), *remote));
        }
    });
}

/// Plan round-15 WP-4: every PDF embed's `(target, 1-based page)`, in document order, including
/// inside transclusions.
pub fn pdf_embeds(model: &RenderModel) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    collect_pdf_embeds(&model.blocks, &mut out);
    out
}

fn collect_pdf_embeds(blocks: &[Block], out: &mut Vec<(String, u32)>) {
    walk_blocks(blocks, true, &mut |v| {
        if let Visit::Block(Block::PdfEmbed { target, page, .. }) = v {
            out.push((target.clone(), *page));
        }
    });
}
