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

/// Round-25 WP-3: one typographic display substitution in a run — the display byte range of
/// the glyph and the source (ASCII) byte range it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sub {
    pub disp: Range<usize>,
    pub src: Range<usize>,
}

/// One visible text run.
///
/// Round-25 WP-3 coordinate domain: `text`, `segments`, [`Occurrence::range`] and every
/// selection byte offset are **display** bytes (what the run widget shapes and hit-tests). The
/// source ASCII is [`VisRun::src`]; [`VisRun::to_source`] and [`VisRun::to_display`] translate
/// (copy and find, nowhere else).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisRun {
    /// The run's displayed text (selection byte offsets index into it).
    pub text: String,
    /// The source text when it differs from `text` (empty when `subs` is empty).
    pub source: String,
    /// The display substitutions, in order (empty: `text` is the source).
    pub subs: Vec<Sub>,
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

impl VisRun {
    /// The run's source text (what copy and find see).
    pub fn src(&self) -> &str {
        if self.subs.is_empty() {
            &self.text
        } else {
            &self.source
        }
    }

    /// Display byte `d` → source byte (a `d` inside a glyph floors to its source start).
    pub fn to_source(&self, d: usize) -> usize {
        let mut out = d;
        for s in &self.subs {
            if d >= s.disp.end {
                out = d - s.disp.end + s.src.end;
            } else if d > s.disp.start {
                return s.src.start;
            } else {
                break;
            }
        }
        out
    }

    /// Source byte `b` → display byte; inside a substituted sequence it rounds to the glyph's
    /// end when `up`, else its start (so a partial match paints the whole glyph).
    pub fn to_display(&self, b: usize, up: bool) -> usize {
        let mut out = b;
        for s in &self.subs {
            if b >= s.src.end {
                out = b - s.src.end + s.disp.end;
            } else if b > s.src.start {
                return if up { s.disp.end } else { s.disp.start };
            } else {
                break;
            }
        }
        out
    }
}

/// Round-25 WP-3: the Reading display form of prose `s` — `-->` `⟶`, `->` `→`, `---` `—`,
/// `--` `–`, `!=` `≠`, `>=` `≥`, `<=` `≤`, `...` `…`, longest first — and its substitutions
/// (piece-relative ranges). `<->`, `<--`, `<=>` stay (Inter ligates them,
/// `docs/spikes/inter-features/WP-0-FINDINGS.md`); URL tokens (`://`, `www.`) and `$inline
/// math$` are untouched.
pub fn substitute(s: &str) -> (Cow<'_, str>, Vec<Sub>) {
    const KEEP: [&str; 3] = ["<->", "<--", "<=>"];
    const TABLE: [(&str, &str); 8] = [
        ("-->", "⟶"),
        ("---", "—"),
        ("...", "…"),
        ("->", "→"),
        ("--", "–"),
        ("!=", "≠"),
        (">=", "≥"),
        ("<=", "≤"),
    ];
    if !s.contains(['-', '!', '>', '<', '.']) {
        return (Cow::Borrowed(s), Vec::new());
    }
    let skip = skip_ranges(s);
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut subs = Vec::new();
    let mut i = 0;
    'outer: while i < s.len() {
        if let Some(r) = skip.iter().find(|r| r.start == i) {
            out.push_str(&s[r.clone()]);
            i = r.end;
            continue;
        }
        let rest = &s[i..];
        if let Some(k) = KEEP.iter().find(|k| rest.starts_with(**k)) {
            out.push_str(k);
            i += k.len();
            continue;
        }
        for (from, to) in TABLE {
            if rest.starts_with(from) {
                let at = out.len();
                out.push_str(to);
                subs.push(Sub {
                    disp: at..out.len(),
                    src: i..i + from.len(),
                });
                i += from.len();
                continue 'outer;
            }
        }
        let n = if b[i].is_ascii() {
            1
        } else {
            rest.chars().next().map_or(1, char::len_utf8)
        };
        out.push_str(&s[i..i + n]);
        i += n;
    }
    if subs.is_empty() {
        (Cow::Borrowed(s), subs)
    } else {
        (Cow::Owned(out), subs)
    }
}

/// The byte ranges of `s` never substituted: URL tokens and `$inline math$`.
fn skip_ranges(s: &str) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut at = 0;
    for tok in s.split(char::is_whitespace) {
        if tok.contains("://") || tok.starts_with("www.") {
            out.push(at..at + tok.len());
        }
        // `split` on a char predicate: the separator is one char wide.
        at += tok.len();
        at += s[at..].chars().next().map_or(0, char::len_utf8);
    }
    let b = s.as_bytes();
    let mut i = 0;
    while let Some(o) = s[i..].find('$').map(|o| o + i) {
        let opens = b
            .get(o + 1)
            .is_some_and(|c| !c.is_ascii_whitespace() && *c != b'$');
        let close = opens
            .then(|| s[o + 1..].find('$').map(|c| c + o + 1))
            .flatten()
            .filter(|&c| !b[c - 1].is_ascii_whitespace());
        match close {
            Some(c) => {
                out.push(o..c + 1);
                i = c + 1;
            }
            None => i = o + 1,
        }
    }
    out.sort_by_key(|r| r.start);
    // Merge overlaps so each skip starts at a position the scan reaches.
    let mut merged: Vec<Range<usize>> = Vec::new();
    for r in out {
        match merged.last_mut() {
            Some(l) if r.start <= l.end => l.end = l.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
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
/// text and segments are display bytes; also returns the source text and the run's
/// substitutions (each inside one piece — never across a styled boundary).
pub fn inlines_text(inlines: &[Inline]) -> (String, Vec<Range<usize>>, String, Vec<Sub>) {
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
    if subs.is_empty() {
        src.clear();
    }
    (s, segs, src, subs)
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
            source: String::new(),
            subs: Vec::new(),
            sep,
            prefix,
            image,
            segments,
            part: self.part,
        });
    }

    fn inlines(&mut self, inlines: &[Inline], sep: Sep) {
        let (text, segs, source, subs) = inlines_text(inlines);
        self.push(text, segs, sep, false);
        if let Some(r) = self.out.last_mut() {
            r.source = source;
            r.subs = subs;
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

    fn disp(s: &str) -> String {
        substitute(s).0.into_owned()
    }

    #[test]
    fn substitution_table_longest_first_with_exclusions() {
        assert_eq!(disp("a -> b --> c"), "a → b ⟶ c");
        assert_eq!(disp("x -- y --- z"), "x – y — z");
        assert_eq!(disp("a != b >= c <= d..."), "a ≠ b ≥ c ≤ d…");
        assert_eq!(disp("---->"), "—→");
        // Inter ligates these itself.
        assert_eq!(
            disp("a <-> b <-- c <=> d <- e => f"),
            "a <-> b <-- c <=> d <- e => f"
        );
        // URLs and inline math are untouched.
        assert_eq!(
            disp("see https://x.y/a--b...c -> www.q.r/--> $a<=b$ ok"),
            "see https://x.y/a--b...c → www.q.r/--> $a<=b$ ok"
        );
        assert!(substitute("plain").1.is_empty());
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
}
