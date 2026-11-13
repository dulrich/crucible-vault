//! Plan round-24 WP-2: a single-line selectable text for chrome outside the Reading run table
//! (view titles and column headers). The Reading `Root` owns gestures for `Run`s only and a
//! second `Run` inside it would corrupt its run indices, so this is self-contained: a press
//! starts a gesture; a release without a drag past [`DRAG_THRESHOLD`] publishes `on_click`
//! (a sortable header still sorts); a drag selects, painted under the text; Ctrl/Cmd+C copies
//! the selection; a press elsewhere clears it. It captures the press it handled, as the
//! `button` it replaces did, so a Reading `Root` above never starts its own gesture here.

use iced::advanced::clipboard::Kind;
use iced::advanced::text::{self, Paragraph as _};
use iced::advanced::widget::{Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, renderer};
use iced::keyboard::{self, Key};
use iced::{Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Size};
use unicode_segmentation::UnicodeSegmentation;

/// Pointer travel (px) before a press becomes a selection drag.
pub const DRAG_THRESHOLD: f32 = 4.0;

/// Whether a press at `from` has become a drag at `now` (pure).
pub fn is_drag(from: Point, now: Point) -> bool {
    (now.x - from.x).hypot(now.y - from.y) > DRAG_THRESHOLD
}

/// The `(lo, hi)` byte range of an anchor/focus pair, `None` when collapsed (pure).
pub fn ordered(anchor: usize, focus: usize) -> Option<(usize, usize)> {
    (anchor != focus).then(|| (anchor.min(focus), anchor.max(focus)))
}

/// The largest char boundary of `s` at or below `i` (pure).
fn floor(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The selected text of `s` for `anchor`/`focus` (pure; empty when collapsed).
pub fn selected(s: &str, anchor: usize, focus: usize) -> &str {
    match ordered(floor(s, anchor), floor(s, focus)) {
        Some((a, b)) => &s[a..b],
        None => "",
    }
}

pub struct ChromeText<'a, M> {
    content: String,
    size: Pixels,
    color: Color,
    font: Font,
    sel_color: Color,
    on_click: Option<M>,
    _p: std::marker::PhantomData<&'a ()>,
}

/// A selectable single line of `content`.
pub fn chrome_text<'a, M>(
    content: impl Into<String>,
    size: impl Into<Pixels>,
    color: Color,
    sel_color: Color,
) -> ChromeText<'a, M> {
    ChromeText {
        content: content.into(),
        size: size.into(),
        color,
        font: Font::default(),
        sel_color,
        on_click: None,
        _p: std::marker::PhantomData,
    }
}

impl<M> ChromeText<'_, M> {
    pub fn font(mut self, font: Font) -> Self {
        self.font = font;
        self
    }

    /// Published on a press-release that never became a drag.
    pub fn on_click(mut self, m: M) -> Self {
        self.on_click = Some(m);
        self
    }
}

struct State<P> {
    paragraph: P,
    text: String,
    anchor: usize,
    focus: usize,
    /// Where the left button went down, and whether it has become a drag.
    press: Option<(Point, bool)>,
}

impl<P: text::Paragraph> State<P> {
    fn byte_at(&self, local: Point) -> usize {
        self.paragraph
            .hit_test(local)
            .map_or(self.text.len(), |h| h.cursor())
            .min(self.text.len())
    }

    fn x_at(&self, byte: usize) -> f32 {
        let b = floor(&self.text, byte);
        let g = self.text[..b].graphemes(false).count();
        self.paragraph.grapheme_position(0, g).map_or(0.0, |p| p.x)
    }
}

impl<M, Theme, R> Widget<M, Theme, R> for ChromeText<'_, M>
where
    M: Clone,
    R: text::Renderer<Font = Font>,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<R::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<R::Paragraph> {
            paragraph: R::Paragraph::default(),
            text: String::new(),
            anchor: 0,
            focus: 0,
            press: None,
        })
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(&mut self, tree: &mut Tree, _r: &R, limits: &layout::Limits) -> layout::Node {
        let st = tree.state.downcast_mut::<State<R::Paragraph>>();
        if st.text != self.content {
            st.anchor = 0;
            st.focus = 0;
            st.text.clone_from(&self.content);
        }
        st.paragraph = R::Paragraph::with_text(text::Text {
            content: self.content.as_str(),
            bounds: Size::new(f32::INFINITY, f32::INFINITY),
            size: self.size,
            line_height: text::LineHeight::default(),
            font: self.font,
            align_x: text::Alignment::Default,
            align_y: iced::alignment::Vertical::Top,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::None,
        });
        let b = st.paragraph.min_bounds();
        layout::Node::new(limits.max().min(Size::new(b.width, b.height)))
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _r: &R,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        _v: &Rectangle,
    ) {
        let st = tree.state.downcast_mut::<State<R::Paragraph>>();
        let b = layout.bounds();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(p) = cursor.position_in(b) {
                    let at = st.byte_at(p);
                    st.anchor = at;
                    st.focus = at;
                    st.press = Some((p, false));
                    shell.capture_event();
                    shell.request_redraw();
                } else if st.anchor != st.focus {
                    st.anchor = 0;
                    st.focus = 0;
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                if let Some((from, dragging)) = st.press {
                    let local = Point::new(position.x - b.x, position.y - b.y);
                    if dragging || is_drag(from, local) {
                        st.press = Some((from, true));
                        st.focus = st.byte_at(local);
                        shell.capture_event();
                        shell.request_redraw();
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if let Some((_, dragging)) = st.press.take() {
                    shell.capture_event();
                    if !dragging
                        && cursor.is_over(b)
                        && let Some(m) = &self.on_click
                    {
                        shell.publish(m.clone());
                    }
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. })
                if modifiers.command()
                    && matches!(key.as_ref(), Key::Character("c" | "C"))
                    && st.anchor != st.focus =>
            {
                clipboard.write(
                    Kind::Standard,
                    selected(&st.text, st.anchor, st.focus).to_owned(),
                );
                shell.capture_event();
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _v: &Rectangle,
        _r: &R,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            if self.on_click.is_some() {
                mouse::Interaction::Pointer
            } else {
                mouse::Interaction::Text
            }
        } else {
            mouse::Interaction::None
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        r: &mut R,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let b = layout.bounds();
        if !b.intersects(viewport) {
            return;
        }
        let st = tree.state.downcast_ref::<State<R::Paragraph>>();
        if let Some((lo, hi)) = ordered(floor(&st.text, st.anchor), floor(&st.text, st.focus)) {
            let (x0, x1) = (st.x_at(lo), st.x_at(hi));
            r.fill_quad(
                renderer::Quad {
                    bounds: Rectangle::new(
                        Point::new(b.x + x0, b.y),
                        Size::new((x1 - x0).max(0.0), b.height),
                    ),
                    ..Default::default()
                },
                self.sel_color,
            );
        }
        r.fill_paragraph(&st.paragraph, b.position(), self.color, *viewport);
    }
}

impl<'a, M, Theme, R> From<ChromeText<'a, M>> for Element<'a, M, Theme, R>
where
    M: Clone + 'a,
    R: text::Renderer<Font = Font> + 'a,
{
    fn from(w: ChromeText<'a, M>) -> Self {
        Element::new(w)
    }
}

#[cfg(test)]
#[path = "chrome_text_tests.rs"]
mod tests;
