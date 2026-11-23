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
use std::collections::BTreeSet;
use std::ops::Range;

use crate::props::{PropValue, Property};
use crate::render::{Block, Cell, CellPart, Fold, Inline, RenderModel};
use crate::typography::{Display, Sub, substitute};

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
    /// A space — between the parts of one table cell (plan round-25 WP-2: text around an image).
    Space,
}

impl Sep {
    /// The separator text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "\n\n",
            Self::Line => "\n",
            Self::Cell => "\t",
            Self::Colon => ": ",
            Self::Space => " ",
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

/// One visible text run; it reads as its [`Display`] (`text`, `subs`, `src()`, `to_source()`,
/// `to_display()`).
///
/// Round-25 WP-3 coordinate domain: `text`, `segments`, [`Occurrence::range`] and every
/// selection byte offset are **display** bytes (what the run widget shapes and hit-tests). The
/// source ASCII is [`Display::src`]; [`Display::to_source`] and [`Display::to_display`] translate
/// (copy and find, nowhere else).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisRun {
    pub(crate) display: Display,
    pub sep: Sep,
    /// Copied before the text when the run is not where the selection starts (list markers,
    /// footnote numbers; a property key is its own run and carries none).
    pub prefix: String,
    /// An image's alt text (no widget).
    pub image: bool,
    /// The find-painted byte ranges of `text`, in order (inline pieces and code-block text;
    /// footnote markers, line breaks, titles and property values are not searched). A match
    /// never crosses a segment boundary.
    pub segments: Vec<Range<usize>>,
    pub part: Part,
}

impl std::ops::Deref for VisRun {
    type Target = Display;

