//! Selectable rich text for the Reading view (plan round-5 WP-7), grown from the spike widget
//! `docs/spikes/reading-select/src/selectable.rs` (verdict GO with caveat).
//!
//! Two widgets:
//! - [`Run`] — one text run (a paragraph, heading, table cell, code block, property row,
//!   callout title or transclusion head): styled spans, find highlights, link hover underline,
//!   and the selected byte range painted under the text. It owns no gesture logic; it exposes
//!   its shaped state to the root through `Operation::custom`.
//! - [`Root`] — wraps the Reading scrollable and owns every gesture: press/drag (through gaps
//!   between runs and past the last one), auto-scroll at the viewport edge, double-click word,
//!   triple-click run, Shift+click, Shift+Arrows, Esc, and link click vs drag (4 px threshold,
//!   the spike's). The selection itself lives on the Reading tab; the root reads it and
//!   publishes changes.
//!
//! **Hard newlines (audit E3):** iced's `hit_test` byte offsets are per hard line
//! (`iced_graphics-0.14.0/src/text/paragraph.rs:262-265`), so a run whose text contains `\n`
//! (code blocks, paragraphs with line breaks) is shaped as one newline-free paragraph per hard
//! line; each keeps its byte offset into the run text, so every position the widget reports is a
//! byte offset into the run's full text (= its copy text).
//!
//! **Caveat (spike):** the `Paragraph` trait exposes no visual-line byte ranges; they are
//! recovered by probing `hit_test` at each line's left and right edge ([`line_ranges`]).

use std::any::Any;
use std::borrow::Cow;
use std::marker::PhantomData;
use std::time::{Duration, Instant};

use iced::advanced::text::{self, Paragraph as _};
use iced::advanced::widget::{Operation, Tree, operation, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::text::Span;
use iced::{Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Size, Vector, window};
use unicode_segmentation::UnicodeSegmentation;

/// A position in the Reading selection: run index (render order) and byte offset in its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct SelPos {
    pub run: usize,
    pub byte: usize,
}

impl SelPos {
    pub const fn new(run: usize, byte: usize) -> Self {
        Self { run, byte }
    }
}

/// A selection as `(anchor, focus)`.
pub type Selection = (SelPos, SelPos);

/// `(anchor, focus)` in document order.
pub fn ordered(anchor: SelPos, focus: SelPos) -> (SelPos, SelPos) {
    if anchor <= focus {
        (anchor, focus)
    } else {
        (focus, anchor)
    }
}

/// The byte range of run `run` (text length `len`) inside the selection, if any part of it is.
pub fn run_range(sel: Option<(SelPos, SelPos)>, run: usize, len: usize) -> Option<(usize, usize)> {
    let (lo, hi) = ordered(sel?.0, sel?.1);
    if lo == hi || run < lo.run || run > hi.run {
        return None;
    }
    let a = if run == lo.run { lo.byte.min(len) } else { 0 };
    let b = if run == hi.run { hi.byte.min(len) } else { len };
    (a < b).then_some((a, b))
}

/// The word (unicode word bounds) around `byte` in `text`.
pub fn word_at(text: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(text.len());
    let mut last = (text.len(), text.len());
    for (i, w) in text.split_word_bound_indices() {
        let end = i + w.len();
        if byte < end {
            // Whitespace runs are words of their own; prefer the word before a trailing space.
            if w.chars().all(char::is_whitespace) && byte == i && i > 0 {
                return last;
            }
            return (i, end);
        }
        last = (i, end);
    }
    last
}

/// The start of the grapheme before `byte` (0 at the start).
fn prev_grapheme(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(i, _)| i)
}

/// The end of the grapheme at `byte` (`text.len()` at the end).
fn next_grapheme(text: &str, byte: usize) -> usize {
    let byte = byte.min(text.len());
    text[byte..]
        .graphemes(true)
        .next()
        .map_or(text.len(), |g| byte + g.len())
}

