//! Plan round-22 WP-1: the tab strip's Left/Right through the real route — a header press and
//! a key press driven through the laid-out widget tree (headless tiny-skia), the ignored key
//! fed to the palette's `keyboard::listen` mapping, and the global mouse listeners' messages
//! delivered as the runtime does. The harness cannot run an app `Task`, so it applies the two
//! widget operations the strip code returns (`tabs.rs`: the header press's unfocus, the
//! `StripKey` scroll) itself, and asserts the app asked for them.

use iced::advanced::widget::operation::scrollable as op;
use iced::advanced::widget::{Operation, Tree, operation};
use iced::advanced::{Shell, clipboard, layout, mouse};
use iced::keyboard::{self, Key, Location, Modifiers, key};
use iced::widget::pane_grid::Pane;
use iced::{Event, Point, Rectangle, Size, Vector};

use super::shared_docs::setup;
use super::*;
use crate::message::ReadingMsg;
use crate::reading::ViewMode;

const W: f32 = 900.0;
const H: f32 = 600.0;

struct Ui {
    renderer: iced::Renderer,
    tree: Option<Tree>,
}

impl Ui {
    fn with<R>(
        &mut self,
        app: &App,
        f: impl FnOnce(
            &mut iced::Element<'_, Message>,
            &mut Tree,
            layout::Layout<'_>,
            &iced::Renderer,
        ) -> R,
    ) -> R {
        let mut el = app.view();
        let tree = match &mut self.tree {
            Some(t) => {
                t.diff(&el);
                t
            }
            None => self.tree.insert(Tree::new(&el)),
        };
        let limits = layout::Limits::new(Size::ZERO, Size::new(W, H));
        let node = el.as_widget_mut().layout(tree, &self.renderer, &limits);
        f(&mut el, tree, layout::Layout::new(&node), &self.renderer)
    }

    fn operate(&mut self, app: &App, o: &mut dyn Operation<()>) {
        self.with(app, |el, tree, l, r| {
            el.as_widget_mut().operate(tree, l, r, o)
        });
    }

    /// Sends `event` through the tree: the published messages and whether it was captured.
    fn event(&mut self, app: &App, event: &Event, at: Point) -> (Vec<Message>, bool) {
        self.with(app, |el, tree, l, r| {
            let mut msgs = Vec::new();
            let mut shell = Shell::new(&mut msgs);
            el.as_widget_mut().update(
                tree,
                event,
                l,
                mouse::Cursor::Available(at),
                r,
                &mut clipboard::Null,
                &mut shell,
                &Rectangle::with_size(Size::new(W, H)),
            );
            let captured = shell.is_event_captured();
            (msgs, captured)
        })
    }

    fn strip(&mut self, app: &App, pane: Pane) -> (Rectangle, f32) {
        let mut g = StripOp {
            id: crate::app::scroll::tab_strip_id(pane),
            out: None,
        };
        self.operate(app, &mut g);
        g.out.expect("tab strip laid out")
    }
}

struct StripOp {
    id: iced::widget::Id,
    out: Option<(Rectangle, f32)>,
}

impl Operation<()> for StripOp {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<()>)) {
        operate(self);
    }
    fn scrollable(
        &mut self,
        id: Option<&iced::widget::Id>,
        bounds: Rectangle,
        _content: Rectangle,
        t: Vector,
        _s: &mut dyn operation::Scrollable,
    ) {
        if id == Some(&self.id) {
            self.out = Some((bounds, t.x));
        }
    }
}

fn arrow(n: key::Named) -> keyboard::Event {
    keyboard::Event::KeyPressed {
        key: Key::Named(n),
        modified_key: Key::Named(n),
        physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
        location: Location::Standard,
        modifiers: Modifiers::empty(),
        text: None,
        repeat: false,
    }
}

fn mouse_button(down: bool) -> Event {
    let b = iced::mouse::Button::Left;
    Event::Mouse(if down {
        iced::mouse::Event::ButtonPressed(b)
    } else {
        iced::mouse::Event::ButtonReleased(b)
    })
}

