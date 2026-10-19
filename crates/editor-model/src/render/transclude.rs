//! Depth-1 transclusion: fills [`Block::Transclusion`] content.

use super::lower::lower_with;
use super::model::{Block, LowerOptions, RenderModel};
use super::query::inline_text;

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