/// Hard-line start offsets of `text` (always starts with 0).
pub fn hard_lines(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

/// Splits `spans` at hard newlines: one newline-free span list per hard line, each with its
/// byte offset into the concatenated text (audit E3).
fn split_lines<'a, L: Clone>(spans: &[Span<'a, L>]) -> Vec<(usize, Vec<Span<'a, L>>)> {
    let mut lines: Vec<(usize, Vec<Span<'a, L>>)> = vec![(0, Vec::new())];
    let mut at = 0;
    for s in spans {
        let t: &str = s.text.as_ref();
        let mut from = 0;
        for (k, piece) in t.split('\n').enumerate() {
            if k > 0 {
                lines.push((at + from, Vec::new()));
            }
            if !piece.is_empty() {
                let mut c = s.clone();
                c.text = match &s.text {
                    Cow::Borrowed(b) => Cow::Borrowed(&b[from..from + piece.len()]),
                    Cow::Owned(o) => Cow::Owned(o[from..from + piece.len()].to_owned()),
                };
                if let Some(l) = lines.last_mut() {
                    l.1.push(c);
                }
            }
            from += piece.len() + 1;
        }
        at += t.len();
    }
    lines
}

/// Visual-line byte ranges (line-local), recovered through the `Paragraph` trait only: probe
/// `hit_test` at the left and right edge of every line (spike caveat).
pub fn line_ranges<P: text::Paragraph>(p: &P, lh: f32) -> Vec<(usize, usize)> {
    let h = p.min_bounds().height;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = (h / lh).round().max(1.0) as usize;
    (0..n)
        .map(|k| {
            #[allow(clippy::cast_precision_loss)]
            let y = (k as f32 + 0.5) * lh;
            let s = p.hit_test(Point::new(-1.0, y)).map_or(0, |h| h.cursor());
            let e = p.hit_test(Point::new(1.0e6, y)).map_or(s, |h| h.cursor());
            (s, e.max(s))
        })
        .collect()
}

/// The x of byte `byte` on visual line `k` (starting at `ls`) of `p` (text `text`).
fn x_at<P: text::Paragraph>(p: &P, text: &str, k: usize, ls: usize, byte: usize) -> f32 {
    let (ls, byte) = (ls.min(text.len()), byte.min(text.len()));
    let g = text.get(ls..byte).map_or(0, |s| s.graphemes(false).count());
    p.grapheme_position(k, g).map_or(0.0, |pt| pt.x)
}

/// Selection rectangles (paragraph-local) for the line-local byte range `[a, b)`.
fn selection_rects<P: text::Paragraph>(
    p: &P,
    text: &str,
    lh: f32,
    a: usize,
    b: usize,
) -> Vec<Rectangle> {
    let mut out = Vec::new();
    for (k, (ls, le)) in line_ranges(p, lh).into_iter().enumerate() {
        let (s, e) = (a.max(ls), b.min(le));
        if s >= e {
            continue;
        }
        let x0 = x_at(p, text, k, ls, s);
        let x1 = x_at(p, text, k, ls, e);
        #[allow(clippy::cast_precision_loss)]
        let y = k as f32 * lh;
        out.push(Rectangle::new(
            Point::new(x0, y),
            Size::new((x1 - x0).max(1.0), lh),
        ));
    }
    out
}

/// One shaped hard line of a run.
struct Line<P, L> {
    /// Byte offset of the line in the run text.
    start: usize,
    text: String,
    spans: Vec<Span<'static, L>>,
    paragraph: P,
    /// Top, relative to the run.
    y: f32,
    height: f32,
}

/// A run's shaped state; the root reaches it through `Operation::custom`.
pub struct RunState<P, L> {
    run: usize,
    text: String,
    lines: Vec<Line<P, L>>,
    lh: f32,
    width: f32,
    /// The hovered link span as (line, span).
    hovered: Option<(usize, usize)>,
}

impl<P: text::Paragraph, L: Clone> RunState<P, L> {
    /// The run index.
    pub fn run(&self) -> usize {
        self.run
    }

    /// The run's full text (hard newlines included).
    pub fn text(&self) -> &str {
        &self.text
    }

    fn line_at(&self, y: f32) -> usize {
        self.lines
            .iter()
            .rposition(|l| l.y <= y)
            .unwrap_or(0)
            .min(self.lines.len().saturating_sub(1))
    }

    /// The byte under run-local point `p` (clamped into the run) and the link span there.
    pub fn hit(&self, p: Point) -> (usize, Option<L>) {
        let Some(last) = self.lines.last() else {
            return (0, None);
        };
        let total = last.y + last.height;
        let y = p.y.clamp(0.0, (total - 1.0).max(0.0));
        let i = self.line_at(y);
        let line = &self.lines[i];
        let local = Point::new(
            p.x.clamp(-1.0, self.width + 1.0),
            (y - line.y).clamp(0.0, (line.height - 1.0).max(0.0)),
        );
        let byte = line
            .paragraph
            .hit_test(local)
            .map_or(0, |h| h.cursor())
            .min(line.text.len());
        let link = line
            .paragraph
            .hit_span(local)
            .and_then(|s| line.spans.get(s)?.link.clone());
        (line.start + byte, link)
    }

    /// Run-local centre-left point of the caret at `byte`.
    pub fn caret(&self, byte: usize) -> Point {
        let i = self
            .lines
            .iter()
            .rposition(|l| l.start <= byte)
            .unwrap_or(0);
        let Some(line) = self.lines.get(i) else {
            return Point::ORIGIN;
        };
        let local = byte.saturating_sub(line.start).min(line.text.len());
        let ranges = line_ranges(&line.paragraph, self.lh);
        let k = ranges.iter().rposition(|(s, _)| *s <= local).unwrap_or(0);
        let ls = ranges.get(k).map_or(0, |r| r.0);
        let x = x_at(&line.paragraph, &line.text, k, ls, local);
        #[allow(clippy::cast_precision_loss)]
        let y = line.y + (k as f32 + 0.5) * self.lh;
        Point::new(x, y)
    }
}

/// One selectable text run. See the module docs.
pub struct Run<'a, L> {
    run: usize,
    spans: Vec<Span<'a, L>>,
    size: Pixels,
    /// The selected byte range of this run's text (already clamped).
    selection: Option<(usize, usize)>,
    sel_color: Color,
}