/// The messages the runtime delivers for one mouse event: the tree's, then the global
/// listeners' (`mouse_button_event`, `tab_drag_event`: any status).
fn deliver_mouse(ui: &mut Ui, app: &mut App, down: bool, at: Point) -> Vec<String> {
    let (mut msgs, _) = ui.event(app, &mouse_button(down), at);
    msgs.push(Message::Panes(crate::message::PanesMsg::MouseButton(down)));
    if !down {
        msgs.push(Message::Tabs(TabsMsg::DragRelease));
    }
    let mut seen = Vec::new();
    for m in msgs {
        seen.push(format!("{m:?}"));
        let is_press = matches!(m, Message::Tabs(TabsMsg::DragPress(..)));
        let t = app.send(m);
        if is_press {
            // `tabs.rs` returns this unfocus with the press; the harness runs it.
            assert!(t.units() > 0, "the header press returns its unfocus");
            let mut f = operation::focusable::focus::<()>(iced::widget::Id::new("tab-strip-focus"));
            ui.operate(app, &mut f);
        }
    }
    seen
}

#[test]
fn header_press_then_arrows_scroll_the_strip_through_the_real_route() {
    let names: Vec<String> = (0..24)
        .map(|i| format!("sr/a-rather-long-note-name-{i:02}.md"))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let (_t, _d, mut app) = setup(&refs);
    let pane = app.focus;
    let path = app.active_note().map(|t| t.path.to_owned()).expect("note");
    let _ = app.send(Message::Reading(ReadingMsg::SetMode {
        path: path.clone(),
        mode: ViewMode::Source,
    }));
    let mut ui = Ui {
        renderer: super::find_keys::renderer(),
        tree: None,
    };
    // The editor is focused, as after typing.
    let mut f = operation::focusable::focus::<()>(crate::source::editor_id(&path));
    ui.operate(&app, &mut f);
    // Start at the strip's left edge.
    let mut s = op::snap_to::<()>(
        crate::app::scroll::tab_strip_id(pane),
        op::RelativeOffset {
            x: Some(0.0),
            y: None,
        },
    );
    ui.operate(&app, &mut s);
    let (bounds, x0) = ui.strip(&app, pane);
    assert!(x0.abs() < 0.5, "starts at the left edge");
    let at = Point::new(bounds.x + 30.0, bounds.y + 8.0);
    let _ = ui.event(
        &app,
        &Event::Mouse(iced::mouse::Event::CursorMoved { position: at }),
        at,
    );
    let mut seen = deliver_mouse(&mut ui, &mut app, true, at);
    seen.extend(deliver_mouse(&mut ui, &mut app, false, at));
    let active = app.active_note().map(|t| t.path.to_owned());
    let body = crate::app::buf(&app.docs, &path).map(|d| d.text());
    assert_eq!(
        app.strip_focus,
        Some(pane),
        "the header press leaves the strip focused (messages: {seen:?})"
    );
    let ev = Event::Keyboard(arrow(key::Named::ArrowRight));
    let (msgs, captured) = ui.event(&app, &ev, at);
    assert!(!captured, "no widget captures Right (messages: {msgs:?})");
    for m in msgs {
        let _ = app.send(m);
    }
    // Ignored: `keyboard::listen` hands it to the palette's mapping.
    let m = crate::palette::key_event(arrow(key::Named::ArrowRight)).expect("Right maps");
    let t = app.send(m);
    assert!(t.units() > 0, "StripKey returns the strip scroll");
    let mut by = op::scroll_by::<()>(
        crate::app::scroll::tab_strip_id(pane),
        op::AbsoluteOffset {
            x: crate::app::scroll::STRIP_KEY_STEP,
            y: 0.0,
        },
    );
    ui.operate(&app, &mut by);
    let (_, x1) = ui.strip(&app, pane);
    assert!(x1 > x0 + 1.0, "Right moved the strip ({x0} -> {x1})");
    assert_eq!(app.active_note().map(|t| t.path.to_owned()), active);
    assert_eq!(crate::app::buf(&app.docs, &path).map(|d| d.text()), body);
}
