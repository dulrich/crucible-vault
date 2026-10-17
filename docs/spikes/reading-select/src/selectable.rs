//! Spike widget: one selectable rich-text block. Selection state is owned by
//! the app (anchor/focus keyed by block index + byte offset), so a drag can
//! span several of these widgets stacked in a column.
use iced_core::layout::{self, Layout};
use iced_core::mouse;
use iced_core::renderer;
use iced_core::text::{self, Paragraph as _, Span};
use iced_core::widget::tree::{self, Tree};
use iced_core::{
    Clipboard, Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Shell, Size, Widget,
    keyboard,
};
use unicode_segmentation::UnicodeSegmentation;

pub type Link = String;

thread_local! {
    /// Spike-only instrumentation: last drawn bounds per block (harness reads it).
    pub static DEBUG_BOUNDS: std::cell::RefCell<Vec<Rectangle>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[derive(Debug, Clone, PartialEq)]
pub enum SelEvent {
    Press { block: usize, byte: usize, at: Point, link: Option<Link> },
    Drag { block: usize, byte: usize, at: Point },
    Release,
    Copy,
}

pub struct SelPara<'a, M> {
    block: usize,
    spans: &'a [Span<'static, Link>],
    /// Byte range of this block that is selected (already clamped by the app).
    selection: Option<(usize, usize)>,
    /// A press is in progress somewhere in the document.
    pressed: bool,
    size: Pixels,
    on: fn(SelEvent) -> M,
}

impl<'a, M> SelPara<'a, M> {
    pub fn new(
        block: usize,
        spans: &'a [Span<'static, Link>],
        selection: Option<(usize, usize)>,
        pressed: bool,
        on: fn(SelEvent) -> M,
    ) -> Self {
        Self { block, spans, selection, pressed, size: Pixels(16.0), on }
    }
}

struct State<P> {
    paragraph: P,
}

/// Visual-line byte ranges, recovered through the `Paragraph` trait only:
/// probe `hit_test` at the left and right edge of every line.
pub fn line_ranges<P: text::Paragraph>(p: &P, lh: f32) -> Vec<(usize, usize)> {
    let h = p.min_bounds().height;
    let n = (h / lh).round().max(1.0) as usize;
    (0..n)
        .map(|k| {
            let y = (k as f32 + 0.5) * lh;
            let s = p.hit_test(Point::new(-1.0, y)).map_or(0, |h| h.cursor());
            let e = p.hit_test(Point::new(1.0e6, y)).map_or(s, |h| h.cursor());
            (s, e.max(s))
        })
        .collect()
}

/// Selection rectangles (paragraph-local) for the byte range `[a, b)`.
pub fn selection_rects<P: text::Paragraph>(
    p: &P,
    text: &str,
    lh: f32,
    a: usize,
    b: usize,
) -> Vec<Rectangle> {
    let x_at = |line: usize, ls: usize, byte: usize| {
        let g = text[ls..byte].graphemes(false).count();
        p.grapheme_position(line, g).map_or(0.0, |pt| pt.x)
    };
    let mut out = Vec::new();
    for (k, (ls, le)) in line_ranges(p, lh).into_iter().enumerate() {
        let (s, e) = (a.max(ls), b.min(le));
        if s >= e {
            continue;
        }
        let x0 = x_at(k, ls, s);
        let x1 = x_at(k, ls, e);
        out.push(Rectangle::new(Point::new(x0, k as f32 * lh), Size::new((x1 - x0).max(1.0), lh)));
    }
    out
}

