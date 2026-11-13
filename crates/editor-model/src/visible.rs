//! The Reading view's visible runs: one walk
//! of a [`RenderModel`] in exactly the order the Reading view draws its text runs — properties,
//! blocks, footnotes. The view draws from it, the selection copies from it and find counts over
//! it, so the three can no longer drift apart.
//!
//! Run `i` is the `i`-th selectable run on screen. Images take a run slot too (no widget: they
//! can't be a selection end, but a selection over them copies their alt text).
//!
//! Copy text = visible text: list items carry their marker (`- `, `1. `, `- [ ] `), table cells
//! are tab-separated and rows newline-separated, code blocks are verbatim, images are their alt
//! text, blocks are separated by a blank line.

use std::borrow::Cow;
use std::ops::Range;

use crate::props::{PropValue, Property};
use crate::render::{Block, Fold, Inline, RenderModel};

/// What goes between a run and the run before it in the copy text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sep {
    /// A blank line (`\n\n`) — between blocks.
    Block,
    /// A newline — list items, table rows, property rows, paragraphs of one list item.
    Line,
    /// A tab — table cells of one row.
    Cell,
    /// `: ` — a property value after its key (round-24 WP-2: the key is its own run).
    Colon,
}

impl Sep {
    /// The separator text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "\n\n",
            Self::Line => "\n",
            Self::Cell => "\t",
            Self::Colon => ": ",
        }
    }
}

/// Where a run sits in the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// The frontmatter properties block.
    Properties,
    /// Inside top-level block `i` (nested blocks included).
    Block(usize),
    /// Inside footnote definition `i` (0-based, drawn as `[i + 1]`).
    Footnote(usize),
}

impl Part {
    /// The top-level block, if any.
    pub fn block(self) -> Option<usize> {
        match self {
            Self::Block(b) => Some(b),
            _ => None,
        }
    }
}

/// One visible text run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisRun {
    /// The run's visible text (selection byte offsets index into it).
    pub text: String,
    pub sep: Sep,
    /// Copied before the text when the run is not where the selection starts (list markers,
    /// property keys, footnote numbers).
    pub prefix: String,
    /// An image's alt text (no widget).
    pub image: bool,
    /// The find-painted byte ranges of `text`, in order (inline pieces and code-block text;
    /// footnote markers, line breaks, titles and property values are not searched). A match
    /// never crosses a segment boundary.
    pub segments: Vec<Range<usize>>,
    pub part: Part,
}

/// One find occurrence over the visible runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    /// The run index.
    pub run: usize,
    /// The byte range in the run's text.
    pub range: Range<usize>,
    pub part: Part,
}

/// A wiki-link's displayed label (alias, else `target#fragment`).
pub fn link_label(target: &str, fragment: Option<&String>, alias: Option<&String>) -> String {
    match (alias, fragment) {
        (Some(a), _) => a.clone(),
        (None, Some(f)) if target.is_empty() => format!("#{f}"),
        (None, Some(f)) => format!("{target}#{f}"),
        (None, None) => target.to_owned(),
    }
}

/// plan editor-round-4 WP-3: the visible (painted) text of an inline, if any — text, code
/// spans, wiki-link/link labels and tags. URLs and link targets are never displayed.
pub fn inline_visible(i: &Inline) -> Option<Cow<'_, str>> {
    match i {
        Inline::Text { text, .. } | Inline::Code(text) => Some(Cow::Borrowed(text)),
        Inline::WikiLink {
            target,
            fragment,
            alias,
        } => Some(Cow::Owned(link_label(
            target,
            fragment.as_ref(),
            alias.as_ref(),
        ))),
        Inline::Link { label, .. } => Some(Cow::Borrowed(label)),
        Inline::Tag(t) => Some(Cow::Owned(format!("#{t}"))),
        Inline::FootnoteRef { .. } | Inline::LineBreak => None,
    }
}

/// The visible text of an inline run with its find segments, exactly as the view builds its
/// spans: footnote refs draw `[n]`, line breaks `\n` (neither searched).
pub fn inlines_text(inlines: &[Inline]) -> (String, Vec<Range<usize>>) {
    let mut s = String::new();
    let mut segs = Vec::new();
    for i in inlines {
        match i {
            Inline::FootnoteRef { ix, .. } => s.push_str(&format!("[{ix}]")),
            Inline::LineBreak => s.push('\n'),
            _ => {
                if let Some(v) = inline_visible(i) {
                    let at = s.len();
                    s.push_str(&v);
                    segs.push(at..s.len());
                }
            }
        }
    }
    (s, segs)
}