/// A run widget: `run` = its index in render order; `selection` its selected byte range.
pub fn run<'a, L>(
    run: usize,
    spans: Vec<Span<'a, L>>,
    size: impl Into<Pixels>,
    selection: Option<(usize, usize)>,
    sel_color: Color,
) -> Run<'a, L> {
    Run {
        run,
        spans,
        size: size.into(),
        selection,
        sel_color,
    }
}

impl<M, Theme, R, L> Widget<M, Theme, R> for Run<'_, L>
where
    R: text::Renderer<Font = Font>,
    L: Clone + PartialEq + 'static,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<RunState<R::Paragraph, L>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(RunState::<R::Paragraph, L> {
            run: self.run,
            text: String::new(),
            lines: Vec::new(),
            lh: 0.0,
            width: 0.0,
            hovered: None,
        })
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(&mut self, tree: &mut Tree, r: &R, limits: &layout::Limits) -> layout::Node {
        let st = tree.state.downcast_mut::<RunState<R::Paragraph, L>>();
        let max = limits.max();
        let font = r.default_font();
        let lh = text::LineHeight::default().to_absolute(self.size).0;
        let split = split_lines(&self.spans);
        st.run = self.run;
        st.lh = lh;
        st.text = self.spans.iter().map(|s| s.text.as_ref()).collect();
        let same_shape = st.lines.len() == split.len()
            && st
                .lines
                .iter()
                .zip(&split)
                .all(|(l, (_, s))| l.spans.as_slice() == s.as_slice());
        let shape = |spans: &[Span<'_, L>]| {
            R::Paragraph::with_spans(text::Text {
                content: spans,
                bounds: Size::new(max.width, f32::INFINITY),
                size: self.size,
                line_height: text::LineHeight::default(),
                font,
                align_x: text::Alignment::Default,
                align_y: iced::alignment::Vertical::Top,
                shaping: text::Shaping::Advanced,
                wrapping: text::Wrapping::Word,
            })
        };
        if same_shape {
            for l in &mut st.lines {
                if l.paragraph.min_bounds().width > max.width
                    || (l.paragraph.bounds().width - max.width).abs() > 0.5
                {
                    l.paragraph.resize(Size::new(max.width, f32::INFINITY));
                }
            }
        } else {
            st.lines = split
                .iter()
                .map(|(start, spans)| {
                    let owned: Vec<Span<'static, L>> =
                        spans.iter().cloned().map(Span::to_static).collect();
                    Line {
                        start: *start,
                        text: owned.iter().map(|s| s.text.as_ref()).collect(),
                        paragraph: shape(spans),
                        spans: owned,
                        y: 0.0,
                        height: 0.0,
                    }
                })
                .collect();
        }
        let (mut y, mut w) = (0.0f32, 0.0f32);
        for l in &mut st.lines {
            let b = l.paragraph.min_bounds();
            l.y = y;
            l.height = b.height.max(lh);
            y += l.height;
            w = w.max(b.width);
        }
        st.width = w.min(max.width);
        layout::Node::new(Size::new(st.width, y))
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _r: &R,
        operation: &mut dyn Operation,
    ) {
        let st = tree.state.downcast_mut::<RunState<R::Paragraph, L>>();
        operation.custom(None, layout.bounds(), st as &mut dyn Any);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _r: &R,
        _clip: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        _v: &Rectangle,
    ) {
        if !matches!(event, Event::Mouse(mouse::Event::CursorMoved { .. })) {
            return;
        }
        let st = tree.state.downcast_mut::<RunState<R::Paragraph, L>>();
        let b = layout.bounds();
        let hovered = cursor.position_in(b).and_then(|p| {
            let i = st.line_at(p.y);
            let line = st.lines.get(i)?;
            let s = line.paragraph.hit_span(Point::new(p.x, p.y - line.y))?;
            line.spans.get(s)?.link.as_ref().map(|_| (i, s))
        });
        if hovered != st.hovered {
            st.hovered = hovered;
            shell.request_redraw();
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _v: &Rectangle,
        _r: &R,
    ) -> mouse::Interaction {
        let st = tree.state.downcast_ref::<RunState<R::Paragraph, L>>();
        if st.hovered.is_some() {
            mouse::Interaction::Pointer
        } else if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::None
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        r: &mut R,
        _theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let b = layout.bounds();
        if !b.intersects(viewport) {
            return;
        }
        let st = tree.state.downcast_ref::<RunState<R::Paragraph, L>>();
        for (li, line) in st.lines.iter().enumerate() {
            let origin = Point::new(b.x, b.y + line.y);
            let tr = origin - Point::ORIGIN;
            let p = &line.paragraph;
            // Find highlights (span backgrounds) first, the selection over them, text on top.
            for (i, s) in line.spans.iter().enumerate() {
                if let Some(h) = s.highlight {
                    for sb in p.span_bounds(i) {
                        r.fill_quad(
                            renderer::Quad {
                                bounds: sb + tr,
                                border: h.border,
                                ..Default::default()
                            },
                            h.background,
                        );
                    }
                }
            }
            if let Some((a, e)) = self.selection {
                let end = line.start + line.text.len();
                let (s, t) = (a.max(line.start), e.min(end));
                if s < t {
                    for rect in
                        selection_rects(p, &line.text, st.lh, s - line.start, t - line.start)
                    {
                        r.fill_quad(
                            renderer::Quad {
                                bounds: rect + tr,
                                ..Default::default()
                            },
                            self.sel_color,
                        );
                    }
                }
            }
            for (i, s) in line.spans.iter().enumerate() {
                let hovered = st.hovered == Some((li, i));
                if !(s.underline || s.strikethrough || hovered) {
                    continue;
                }
                let size = s.size.unwrap_or(self.size);
                let lh = s.line_height.unwrap_or_default().to_absolute(size).0;
                let color = s.color.unwrap_or(style.text_color);
                let base = tr + Vector::new(0.0, size.0 + (lh - size.0) / 2.0);
                for sb in p.span_bounds(i) {
                    if s.underline || hovered {
                        r.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle::new(
                                    sb.position() + base - Vector::new(0.0, size.0 * 0.08),
                                    Size::new(sb.width, 1.0),
                                ),
                                ..Default::default()
                            },
                            color,
                        );
                    }
                    if s.strikethrough {
                        r.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle::new(
                                    sb.position() + base - Vector::new(0.0, size.0 / 2.0),
                                    Size::new(sb.width, 1.0),
                                ),
                                ..Default::default()
                            },
                            color,
                        );
                    }
                }
            }
            r.fill_paragraph(p, origin, style.text_color, *viewport);
        }
    }
}

