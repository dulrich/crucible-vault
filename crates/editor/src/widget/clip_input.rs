//! A `text_input` wrapper that adds the X11/Wayland PRIMARY selection contract — plan
//! round-17-todos-ui-sweep WP-4 (Decision 12).
//!
//! iced's `text_input` already handles Ctrl+C/X/V on the Standard clipboard; it has no PRIMARY
//! behaviour. [`ClipInput`] wraps one `TextInput` (its tree child 0, whose state is iced's public
//! `text_input::State`) and, after every event the child handled:
//! - publishes a changed, non-empty selection to PRIMARY;
//! - on a middle press inside its bounds, forwards a synthesized left press + release (focus and
//!   cursor placement at the click), reads PRIMARY and publishes `on_input(value with the text
//!   inserted at the cursor)`, then moves the cursor after the inserted text.
//!
//! An input without `on_input` is read-only: it copies to PRIMARY but never pastes.
//!
//! The builder mirrors the `TextInput` methods the call sites use; `on_input` is given once and
//! feeds both the inner input and the paste path.

use std::rc::Rc;

use iced::advanced::clipboard::Kind;
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer, text};
use iced::widget::text_input::{self, Catalog, Status, Style, TextInput, Value};
use iced::{Element, Event, Length, Padding, Pixels, Rectangle, Size, Vector};

/// See the module docs.
pub struct ClipInput<'a, Message, Theme = iced::Theme, Renderer = iced::Renderer>
where
    Theme: Catalog,
    Renderer: text::Renderer,
{
    input: TextInput<'a, Message, Theme, Renderer>,
    value: String,
    on_input: Option<Rc<dyn Fn(String) -> Message + 'a>>,
}

/// The selection last published to PRIMARY.
#[derive(Debug, Default)]
struct State {
    published: Option<String>,
}

/// A [`ClipInput`] with `placeholder` showing `value` (same arguments as `text_input`).
pub fn clip_input<'a, Message, Theme, Renderer>(
    placeholder: &str,
    value: &str,
) -> ClipInput<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
    Theme: Catalog + 'a,
    Renderer: text::Renderer,
{
    ClipInput {
        input: TextInput::new(placeholder, value),
        value: value.to_owned(),
        on_input: None,
    }
}

impl<'a, Message, Theme, Renderer> ClipInput<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
    Theme: Catalog + 'a,
    Renderer: text::Renderer,
{
    /// Editable: typing (and a middle paste) publishes `f(new value)`.
    #[must_use]
    pub fn on_input(mut self, f: impl Fn(String) -> Message + 'a) -> Self {
        let f: Rc<dyn Fn(String) -> Message + 'a> = Rc::new(f);
        let g = Rc::clone(&f);
        self.input = self.input.on_input(move |s| g(s));
        self.on_input = Some(f);
        self
    }

    /// See `TextInput::id`.
    #[must_use]
    pub fn id(mut self, id: impl Into<iced::widget::Id>) -> Self {
        self.input = self.input.id(id);
        self
    }

    /// See `TextInput::on_submit`.
    #[must_use]
    pub fn on_submit(mut self, message: Message) -> Self {
        self.input = self.input.on_submit(message);
        self
    }

    /// See `TextInput::size`.
    #[must_use]
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.input = self.input.size(size);
        self
    }

    /// See `TextInput::padding`.
    #[must_use]
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.input = self.input.padding(padding);
        self
    }

    /// See `TextInput::width`.
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.input = self.input.width(width);
        self
    }

    /// See `TextInput::font`.
    #[must_use]
    pub fn font(mut self, font: Renderer::Font) -> Self {
        self.input = self.input.font(font);
        self
    }

    /// See `TextInput::style`.
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme, Status) -> Style + 'a) -> Self
    where
        Theme::Class<'a>: From<text_input::StyleFn<'a, Theme>>,
    {
        self.input = self.input.style(style);
        self
    }
}

/// The text the cursor selects in `value`, if any (grapheme indices, as iced's `Value`).
pub fn selected_text(value: &str, cursor: text_input::Cursor) -> Option<String> {
    let v = Value::new(value);
    let (a, b) = cursor.selection(&v)?;
    let s = v.select(a, b).to_string();
    (!s.is_empty()).then_some(s)
}

/// `value` with `insert` placed at the cursor (replacing its selection), and the grapheme index
/// just after the inserted text.
pub fn insert_at_cursor(value: &str, cursor: text_input::Cursor, insert: &str) -> (String, usize) {
    let v = Value::new(value);
    let (a, b) = cursor.selection(&v).unwrap_or_else(|| {
        let at = match cursor.state(&v) {
            text_input::cursor::State::Index(i) => i,
            text_input::cursor::State::Selection { start, end } => start.min(end),
        };
        (at, at)
    });
    let out = format!("{}{insert}{}", v.until(a), v.select(b, v.len()));
    (out, a + Value::new(insert).len())
}

fn child_state<Renderer: text::Renderer>(
    tree: &mut Tree,
) -> &mut text_input::State<Renderer::Paragraph> {
    tree.children[0]
        .state
        .downcast_mut::<text_input::State<Renderer::Paragraph>>()
}

impl<Message, Theme, Renderer> ClipInput<'_, Message, Theme, Renderer>
where
    Message: Clone,
    Theme: Catalog,
    Renderer: text::Renderer,
{
    fn child(&self) -> &dyn Widget<Message, Theme, Renderer> {
        &self.input
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for ClipInput<'_, Message, Theme, Renderer>
where
    Message: Clone,
    Theme: Catalog,
    Renderer: text::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(self.child())]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[self.child()]);
    }

    fn size(&self) -> Size<Length> {
        Widget::size(&self.input)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        Widget::layout(&mut self.input, &mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        Widget::operate(
            &mut self.input,
            &mut tree.children[0],
            layout,
            renderer,
            operation,
        );
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let middle = matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Middle))
        ) && cursor.is_over(layout.bounds());
        if middle {
            for e in [
                mouse::Event::ButtonPressed(mouse::Button::Left),
                mouse::Event::ButtonReleased(mouse::Button::Left),
            ] {
                Widget::update(
                    &mut self.input,
                    &mut tree.children[0],
                    &Event::Mouse(e),
                    layout,
                    cursor,
                    renderer,
                    clipboard,
                    shell,
                    viewport,
                );
            }
            if let Some(f) = &self.on_input
                && let Some(text) = clipboard.read(Kind::Primary).filter(|t| !t.is_empty())
            {
                let state = child_state::<Renderer>(tree);
                let (new, at) = insert_at_cursor(&self.value, state.cursor(), &text);
                state.move_cursor_to(at);
                shell.publish(f(new));
            }
            shell.capture_event();
        } else {
            Widget::update(
                &mut self.input,
                &mut tree.children[0],
                event,
                layout,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
        }
        let sel = selected_text(&self.value, child_state::<Renderer>(tree).cursor());
        let st = tree.state.downcast_mut::<State>();
        if sel != st.published {
            if let Some(s) = &sel {
                clipboard.write(Kind::Primary, s.clone());
            }
            st.published = sel;
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        Widget::mouse_interaction(
            &self.input,
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        Widget::draw(
            &self.input,
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        Widget::overlay(
            &mut self.input,
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<ClipInput<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
    Theme: Catalog + 'a,
    Renderer: text::Renderer + 'a,
{
    fn from(w: ClipInput<'a, Message, Theme, Renderer>) -> Self {
        Element::new(w)
    }
}

#[cfg(test)]
#[path = "clip_input_tests.rs"]
mod tests;