/// A property row's visible values; `None` for a single-boolean row (drawn as a checkbox, no
/// text run).
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

/// A callout's visible title (fold marker included).
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
    out: Vec<VisRun>,
    /// Separator + prefix for the next run (a list item's or footnote's first run).
    pending: Option<(Sep, String)>,
    part: Part,
}

impl Walk {
    fn push(&mut self, text: String, segments: Vec<Range<usize>>, sep: Sep, image: bool) {
        let (sep, prefix) = self.pending.take().unwrap_or((sep, String::new()));
        self.out.push(VisRun {
            text,
            sep,
            prefix,
            image,
            segments,
            part: self.part,
        });
    }

    fn inlines(&mut self, inlines: &[Inline], sep: Sep) {
        let (text, segs) = inlines_text(inlines);
        self.push(text, segs, sep, false);
    }

    /// `depth` = list nesting (0 = not in a list): runs inside a list are newline-separated.
    fn block(&mut self, b: &Block, depth: usize) {
        let sep = if depth > 0 { Sep::Line } else { Sep::Block };
        match b {
            Block::Heading { inlines, .. } | Block::Paragraph(inlines) => {
                self.inlines(inlines, sep)
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
                    self.inlines(c, if i == 0 { sep } else { Sep::Cell });
                }
                for r in rows {
                    for (i, c) in r.iter().enumerate() {
                        self.inlines(c, if i == 0 { Sep::Line } else { Sep::Cell });
                    }
                }
            }
            Block::Callout {
                kind,
                title,
                fold,
                children,
            } => {
                let head = callout_title(kind, title.as_ref(), *fold);
                self.push(head, Vec::new(), sep, false);
                for c in children {
                    self.block(c, depth);
                }
            }
            Block::CodeBlock { text, .. } => {
                let all = std::iter::once(0..text.len()).collect();
                self.push(text.clone(), all, sep, false);
            }
            Block::BlockQuote(children) => {
                for c in children {
                    self.block(c, depth);
                }
            }
            Block::Image { alt, .. } => self.push(alt.clone(), Vec::new(), sep, true),
            Block::Transclusion {
                target,
                fragment,
                content,
            } => {
                let head = transclusion_label(target, fragment.as_ref());
                self.push(head, Vec::new(), sep, false);
                for c in content.iter().flatten() {
                    self.block(c, depth);
                }
            }
            // The card's file name is its copy text (a non-text slot, as an image's alt).
            Block::FileEmbed { target } | Block::PdfEmbed { target, .. } => {
                let name = target.rsplit('/').next().unwrap_or(target).to_owned();
                self.push(name, Vec::new(), sep, true);
            }
            Block::RemoteMedia { url, .. } => self.push(url.clone(), Vec::new(), sep, true),
            Block::ThematicBreak | Block::ViewFence { .. } => {}
        }
    }
}

/// The visible runs of `model`, in render order (see the module docs).
pub fn visible_runs(model: &RenderModel) -> Vec<VisRun> {
    let mut w = Walk {
        out: Vec::new(),
        pending: None,
        part: Part::Properties,
    };
    for p in &model.properties {
        // round-24 WP-2: the key is its own (selectable) run; the value run follows after `: `.
        // A boolean row (a checkbox) has only the key run.
        let sep = if w.out.is_empty() {
            Sep::Block
        } else {
            Sep::Line
        };
        w.push(p.key.clone(), Vec::new(), sep, false);
        if let Some(text) = property_text(p) {
            w.push(text, Vec::new(), Sep::Colon, false);
        }
    }
    for (i, b) in model.blocks.iter().enumerate() {
        w.part = Part::Block(i);
        w.block(b, 0);
    }
    for (i, f) in model.footnotes.iter().enumerate() {
        w.part = Part::Footnote(i);
        w.pending = Some((Sep::Block, format!("[{}] ", i + 1)));
        for b in &f.blocks {
            w.block(b, 0);
        }
        w.pending = None;
    }
    w.out
}

/// Every case-insensitive occurrence of `q` over the runs' find segments, in render order.
pub fn occurrences(runs: &[VisRun], q: &str) -> Vec<Occurrence> {
    let mut out = Vec::new();
    for (i, r) in runs.iter().enumerate() {
        for s in &r.segments {
            out.extend(
                crate::find::matches(&r.text[s.clone()], q)
                    .into_iter()
                    .map(|m| Occurrence {
                        run: i,
                        range: s.start + m.start..s.start + m.end,
                        part: r.part,
                    }),
            );
        }
    }
    out
}