impl<'a, M, Theme, R, L> From<Run<'a, L>> for Element<'a, M, Theme, R>
where
    R: text::Renderer<Font = Font> + 'a,
    L: Clone + PartialEq + 'static,
{
    fn from(w: Run<'a, L>) -> Self {
        Element::new(w)
    }
}

/// A run's laid-out bounds (content coordinates) and text length.
#[derive(Debug, Clone, Copy)]
pub struct RunBox {
    pub run: usize,
    pub bounds: Rectangle,
    pub len: usize,
}

/// Collects every run's bounds plus the target scrollable's translation and viewport.
struct Collect<P, L> {
    scroll: iced::widget::Id,
    translation: Vector,
    viewport: Option<Rectangle>,
    runs: Vec<RunBox>,
    _p: PhantomData<fn() -> (P, L)>,
}

impl<P: text::Paragraph + 'static, L: Clone + 'static> Operation for Collect<P, L> {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn scrollable(
        &mut self,
        id: Option<&iced::widget::Id>,
        bounds: Rectangle,
        _content: Rectangle,
        translation: Vector,
        _state: &mut dyn operation::Scrollable,
    ) {
        if id == Some(&self.scroll) {
            self.translation = translation;
            self.viewport = Some(bounds);
        }
    }

    fn custom(&mut self, _id: Option<&iced::widget::Id>, bounds: Rectangle, state: &mut dyn Any) {
        if let Some(s) = state.downcast_ref::<RunState<P, L>>() {
            self.runs.push(RunBox {
                run: s.run,
                bounds,
                len: s.text.len(),
            });
        }
    }
}

