//! Plan round-5 WP-7: the Reading selection's run table and copy text (pure; no iced).
//!
//! [`runs`] walks the render model in exactly the order `view::reading` draws its text runs
//! (properties, blocks, footnotes), so run `i` of the table is the `i`-th
//! [`crate::widget::selectable::Run`] on screen. Images take a run slot too (no widget: they
//! can't be a selection end, but a selection over them copies their alt text).
//!
//! Copy text = visible text: list items carry their marker (`- `, `1. `, `- [ ] `), table cells
//! are tab-separated and rows newline-separated, code blocks are verbatim, images are their alt
//! text, blocks are separated by a blank line.

use editor_model::render::{Block, Fold, Inline, PropValue, Property, RenderModel, inline_text};

use super::{inline_visible, link_label};
use crate::widget::selectable::{SelPos, ordered};

/// What goes between a run and the run before it in the copy text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sep {
    /// A blank line (`\n\n`) — between blocks.
    Block,
    /// A newline — list items, table rows, property rows, paragraphs of one list item.
    Line,
    /// A tab — table cells of one row.
    Cell,
}

impl Sep {
    fn as_str(self) -> &'static str {
        match self {
            Self::Block => "\n\n",
            Self::Line => "\n",
            Self::Cell => "\t",
        }
    }
}

/// One run of the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunInfo {
    /// The run's visible text (byte offsets of the selection index into it).
    pub text: String,
    pub sep: Sep,
    /// Copied before the text when the run is not where the selection starts (list markers,
    /// property keys, footnote numbers).
    pub prefix: String,
    /// An image's alt text (no widget).
    pub image: bool,
}

/// The visible text of an inline run, exactly as `view::inlines_view` builds its spans.
pub fn inlines_text(inlines: &[Inline]) -> String {
    if inlines.is_empty() {
        return inline_text(inlines);
    }
    let mut s = String::new();
    for i in inlines {
        match i {
            Inline::FootnoteRef { ix, .. } => s.push_str(&format!("[{ix}]")),
            Inline::LineBreak => s.push('\n'),
            _ => {
                if let Some(v) = inline_visible(i) {
                    s.push_str(&v);
                }
            }
        }
    }
    s
}

/// A property row's visible values, as `view::properties` builds its spans; `None` for a
/// single-boolean row (drawn as a checkbox, no text run).
pub fn property_text(p: &Property) -> Option<String> {
    if let [PropValue::Bool(_)] = p.values.as_slice() {
        return None;
    }
    let parts: Vec<String> = p
        .values
        .iter()
        .map(|v| match v {
            PropValue::Text(s) => s.clone(),
            PropValue::Bool(b) => b.to_string(),
            PropValue::Url { label, .. } => label.clone(),
            PropValue::Link {
                target,
                fragment,
                alias,
            } => link_label(target, fragment.as_ref(), alias.as_ref()),
        })
        .collect();
    Some(parts.join(", "))
}

/// A callout's visible title (fold marker included), as `view::block` draws it.
pub fn callout_title(kind: &str, title: Option<&String>, fold: Option<Fold>) -> String {
    let marker = match fold {
        Some(Fold::Folded) => "▸ ",
        Some(Fold::Open) => "▾ ",
        None => "",
    };
    let t = title.cloned().unwrap_or_else(|| {
        let mut c = kind.chars();
        c.next()
            .map(|f| f.to_uppercase().chain(c).collect())
            .unwrap_or_default()
    });
    format!("{marker}{t}")
}

/// A transclusion head's visible label.
pub fn transclusion_label(target: &str, fragment: Option<&String>) -> String {
    match fragment {
        Some(f) => format!("↳ {target}#{f}"),
        None => format!("↳ {target}"),
    }
}

struct Walk {
    out: Vec<RunInfo>,
    /// Separator + prefix for the next run (a list item's or footnote's first run).
    pending: Option<(Sep, String)>,
}

impl Walk {
    fn push(&mut self, text: String, sep: Sep, image: bool) {
        let (sep, prefix) = self.pending.take().unwrap_or((sep, String::new()));
        self.out.push(RunInfo {
            text,
            sep,
            prefix,
            image,
        });
    }

