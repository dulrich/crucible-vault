//! A wrapper that publishes one message on a right-button press, carrying the cursor position in
//! window coordinates — plan editor-ui-round-3 WP-4 (the context-menu anchor).
//!
//! Sibling of [`super::middle_click`]: the position is read at press time, so plain pointer
//! motion costs no message (no app-wide `CursorMoved` subscription). Inside a `scrollable` the
//! cursor arrives in content coordinates; the accumulated scroll translation is the `translation`
//! iced hands to `overlay` (called before `update` on every event), so it is stored in the
//! widget state there and added back at press time.

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector};

/// See the module docs.
pub struct RightClick<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    on_right: Box<dyn Fn(Point) -> Message + 'a>,
}

/// The last scroll translation seen by `overlay`.
#[derive(Debug, Default)]
struct State {
    translation: Vector,
}

/// Wraps `content`; a right press over it publishes `on_right(window position)`.
pub fn right_click<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    on_right: impl Fn(Point) -> Message + 'a,
) -> RightClick<'a, Message, Theme, Renderer> {
    RightClick {
        content: content.into(),
        on_right: Box::new(on_right),
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for RightClick<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
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
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
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
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if shell.is_event_captured() {
            return;
        }
        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) = event
            && cursor.is_over(layout.bounds())
            && let Some(at) = cursor.position()
        {
            let translation = tree.state.downcast_ref::<State>().translation;
            shell.publish((self.on_right)(at + translation));
            shell.capture_event();
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
        self.content.as_widget().mouse_interaction(
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
        self.content.as_widget().draw(
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
        tree.state.downcast_mut::<State>().translation = translation;
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<RightClick<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: 'a + renderer::Renderer,
{
    fn from(w: RightClick<'a, Message, Theme, Renderer>) -> Self {
        Element::new(w)
    }
}