    fn deref(&self) -> &Display {
        &self.display
    }
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

/// Round-25 WP-3: an inline's Reading display text and substitutions — prose pieces (text,
/// wiki-link and link labels) are substituted; code spans, tags and a bare URL label are not.
pub fn inline_display(i: &Inline) -> Option<(Cow<'_, str>, Vec<Sub>)> {
    let v = inline_visible(i)?;
    let prose = match i {
        Inline::Text { .. } | Inline::WikiLink { .. } => true,
        Inline::Link { url, label } => url != label,
        _ => false,
    };
    if !prose {
        return Some((v, Vec::new()));
    }
    Some(match v {
        Cow::Borrowed(s) => substitute(s),
        Cow::Owned(s) => {
            let (d, subs) = substitute(&s);
            let d = d.into_owned();
            (Cow::Owned(d), subs)
        }
    })
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
/// spans: footnote refs draw `[n]`, line breaks `\n` (neither searched). Round-25 WP-3: the
/// text and segments are display bytes; the display also carries the source text and the run's
/// substitutions (each inside one piece — never across a styled boundary).
pub fn inlines_text(inlines: &[Inline]) -> Inlined {
    let mut s = String::new();
    let mut src = String::new();
    let mut segs = Vec::new();
    let mut subs = Vec::new();
    for i in inlines {
        match i {
            Inline::FootnoteRef { ix, .. } => {
                let f = format!("[{ix}]");
                s.push_str(&f);
                src.push_str(&f);
            }
            Inline::LineBreak => {
                s.push('\n');
                src.push('\n');
            }
            _ => {
                if let Some((v, piece)) = inline_display(i) {
                    let at = s.len();
                    let src_at = src.len();
                    s.push_str(&v);
                    src.push_str(&inline_visible(i).unwrap_or_default());
                    subs.extend(piece.into_iter().map(|p| Sub {
                        disp: at + p.disp.start..at + p.disp.end,
                        src: src_at + p.src.start..src_at + p.src.end,
                    }));
                    segs.push(at..s.len());
                }
            }
        }
    }
    Inlined {
        display: Display::new(s, src, subs),
        segments: segs,
    }
}

/// A run's display text and the find segments inside it.
pub struct Inlined {
    pub display: Display,
    pub segments: Vec<Range<usize>>,
}

impl Inlined {
    /// Text with the given find segments and no substitutions.
    fn plain(text: String, segments: Vec<Range<usize>>) -> Self {
        Self {
            display: Display::plain(text),
            segments,
        }
    }
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

/// Plan abstract-removal-confidence-deletion WP-5: callouts are numbered in pre-order over the
/// blocks then the footnotes (a callout before its children, children in order, transcluded
/// content included). A callout's fold state is its marker (`-` collapsed, `+` open, plain not
/// foldable) with the ids in a `flipped` set inverted; the walker and the Reading view both
/// number through this one scheme, so they skip exactly the same children.
#[must_use]
pub fn is_collapsed(fold: Option<Fold>, id: usize, flipped: &BTreeSet<usize>) -> bool {
    match fold {
        Some(f) => (f == Fold::Folded) != flipped.contains(&id),
        None => false,
    }
}

fn collect_folds(blocks: &[Block], out: &mut Vec<Option<Fold>>) {
    for b in blocks {
        match b {
            Block::Callout { fold, children, .. } => {
                out.push(*fold);
                collect_folds(children, out);
            }
            Block::BlockQuote(c) => collect_folds(c, out),
            Block::List { items, .. } => {
                items.iter().for_each(|it| collect_folds(&it.children, out))
            }
            Block::Transclusion {
                content: Some(c), ..
            } => collect_folds(c, out),
            _ => {}
        }
    }
}

/// How many callouts `blocks` hold (nested and transcluded ones included): the ids a skipped
/// subtree consumes.
#[must_use]
pub fn count_callouts(blocks: &[Block]) -> usize {
    let mut v = Vec::new();
    collect_folds(blocks, &mut v);
    v.len()
}

/// The fold marker of every callout of `model` in id order (`None` = plain, not foldable).
#[must_use]
pub fn callout_folds(model: &RenderModel) -> Vec<Option<Fold>> {
    let mut v = Vec::new();
    collect_folds(&model.blocks, &mut v);
    for f in &model.footnotes {
        collect_folds(&f.blocks, &mut v);
    }
    v
}

struct Walk<'f> {
    flipped: &'f BTreeSet<usize>,
    /// The id of the next callout (see [`is_collapsed`]).
    next_callout: usize,
    out: Vec<VisRun>,
    /// Separator + prefix for the next run (a list item's or footnote's first run).
    pending: Option<(Sep, String)>,
    part: Part,
}

impl Walk<'_> {
    fn push(&mut self, run: Inlined, sep: Sep, image: bool) {
        let (sep, prefix) = self.pending.take().unwrap_or((sep, String::new()));
        self.out.push(VisRun {
            display: run.display,
            sep,
            prefix,
            image,
            segments: run.segments,
            part: self.part,
        });
    }

    /// A run of `text` with find segments `segments` and no substitutions.
    fn push_text(&mut self, text: String, segments: Vec<Range<usize>>, sep: Sep, image: bool) {
        self.push(Inlined::plain(text, segments), sep, image);
    }

    fn inlines(&mut self, inlines: &[Inline], sep: Sep) {
        self.push(inlines_text(inlines), sep, false);
    }

    /// Plan round-25 WP-2: a table cell — one run per part (an image's alt text is its copy
    /// text, as a top-level image); an empty cell is one empty run. Parts after the first are
    /// space-separated.
    fn cell(&mut self, c: &Cell, sep: Sep) {
        if c.is_empty() {
            self.inlines(&[], sep);
        }
        for (i, part) in c.iter().enumerate() {
            let sep = if i == 0 { sep } else { Sep::Space };
            match part {
                CellPart::Inlines(inlines) => self.inlines(inlines, sep),
                CellPart::Image { alt, .. } => self.push_text(alt.clone(), Vec::new(), sep, true),
            }
        }
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
                    self.cell(c, if i == 0 { sep } else { Sep::Cell });
                }
                for r in rows {
                    for (i, c) in r.iter().enumerate() {
                        self.cell(c, if i == 0 { Sep::Line } else { Sep::Cell });
                    }
                }
            }
            Block::Callout {
                kind,
                title,
                fold,
                children,
            } => {
                let id = self.next_callout;
                self.next_callout += 1;
                let collapsed = is_collapsed(*fold, id, self.flipped);
                let shown = fold.map(|_| if collapsed { Fold::Folded } else { Fold::Open });
                let head = callout_title(kind, title.as_ref(), shown);
                self.push_text(head, Vec::new(), sep, false);
                if collapsed {
                    // The children give no runs; their callouts still consume ids.
                    self.next_callout += count_callouts(children);
                } else {
                    for c in children {
                        self.block(c, depth);
                    }
                }
            }
            Block::CodeBlock { text, .. } => {
                let all = std::iter::once(0..text.len()).collect();
                self.push_text(text.clone(), all, sep, false);
            }
            Block::BlockQuote(children) => {
                for c in children {
                    self.block(c, depth);
                }
            }
            Block::Image { alt, .. } => self.push_text(alt.clone(), Vec::new(), sep, true),
            Block::Transclusion {
                target,
                fragment,
                content,
            } => {
                let head = transclusion_label(target, fragment.as_ref());
                self.push_text(head, Vec::new(), sep, false);
                for c in content.iter().flatten() {
                    self.block(c, depth);
                }
            }
            // The card's file name is its copy text (a non-text slot, as an image's alt).
            Block::FileEmbed { target } | Block::PdfEmbed { target, .. } => {
                let name = target.rsplit('/').next().unwrap_or(target).to_owned();
                self.push_text(name, Vec::new(), sep, true);
            }
            Block::RemoteMedia { url, .. } => self.push_text(url.clone(), Vec::new(), sep, true),
            Block::ThematicBreak | Block::ViewFence { .. } => {}
        }
    }
}