    /// `depth` = list nesting (0 = not in a list): runs inside a list are newline-separated.
    fn block(&mut self, b: &Block, depth: usize) {
        let sep = if depth > 0 { Sep::Line } else { Sep::Block };
        match b {
            Block::Heading { inlines, .. } | Block::Paragraph(inlines) => {
                self.push(inlines_text(inlines), sep, false);
            }
            Block::List {
                ordered,
                start,
                items,
            } => {
                for (i, it) in items.iter().enumerate() {
                    let indent = "  ".repeat(depth);
                    let marker = match (it.task, ordered) {
                        (Some(c), _) => format!("{indent}- [{c}] "),
                        (None, true) => format!("{indent}{}. ", start + i),
                        (None, false) => format!("{indent}- "),
                    };
                    // The first list item keeps the separator from before the list.
                    let item_sep = if i == 0 { sep } else { Sep::Line };
                    let pending = self.pending.take().map_or(item_sep, |p| p.0);
                    self.pending = Some((pending, marker));
                    for c in &it.children {
                        self.block(c, depth + 1);
                    }
                    // An item without a text run drops its marker.
                    self.pending = None;
                }
            }
            Block::Table { header, rows, .. } => {
                for (i, c) in header.iter().enumerate() {
                    self.push(inlines_text(c), if i == 0 { sep } else { Sep::Cell }, false);
                }
                for r in rows {
                    for (i, c) in r.iter().enumerate() {
                        let s = if i == 0 { Sep::Line } else { Sep::Cell };
                        self.push(inlines_text(c), s, false);
                    }
                }
            }
            Block::Callout {
                kind,
                title,
                fold,
                children,
            } => {
                self.push(callout_title(kind, title.as_ref(), *fold), sep, false);
                for c in children {
                    self.block(c, depth);
                }
            }
            Block::CodeBlock { text, .. } => self.push(text.clone(), sep, false),
            Block::BlockQuote(children) => {
                for c in children {
                    self.block(c, depth);
                }
            }
            Block::Image { alt, .. } => self.push(alt.clone(), sep, true),
            Block::Transclusion {
                target,
                fragment,
                content,
            } => {
                self.push(transclusion_label(target, fragment.as_ref()), sep, false);
                for c in content.iter().flatten() {
                    self.block(c, depth);
                }
            }
            Block::ThematicBreak | Block::ViewFence { .. } => {}
        }
    }
}

/// The run table of `model`, in render order (see the module docs).
pub fn runs(model: &RenderModel) -> Vec<RunInfo> {
    let mut w = Walk {
        out: Vec::new(),
        pending: None,
    };
    for p in &model.properties {
        if let Some(text) = property_text(p) {
            let first = w.out.is_empty();
            w.out.push(RunInfo {
                text,
                sep: if first { Sep::Block } else { Sep::Line },
                prefix: format!("{}: ", p.key),
                image: false,
            });
        }
    }
    for b in &model.blocks {
        w.block(b, 0);
    }
    for (i, f) in model.footnotes.iter().enumerate() {
        w.pending = Some((Sep::Block, format!("[{}] ", i + 1)));
        for b in &f.blocks {
            w.block(b, 0);
        }
        w.pending = None;
    }
    w.out
}

/// The largest char boundary of `s` at or below `i`.
fn floor(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The copy text of the selection `(anchor, focus)` over `runs` (empty when collapsed).
pub fn copy_text(runs: &[RunInfo], anchor: SelPos, focus: SelPos) -> String {
    let (lo, hi) = ordered(anchor, focus);
    let mut out = String::new();
    if lo == hi || runs.is_empty() {
        return out;
    }
    let last = hi.run.min(runs.len() - 1);
    for (i, r) in runs.iter().enumerate().take(last + 1).skip(lo.run) {
        let a = if i == lo.run {
            floor(&r.text, lo.byte)
        } else {
            0
        };
        let b = if i == hi.run {
            floor(&r.text, hi.byte)
        } else {
            r.text.len()
        };
        if i > lo.run {
            out.push_str(r.sep.as_str());
        }
        if i != lo.run || (a == 0 && hi.run > lo.run) {
            out.push_str(&r.prefix);
        }
        if a < b {
            out.push_str(&r.text[a..b]);
        }
    }
    out
}

/// The whole-document selection: `(start of run 0, end of the last run)`.
pub fn all(runs: &[RunInfo]) -> Option<(SelPos, SelPos)> {
    let last = runs.len().checked_sub(1)?;
    Some((SelPos::new(0, 0), SelPos::new(last, runs[last].text.len())))
}
