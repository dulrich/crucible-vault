//! The [`Run`] widget: one selectable text run (see the module docs of `selectable`).

use std::any::Any;
use std::borrow::Cow;

use iced::advanced::text::{self, Paragraph as _};
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, renderer};
use iced::widget::text::Span;
use iced::{Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Size, Vector};
use unicode_segmentation::UnicodeSegmentation;

/// Splits `spans` at hard newlines: one newline-free span list per hard line, each with its
/// byte offset into the concatenated text (audit E3).
pub(super) fn split_lines<'a, L: Clone>(spans: &[Span<'a, L>]) -> Vec<(usize, Vec<Span<'a, L>>)> {
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
    pub(super) run: usize,
    pub(super) text: String,
    lines: Vec<Line<P, L>>,
    pub(super) lh: f32,
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

    /// The run-text byte range of the visual line holding `byte`.
    pub fn visual_line(&self, byte: usize) -> (usize, usize) {
        let i = self
            .lines
            .iter()
            .rposition(|l| l.start <= byte)
            .unwrap_or(0);
        let Some(line) = self.lines.get(i) else {
            return (0, 0);
        };
        let local = byte.saturating_sub(line.start).min(line.text.len());
        let ranges = line_ranges(&line.paragraph, self.lh);
        let (s, e) = super::line_containing(&ranges, local).unwrap_or((0, line.text.len()));
        (line.start + s, line.start + e)
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