/// The visible runs of `model`, in render order (see the module docs).
pub fn visible_runs(model: &RenderModel) -> Vec<VisRun> {
    visible_runs_with(model, &BTreeSet::new())
}

/// [`visible_runs`] with the callout fold state `flipped` (ids inverted from their markers).
pub fn visible_runs_with(model: &RenderModel, flipped: &BTreeSet<usize>) -> Vec<VisRun> {
    let mut w = Walk {
        flipped,
        next_callout: 0,
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
        w.push_text(p.key.clone(), Vec::new(), sep, false);
        if let Some(text) = property_text(p) {
            w.push_text(text, Vec::new(), Sep::Colon, false);
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
///
/// Round-25 WP-3: the ASCII query matches the source text of each segment; the ranges are
/// display bytes, widened to whole glyphs (a query covering part of `->` paints the `→`).
/// Matches that widen onto the same glyph merge into one occurrence.
pub fn occurrences(runs: &[VisRun], q: &str) -> Vec<Occurrence> {
    let mut out: Vec<Occurrence> = Vec::new();
    for (i, r) in runs.iter().enumerate() {
        let src = r.src();
        for s in &r.segments {
            let (a, b) = (r.to_source(s.start), r.to_source(s.end));
            for m in crate::find::matches(&src[a..b], q) {
                let range = r.to_display(a + m.start, false)..r.to_display(a + m.end, true);
                match out.last_mut() {
                    Some(o) if o.run == i && range.start < o.range.end => {
                        o.range.end = o.range.end.max(range.end);
                    }
                    _ => out.push(Occurrence {
                        run: i,
                        range,
                        part: r.part,
                    }),
                }
            }
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

    /// Plan round-25 WP-2: an image in a table cell is its own image run carrying its alt text
    /// (the copy text), space-separated from the cell's text; text-only cells are unchanged.
    #[test]
    fn table_cell_image_copies_as_alt_text() {
        let m = lower("| a | b |\n| - | - |\n| x ![shot](s.png) y |  |\n");
        let rs = visible_runs(&m);
        assert_eq!(texts(&rs), ["a", "b", "x ", "shot", " y", ""]);
        let seps: Vec<Sep> = rs.iter().map(|r| r.sep).collect();
        assert_eq!(
            seps,
            [
                Sep::Block,
                Sep::Cell,
                Sep::Line,
                Sep::Space,
                Sep::Space,
                Sep::Cell
            ]
        );
        assert_eq!(
            rs.iter().map(|r| r.image).collect::<Vec<_>>(),
            [false, false, false, true, false, false]
        );
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
    fn code_links_and_pieces_keep_their_ascii() {
        let m = lower(
            "a->b `c->d` [x->y](https://e.x/a->b) <https://e.x/p--q> **m-**>n\n\n```\nk -> l\n```\n",
        );
        let rs = visible_runs(&m);
        assert_eq!(rs[0].text, "a→b c->d x→y https://e.x/p--q m->n");
        assert_eq!(rs[0].src(), "a->b c->d x->y https://e.x/p--q m->n");
        assert_eq!(rs[1].text, "k -> l");
        assert!(rs[1].subs.is_empty());
    }

    #[test]
    fn find_matches_ascii_and_paints_whole_glyphs() {
        let m = lower("a -> b --> c\n");
        let rs = visible_runs(&m);
        assert_eq!(rs[0].text, "a → b ⟶ c");
        let arrow = "a ".len().."a →".len();
        let long = "a → b ".len().."a → b ⟶".len();
        let r = |q: &str| -> Vec<Range<usize>> {
            occurrences(&rs, q).into_iter().map(|o| o.range).collect()
        };
        assert_eq!(r("->"), vec![arrow.clone(), long.clone()]);
        assert_eq!(r("-->"), vec![long.clone()]);
        // Partial: `-` covers part of `->` (one glyph) and twice part of `-->` (merged).
        assert_eq!(r("-"), vec![arrow.clone(), long.clone()]);
        assert_eq!(r("> b"), vec![arrow.start.."a → b".len()]);
        assert!(r("→").is_empty(), "find speaks the ASCII source");
        // Display ↔ source offsets.
        assert_eq!(rs[0].to_source(arrow.end), "a ->".len());
        assert_eq!(rs[0].to_source(rs[0].text.len()), rs[0].src().len());
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

    /// WP-5: a collapsed callout yields its title run and no child run or find match; the run
    /// after it is unchanged; a flip brings the children back (and the marker follows).
    #[test]
    fn collapsed_callout_yields_title_only() {
        let m = lower("> [!note]- Folded\n> hidden zeta\n\nafter zeta\n");
        let rs = visible_runs(&m);
        assert_eq!(texts(&rs), vec!["▸ Folded", "after zeta"]);
        let occ = occurrences(&rs, "zeta");
        assert_eq!(occ.len(), 1);
        assert_eq!(rs[occ[0].run].text, "after zeta");
        let open = visible_runs_with(&m, &BTreeSet::from([0]));
        assert_eq!(texts(&open), vec!["▾ Folded", "hidden zeta", "after zeta"]);
        let occ = occurrences(&open, "zeta");
        assert_eq!(occ.len(), 2);
        assert_eq!(open[occ[1].run].text, "after zeta");
    }

    /// WP-5: `+` starts open and collapses on a flip; a plain callout ignores a flip.
    #[test]
    fn plus_callout_collapses_and_plain_ignores_flip() {
        let m = lower("> [!tip]+ Open\n> body\n\n> [!note] Plain\n> plain body\n");
        assert_eq!(callout_folds(&m), vec![Some(Fold::Open), None]);
        let none = BTreeSet::new();
        assert_eq!(
            texts(&visible_runs_with(&m, &none)),
            vec!["▾ Open", "body", "Plain", "plain body"]
        );
        let flip = BTreeSet::from([0, 1]);
        assert_eq!(
            texts(&visible_runs_with(&m, &flip)),
            vec!["▸ Open", "Plain", "plain body"]
        );
    }

    /// WP-5: a collapsed parent hides an open child whatever the child's state, and the ids of
    /// a skipped subtree stay consumed (the callout after it is still id 2).
    #[test]
    fn collapsed_parent_hides_open_child_and_keeps_ids() {
        let m =
            lower("> [!note]- Parent\n> > [!tip]+ Child\n> > inner\n\n> [!note]+ Sibling\n> sib\n");
        assert_eq!(
            callout_folds(&m),
            vec![Some(Fold::Folded), Some(Fold::Open), Some(Fold::Open)]
        );
        let t = |f: &[usize]| {
            visible_runs_with(&m, &f.iter().copied().collect())
                .into_iter()
                .map(|r| r.text.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(t(&[]), vec!["▸ Parent", "▾ Sibling", "sib"]);
        // Flipping the child (open -> collapsed) changes nothing while the parent is collapsed.
        assert_eq!(t(&[1]), vec!["▸ Parent", "▾ Sibling", "sib"]);
        assert_eq!(
            t(&[0]),
            vec!["▾ Parent", "▾ Child", "inner", "▾ Sibling", "sib"]
        );
        assert_eq!(t(&[0, 1]), vec!["▾ Parent", "▸ Child", "▾ Sibling", "sib"]);
        assert_eq!(t(&[2]), vec!["▸ Parent", "▸ Sibling"]);
    }
}