pub fn plain(spans: &[Span<'static, Link>]) -> String {
    spans.iter().map(|s| s.text.as_ref()).collect()
}

impl<M, T, R> Widget<M, T, R> for SelPara<'_, M>
where
    R: text::Renderer<Font = Font>,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<R::Paragraph>>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(State { paragraph: R::Paragraph::default() })
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(&mut self, tree: &mut Tree, _r: &R, limits: &layout::Limits) -> layout::Node {
        let st = tree.state.downcast_mut::<State<R::Paragraph>>();
        let width = limits.max().width;
        st.paragraph = R::Paragraph::with_spans(text::Text {
            content: self.spans,
            bounds: Size::new(width, f32::INFINITY),
            size: self.size,
            line_height: text::LineHeight::default(),
            font: Font::DEFAULT,
            align_x: text::Alignment::Default,
            align_y: iced_core::alignment::Vertical::Top,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::Word,
        });
        layout::Node::new(Size::new(width, st.paragraph.min_bounds().height))
    }

    fn draw(
        &self,
        tree: &Tree,
        r: &mut R,
        _t: &T,
        style: &renderer::Style,
        layout: Layout<'_>,
        _c: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let b = layout.bounds();
        DEBUG_BOUNDS.with(|d| {
            let mut d = d.borrow_mut();
            if d.len() <= self.block { d.resize(self.block + 1, Rectangle::default()); }
            d[self.block] = b;
        });
        if !b.intersects(viewport) {
            return;
        }
        let p = &tree.state.downcast_ref::<State<R::Paragraph>>().paragraph;
        let lh = p.line_height().to_absolute(self.size).0;
        if let Some((a, e)) = self.selection {
            for rect in selection_rects(p, &plain(self.spans), lh, a, e) {
                r.fill_quad(
                    renderer::Quad { bounds: rect + (b.position() - Point::ORIGIN), ..Default::default() },
                    Color::from_rgb8(0xb3, 0xd4, 0xff),
                );
            }
        }
        for (i, s) in self.spans.iter().enumerate() {
            if s.underline {
                for sb in p.span_bounds(i) {
                    r.fill_quad(
                        renderer::Quad {
                            bounds: Rectangle::new(
                                Point::new(b.x + sb.x, b.y + sb.y + sb.height - 3.0),
                                Size::new(sb.width, 1.0),
                            ),
                            ..Default::default()
                        },
                        s.color.unwrap_or(style.text_color),
                    );
                }
            }
        }
        r.fill_paragraph(p, b.position(), style.text_color, *viewport);
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
        let p = &tree.state.downcast_ref::<State<R::Paragraph>>().paragraph;
        let b = layout.bounds();
        let hit = |at: Point| {
            // Clamp into the paragraph so drags past the edge snap to line ends.
            let local = Point::new((at.x - b.x).clamp(-1.0, b.width + 1.0), (at.y - b.y).clamp(0.0, b.height - 1.0));
            (p.hit_test(local).map_or(0, |h| h.cursor()), p.hit_span(local))
        };
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(at) = cursor.position_over(b) {
                    let (byte, span) = hit(at);
                    let link = span.and_then(|i| self.spans.get(i)?.link.clone());
                    shell.publish((self.on)(SelEvent::Press { block: self.block, byte, at, link }));
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if self.pressed => {
                if let Some(at) = cursor.position() {
                    if at.y >= b.y && at.y < b.y + b.height {
                        let (byte, _) = hit(at);
                        shell.publish((self.on)(SelEvent::Drag { block: self.block, byte, at }));
                    }
                }
            }
            // Spike shortcut: document-global events are emitted by block 0 only;
            // production puts these on a selection-root container.
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if self.pressed && self.block == 0 =>
            {
                shell.publish((self.on)(SelEvent::Release));
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. })
                if self.block == 0
                    && modifiers.command()
                    && *key == keyboard::Key::Character("c".into()) =>
            {
                shell.publish((self.on)(SelEvent::Copy));
            }
            _ => {}
        }
    }
}

impl<'a, M: 'a, T, R> From<SelPara<'a, M>> for Element<'a, M, T, R>
where
    R: text::Renderer<Font = Font> + 'a,
{
    fn from(w: SelPara<'a, M>) -> Self {
        Element::new(w)
    }
}