/// Runs `f` on the state of run `run` (with its bounds).
struct OnRun<P, L, F> {
    run: usize,
    f: Option<F>,
    _p: PhantomData<fn() -> (P, L)>,
}

impl<P, L, F> Operation for OnRun<P, L, F>
where
    P: text::Paragraph + 'static,
    L: Clone + 'static,
    F: FnOnce(&RunState<P, L>, Rectangle) + Send,
{
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        if self.f.is_some() {
            operate(self);
        }
    }

    fn custom(&mut self, _id: Option<&iced::widget::Id>, bounds: Rectangle, state: &mut dyn Any) {
        if let Some(s) = state.downcast_ref::<RunState<P, L>>()
            && s.run == self.run
            && let Some(f) = self.f.take()
        {
            f(s, bounds);
        }
    }
}

/// Every run's box under `el` (content coordinates; tests and probes).
pub fn probe_runs<M, L: Clone + 'static>(
    el: &mut Element<'_, M>,
    tree: &mut Tree,
    layout: Layout<'_>,
    r: &iced::Renderer,
) -> Vec<RunBox> {
    let mut op = Collect::<Para, L> {
        scroll: iced::widget::Id::unique(),
        translation: Vector::ZERO,
        viewport: None,
        runs: Vec::new(),
        _p: PhantomData,
    };
    el.as_widget_mut().operate(tree, layout, r, &mut op);
    op.runs
}

/// The caret point (content coordinates) of `pos` under `el` (tests and probes).
pub fn probe_caret<M, L: Clone + 'static>(
    el: &mut Element<'_, M>,
    tree: &mut Tree,
    layout: Layout<'_>,
    r: &iced::Renderer,
    pos: SelPos,
) -> Option<Point> {
    let out = std::sync::Mutex::new(None);
    {
        let mut op = OnRun::<Para, L, _> {
            run: pos.run,
            f: Some(|s: &RunState<Para, L>, b: Rectangle| {
                if let Ok(mut o) = out.lock() {
                    *o = Some(s.caret(pos.byte) + (b.position() - Point::ORIGIN));
                }
            }),
            _p: PhantomData,
        };
        el.as_widget_mut().operate(tree, layout, r, &mut op);
    }
    out.into_inner().ok().flatten()
}

/// Which run a content point belongs to when it is not over one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bias {
    /// Dragging: a gap snaps to the end of the run above.
    Drag,
    /// Line down: a gap goes to the run below.
    Down,
    /// Line up: a gap goes to the run above.
    Up,
}

/// The run for content point `p`: `(index into runs, point clamped into it)`.
fn pick(runs: &[RunBox], p: Point, bias: Bias) -> Option<(usize, Point)> {
    let clamp = |r: &Rectangle, p: Point| {
        Point::new(
            p.x.clamp(r.x - 1.0, r.x + r.width + 1.0),
            p.y.clamp(r.y, r.y + (r.height - 1.0).max(0.0)),
        )
    };
    if let Some(i) = runs.iter().position(|r| r.bounds.contains(p)) {
        return Some((i, p));
    }
    // Same band (e.g. table cells side by side): the horizontally nearest.
    let band = runs
        .iter()
        .enumerate()
        .filter(|(_, r)| p.y >= r.bounds.y && p.y < r.bounds.y + r.bounds.height)
        .min_by(|(_, a), (_, b)| {
            let d = |r: &Rectangle| (p.x - r.center_x()).abs();
            d(&a.bounds).total_cmp(&d(&b.bounds))
        });
    if let Some((i, r)) = band {
        return Some((i, clamp(&r.bounds, p)));
    }
    let above = runs
        .iter()
        .enumerate()
        .filter(|(_, r)| r.bounds.y + r.bounds.height <= p.y)
        .max_by_key(|(_, r)| r.run);
    let below = runs
        .iter()
        .enumerate()
        .filter(|(_, r)| r.bounds.y > p.y)
        .min_by_key(|(_, r)| r.run);
    let end = |(i, r): (usize, &RunBox)| {
        (
            i,
            Point::new(f32::MAX / 4.0, r.bounds.y + r.bounds.height - 1.0),
        )
    };
    match bias {
        Bias::Drag => above
            .map(end)
            .or_else(|| below.map(|(i, r)| (i, Point::new(-1.0, r.bounds.y)))),
        Bias::Up => above
            .map(|(i, r)| (i, clamp(&r.bounds, p)))
            .or_else(|| below.map(|(i, r)| (i, Point::new(-1.0, r.bounds.y)))),
        Bias::Down => below
            .map(|(i, r)| (i, clamp(&r.bounds, p)))
            .or_else(|| above.map(end)),
    }
}

