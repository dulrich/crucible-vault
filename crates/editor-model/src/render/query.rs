//! Read-only walks over a [`RenderModel`]: task lines, link targets, images, inline text.

use super::lower::is_remote;
use super::model::{Block, Inline, RenderModel};
use crate::props::PropValue;

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
    for b in blocks {
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
            Block::List { items, .. } => {
                for i in items {
                    collect_links(&i.children, out);
                }
            }
            Block::Callout { children, .. } | Block::BlockQuote(children) => {
                collect_links(children, out);
            }
            Block::Transclusion {
                content: Some(c), ..
            } => collect_links(c, out),
            _ => {}
        }
    }
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

/// Plan round-15 WP-4: every PDF embed's `(target, 1-based page)`, in document order, including
/// inside transclusions.
pub fn pdf_embeds(model: &RenderModel) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    collect_pdf_embeds(&model.blocks, &mut out);
    out
}

fn collect_pdf_embeds(blocks: &[Block], out: &mut Vec<(String, u32)>) {
    for b in blocks {
        match b {
            Block::PdfEmbed { target, page, .. } => out.push((target.clone(), *page)),
            Block::List { items, .. } => {
                for i in items {
                    collect_pdf_embeds(&i.children, out);
                }
            }
            Block::Callout { children, .. } | Block::BlockQuote(children) => {
                collect_pdf_embeds(children, out);
            }
            Block::Transclusion {
                content: Some(c), ..
            } => collect_pdf_embeds(c, out),
            _ => {}
        }
    }
}