/// Each occurrence's `(part, nth occurrence inside that part)` key.
pub fn keyed(occ: &[Occurrence]) -> Vec<(Part, usize)> {
    let mut out: Vec<(Part, usize)> = Vec::with_capacity(occ.len());
    for o in occ {
        let n = match out.last() {
            Some(&(p, n)) if p == o.part => n + 1,
            _ => 0,
        };
        out.push((o.part, n));
    }
    out
}

/// The index of the `nth` occurrence inside top-level `block`, if any.
pub fn ordinal(occ: &[Occurrence], block: usize, nth: usize) -> Option<usize> {
    let first = occ.iter().position(|o| o.part == Part::Block(block))?;
    let i = first + nth;
    (occ.get(i)?.part == Part::Block(block)).then_some(i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::lower;

    fn texts(rs: &[VisRun]) -> Vec<&str> {
        rs.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn runs_cover_properties_blocks_and_footnotes_in_order() {
        let m = lower("---\ntags: a\ndone: true\n---\n# Head\n\nbody[^1] x\n\n[^1]: note foo\n");
        let rs = visible_runs(&m);
        assert_eq!(
            texts(&rs),
            vec!["tags", "a", "done", "Head", "body[1] x", "note foo"]
        );
        let parts: Vec<Part> = rs.iter().map(|r| r.part).collect();
        assert_eq!(
            parts,
            vec![
                Part::Properties,
                Part::Properties,
                Part::Properties,
                Part::Block(0),
                Part::Block(1),
                Part::Footnote(0)
            ]
        );
        // round-24 WP-2: the key is its own run, the value follows after `: `.
        assert_eq!(rs[1].sep, Sep::Colon);
        assert_eq!(rs[2].sep, Sep::Line);
        assert_eq!(rs[5].prefix, "[1] ");
        assert_eq!(rs[5].sep, Sep::Block);
        // The footnote ref is visible text but never searched.
        assert_eq!(rs[4].segments, vec![0..4, 7..9]);
    }

    #[test]
    fn property_key_is_its_own_run_and_copies_once() {
        let m = lower("---\ntags: a\n---\nbody\n");
        let rs = visible_runs(&m);
        assert_eq!(texts(&rs), vec!["tags", "a", "body"]);
        // Key start .. value end copies `key: value` once (the `: ` is the value run's sep).
        let copied = format!("{}{}{}", rs[0].text, rs[1].sep.as_str(), rs[1].text);
        assert_eq!(copied, "tags: a");
        assert!(rs[0].prefix.is_empty() && rs[1].prefix.is_empty());
    }

    #[test]
    fn nested_blocks_keep_their_top_level_part() {
        let m = lower("- one\n  - two\n\n> [!note] T\n> inner `c`\n\n```\ncode\n```\n");
        let rs = visible_runs(&m);
        assert_eq!(texts(&rs), vec!["one", "two", "T", "inner c", "code"]);
        let parts: Vec<Option<usize>> = rs.iter().map(|r| r.part.block()).collect();
        assert_eq!(parts, vec![Some(0), Some(0), Some(1), Some(1), Some(2)]);
        assert_eq!(rs[1].prefix, "  - ");
        assert_eq!(rs[1].sep, Sep::Line);
        assert!(rs[2].segments.is_empty(), "callout titles are not searched");
        assert_eq!(rs[3].segments, vec![0..6, 6..7]);
        assert_eq!(rs[4].segments, vec![0..4]);
    }

    #[test]
    fn occurrences_include_footnotes_and_key_per_part() {
        let m = lower("foo foo\n\nx[^1]\n\n[^1]: a foo\n");
        let rs = visible_runs(&m);
        let occ = occurrences(&rs, "foo");
        assert_eq!(occ.len(), 3);
        assert_eq!(occ[2].part, Part::Footnote(0));
        assert_eq!(occ[2].range, 2..5);
        assert_eq!(
            keyed(&occ),
            vec![
                (Part::Block(0), 0),
                (Part::Block(0), 1),
                (Part::Footnote(0), 0)
            ]
        );
        assert_eq!(ordinal(&occ, 0, 1), Some(1));
        assert_eq!(ordinal(&occ, 0, 2), None);
        assert_eq!(ordinal(&occ, 1, 0), None);
    }
}