/// An in-progress press.
struct Press<L> {
    /// Window position of the press.
    at: Point,
    anchor: SelPos,
    dragging: bool,
    link: Option<L>,
    /// The last window position seen (auto-scroll ticks re-use it).
    last: Point,
}

struct RootState<L> {
    press: Option<Press<L>>,
    /// (time, window position, count) of the last press — double/triple click.
    last_click: Option<(Instant, Point, u8)>,
    modifiers: keyboard::Modifiers,
}

/// Double/triple-click window and distance.
const MULTI_CLICK: Duration = Duration::from_millis(500);
/// Press-release movement below this (px) is a click, not a drag (spike threshold).
const DRAG_THRESHOLD: f32 = 4.0;
/// Auto-scroll zone at the viewport's top/bottom edge (px).
const EDGE: f32 = 24.0;

/// The gesture root around a Reading scrollable. See the module docs.
pub struct Root<'a, M, L> {
    content: Element<'a, M>,
    scroll: iced::widget::Id,
    selection: Option<(SelPos, SelPos)>,
    keys: bool,
    on_change: Box<dyn Fn(Option<Selection>) -> M + 'a>,
    on_release: Box<dyn Fn() -> M + 'a>,
    on_link: Box<dyn Fn(L) -> M + 'a>,
}

/// Wraps `content` (which holds the scrollable `scroll` and the [`Run`]s). `selection` is the
/// tab's `(anchor, focus)`; `keys` = this pane takes Shift+Arrows / Esc for the selection.
pub fn root<'a, M, L>(
    content: impl Into<Element<'a, M>>,
    scroll: iced::widget::Id,
    selection: Option<(SelPos, SelPos)>,
    keys: bool,
    on_change: impl Fn(Option<(SelPos, SelPos)>) -> M + 'a,
    on_release: impl Fn() -> M + 'a,
    on_link: impl Fn(L) -> M + 'a,
) -> Root<'a, M, L> {
    Root {
        content: content.into(),
        scroll,
        selection,
        keys,
        on_change: Box::new(on_change),
        on_release: Box::new(on_release),
        on_link: Box::new(on_link),
    }
}

type Para = <iced::Renderer as text::Renderer>::Paragraph;

impl<M, L> Root<'_, M, L>
where
    L: Clone + Send + 'static,
{
    fn collect(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        r: &iced::Renderer,
    ) -> Collect<Para, L> {
        let mut op = Collect::<Para, L> {
            scroll: self.scroll.clone(),
            translation: Vector::ZERO,
            viewport: None,
            runs: Vec::new(),
            _p: PhantomData,
        };
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, r, &mut op);
        op
    }

    fn on_run<T: Send + Default>(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        r: &iced::Renderer,
        run: usize,
        f: impl FnOnce(&RunState<Para, L>, Rectangle) -> T + Send,
    ) -> T {
        let out = std::sync::Mutex::new(T::default());
        {
            let mut op = OnRun::<Para, L, _> {
                run,
                f: Some(|s: &RunState<Para, L>, b: Rectangle| {
                    if let Ok(mut o) = out.lock() {
                        *o = f(s, b);
                    }
                }),
                _p: PhantomData,
            };
            self.content
                .as_widget_mut()
                .operate(&mut tree.children[0], layout, r, &mut op);
        }
        out.into_inner().unwrap_or_default()
    }

    /// The selection position (and link) at window point `at`.
    fn hit(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        r: &iced::Renderer,
        at: Point,
        bias: Bias,
    ) -> Option<(SelPos, Option<L>)> {
        let c = self.collect(tree, layout, r);
        let p = at + c.translation;
        let (i, q) = pick(&c.runs, p, bias)?;
        let rb = c.runs[i];
        let hit = self.on_run(tree, layout, r, rb.run, move |s, b| {
            let (byte, link) = s.hit(q - (b.position() - Point::ORIGIN));
            Some((byte, link))
        })?;
        Some((SelPos::new(rb.run, hit.0), hit.1))
    }

    fn publish_focus(&self, shell: &mut Shell<'_, M>, anchor: SelPos, focus: SelPos) {
        if self.selection != Some((anchor, focus)) {
            shell.publish((self.on_change)(Some((anchor, focus))));
        }
    }

    /// A drag step at window point `at` (also auto-scroll ticks).
    fn drag(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        r: &iced::Renderer,
        shell: &mut Shell<'_, M>,
        at: Point,
    ) {
        let st = tree.state.downcast_mut::<RootState<L>>();
        let Some(press) = st.press.as_mut() else {
            return;
        };
        press.last = at;
        if !press.dragging && press.at.distance(at) > DRAG_THRESHOLD {
            press.dragging = true;
            press.link = None;
        }
        if !press.dragging {
            return;
        }
        let anchor = press.anchor;
        if let Some((pos, _)) = self.hit(tree, layout, r, at, Bias::Drag) {
            self.publish_focus(shell, anchor, pos);
        }
        // Auto-scroll while the cursor is near / past the viewport's top or bottom edge.
        let c = self.collect(tree, layout, r);
        if let Some(v) = c.viewport {
            let dy = if at.y < v.y + EDGE {
                -(v.y + EDGE - at.y).min(120.0)
            } else if at.y > v.y + v.height - EDGE {
                (at.y - (v.y + v.height - EDGE)).min(120.0)
            } else {
                0.0
            };
            if dy != 0.0 {
                let mut op = operation::scrollable::scroll_by::<()>(
                    self.scroll.clone(),
                    operation::scrollable::AbsoluteOffset {
                        x: 0.0,
                        y: dy / 2.0,
                    },
                );
                self.content
                    .as_widget_mut()
                    .operate(&mut tree.children[0], layout, r, &mut op);
                shell.request_redraw();
            }
        }
    }

    /// Shift+Arrow: moves the focus by a grapheme (Left/Right) or a visual line (Up/Down).
    fn step(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        r: &iced::Renderer,
        named: Named,
    ) -> Option<(SelPos, SelPos)> {
        let (anchor, focus) = self.selection?;
        let c = self.collect(tree, layout, r);
        let i = c.runs.iter().position(|b| b.run == focus.run)?;
        let new = match named {
            Named::ArrowLeft | Named::ArrowRight => {
                let left = named == Named::ArrowLeft;
                let text = self.on_run(tree, layout, r, focus.run, |s, _| s.text().to_owned());
                if left && focus.byte == 0 {
                    let p = c.runs.get(i.checked_sub(1)?)?;
                    SelPos::new(p.run, p.len)
                } else if !left && focus.byte >= text.len() {
                    let n = c.runs.get(i + 1)?;
                    SelPos::new(n.run, 0)
                } else if left {
                    SelPos::new(focus.run, prev_grapheme(&text, focus.byte))
                } else {
                    SelPos::new(focus.run, next_grapheme(&text, focus.byte))
                }
            }
            Named::ArrowUp | Named::ArrowDown => {
                let down = named == Named::ArrowDown;
                let (caret, lh) = self.on_run(tree, layout, r, focus.run, |s, b| {
                    (s.caret(focus.byte) + (b.position() - Point::ORIGIN), s.lh)
                });
                let target = Point::new(caret.x, caret.y + if down { lh } else { -lh });
                let (j, q) = pick(&c.runs, target, if down { Bias::Down } else { Bias::Up })?;
                let rb = c.runs[j];
                let byte = self.on_run(tree, layout, r, rb.run, move |s, b| {
                    s.hit(q - (b.position() - Point::ORIGIN)).0
                });
                SelPos::new(rb.run, byte)
            }
            _ => return None,
        };
        Some((anchor, new))
    }
}

impl<M, L> Widget<M, iced::Theme, iced::Renderer> for Root<'_, M, L>
where
    L: Clone + Send + 'static,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<RootState<L>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(RootState::<L> {
            press: None,
            last_click: None,
            modifiers: keyboard::Modifiers::empty(),
        })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        r: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], r, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        r: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, r, operation);
    }

    #[allow(clippy::too_many_lines)] // one event match; each arm is a gesture
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        r: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        if let Event::Keyboard(keyboard::Event::ModifiersChanged(m)) = event {
            tree.state.downcast_mut::<RootState<L>>().modifiers = *m;
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            r,
            clipboard,
            shell,
            viewport,
        );
        let pressing = tree.state.downcast_ref::<RootState<L>>().press.is_some();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if shell.is_event_captured() {
                    return;
                }
                let Some(at) = cursor.position_over(layout.bounds()) else {
                    return;
                };
                let Some((pos, link)) = self.hit(tree, layout, r, at, Bias::Drag) else {
                    return;
                };
                let st = tree.state.downcast_mut::<RootState<L>>();
                let now = Instant::now();
                let count = match st.last_click {
                    Some((t, p, n))
                        if now.duration_since(t) < MULTI_CLICK
                            && p.distance(at) <= DRAG_THRESHOLD =>
                    {
                        n % 3 + 1
                    }
                    _ => 1,
                };
                st.last_click = Some((now, at, count));
                let shift = st.modifiers.shift();
                let mut press = Press {
                    at,
                    anchor: pos,
                    dragging: false,
                    link,
                    last: at,
                };
                if shift && let Some((anchor, _)) = self.selection {
                    press.anchor = anchor;
                    press.dragging = true;
                    press.link = None;
                    shell.publish((self.on_change)(Some((anchor, pos))));
                } else if count >= 2 {
                    let text = self.on_run(tree, layout, r, pos.run, |s, _| s.text().to_owned());
                    let (a, b) = if count == 2 {
                        word_at(&text, pos.byte)
                    } else {
                        (0, text.len())
                    };
                    press.anchor = SelPos::new(pos.run, a);
                    press.dragging = true;
                    press.link = None;
                    shell.publish((self.on_change)(Some((
                        SelPos::new(pos.run, a),
                        SelPos::new(pos.run, b),
                    ))));
                } else if self.selection.is_some() {
                    shell.publish((self.on_change)(None));
                }
                tree.state.downcast_mut::<RootState<L>>().press = Some(press);
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if pressing => {
                if let Some(at) = cursor.position() {
                    self.drag(tree, layout, r, shell, at);
                }
            }
            Event::Window(window::Event::RedrawRequested(_)) if pressing => {
                let st = tree.state.downcast_ref::<RootState<L>>();
                if let Some(p) = st.press.as_ref().filter(|p| p.dragging) {
                    let at = p.last;
                    let c = self.collect(tree, layout, r);
                    if c.viewport
                        .is_some_and(|v| at.y < v.y + EDGE || at.y > v.y + v.height - EDGE)
                    {
                        self.drag(tree, layout, r, shell, at);
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if pressing => {
                let st = tree.state.downcast_mut::<RootState<L>>();
                let Some(p) = st.press.take() else {
                    return;
                };
                if p.dragging {
                    shell.publish((self.on_release)());
                } else if let Some(l) = p.link {
                    shell.publish((self.on_link)(l));
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(named),
                modifiers,
                ..
            }) if self.keys && !shell.is_event_captured() => {
                let has = self.selection.is_some_and(|(a, f)| a != f);
                if !has {
                    return;
                }
                match named {
                    Named::Escape => {
                        shell.publish((self.on_change)(None));
                        shell.capture_event();
                    }
                    Named::ArrowLeft | Named::ArrowRight | Named::ArrowUp | Named::ArrowDown
                        if modifiers.shift() && !modifiers.command() && !modifiers.alt() =>
                    {
                        if let Some(s) = self.step(tree, layout, r, *named) {
                            self.publish_focus(shell, s.0, s.1);
                        }
                        shell.capture_event();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        r: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(&tree.children[0], layout, cursor, viewport, r)
    }

    fn draw(
        &self,
        tree: &Tree,
        r: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content
            .as_widget()
            .draw(&tree.children[0], r, theme, style, layout, cursor, viewport);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        r: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, iced::Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            r,
            viewport,
            translation,
        )
    }
}

impl<'a, M: 'a, L> From<Root<'a, M, L>> for Element<'a, M>
where
    L: Clone + Send + 'static,
{
    fn from(w: Root<'a, M, L>) -> Self {
        Element::new(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_lines_maps_hard_lines_to_run_offsets() {
        let spans: Vec<Span<'_, ()>> =
            vec![Span::new("ab\ncd"), Span::new("e\n"), Span::new("\nf")];
        let lines = split_lines(&spans);
        let starts: Vec<usize> = lines.iter().map(|l| l.0).collect();
        assert_eq!(starts, hard_lines("ab\ncde\n\nf"));
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.1.iter().map(|s| s.text.as_ref()).collect())
            .collect();
        assert_eq!(texts, ["ab", "cde", "", "f"]);
    }

    #[test]
    fn word_and_run_ranges() {
        let t = "hello brave world";
        assert_eq!(word_at(t, 7), (6, 11));
        assert_eq!(word_at(t, 6), (6, 11));
        assert_eq!(word_at(t, t.len()), (12, 17));
        let sel = Some((SelPos::new(2, 3), SelPos::new(0, 1)));
        assert_eq!(run_range(sel, 0, 5), Some((1, 5)));
        assert_eq!(run_range(sel, 1, 4), Some((0, 4)));
        assert_eq!(run_range(sel, 2, 9), Some((0, 3)));
        assert_eq!(run_range(sel, 3, 9), None);
    }
}
