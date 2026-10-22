//! The gesture [`Root`] around a Reading scrollable (see the module docs of `selectable`).

use std::time::{Duration, Instant};

use iced::advanced::widget::{Operation, Tree, operation, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer};
use iced::keyboard::{self, Key, key::Named};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector, window};

use super::ops::{Bias, Collected, Para, collect, on_run, pick};
use super::run::RunState;
use super::{
    SelPos, Selection, next_grapheme, next_word_boundary, prev_grapheme, prev_word_boundary,
    word_at,
};

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
    selection: Option<Selection>,
    keys: bool,
    on_change: Box<dyn Fn(Option<Selection>) -> M + 'a>,
    on_release: Box<dyn Fn() -> M + 'a>,
    on_link: Box<dyn Fn(L) -> M + 'a>,
    /// Plan round-5 WP-8: right press → (link under the cursor, press inside the selection,
    /// window point). Only published when there is a link or the press is inside the selection.
    on_right: Option<RightFn<'a, M, L>>,
}

type RightFn<'a, M, L> = Box<dyn Fn(Option<L>, bool, Point) -> M + 'a>;

/// Wraps `content` (which holds the scrollable `scroll` and the [`Run`](super::Run)s). `selection` is the
/// tab's [`Selection`]; `keys` = this pane takes Shift+Arrows / Esc for the selection.
pub fn root<'a, M, L>(
    content: impl Into<Element<'a, M>>,
    scroll: iced::widget::Id,
    selection: Option<Selection>,
    keys: bool,
    on_change: impl Fn(Option<Selection>) -> M + 'a,
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
        on_right: None,
    }
}

impl<'a, M, L> Root<'a, M, L> {
    /// Plan round-5 WP-8: sets the right-press callback (see [`Root`]'s `on_right`).
    #[must_use]
    pub fn on_right(mut self, f: impl Fn(Option<L>, bool, Point) -> M + 'a) -> Self {
        self.on_right = Some(Box::new(f));
        self
    }
}

impl<M, L> Root<'_, M, L>
where
    L: Clone + Send + 'static,
{
    /// Every run's box and the scrollable's translation/viewport (once per event).
    fn collect(&mut self, tree: &mut Tree, layout: Layout<'_>, r: &iced::Renderer) -> Collected {
        let scroll = Some(self.scroll.clone());
        collect::<M, L>(&mut self.content, &mut tree.children[0], layout, r, scroll)
    }

    fn on_run<T: Send + Default>(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        r: &iced::Renderer,
        run: usize,
        f: impl FnOnce(&RunState<Para, L>, Rectangle) -> T + Send,
    ) -> T {
        on_run(&mut self.content, &mut tree.children[0], layout, r, run, f)
    }

    /// The selection position (and link) at window point `at`, over the collected runs `c`.
    fn hit(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        r: &iced::Renderer,
        c: &Collected,
        at: Point,
        bias: Bias,
    ) -> Option<(SelPos, Option<L>)> {
        let p = at + c.translation;
        let (i, q) = pick(&c.runs, p, bias)?;
        let rb = c.runs[i];
        let hit = self.on_run(tree, layout, r, rb.run, move |s, b| {
            Some(s.hit(q - (b.position() - Point::ORIGIN)))
        })?;
        Some((SelPos::new(rb.run, hit.0), hit.1))
    }

    fn publish(&self, shell: &mut Shell<'_, M>, sel: Selection) {
        if self.selection != Some(sel) {
            shell.publish((self.on_change)(Some(sel)));
        }
    }

    /// A drag step at window point `at` (also auto-scroll ticks), over the collected runs `c`.
    #[allow(clippy::too_many_arguments)]
    fn drag(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        r: &iced::Renderer,
        c: &Collected,
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
        if let Some((pos, _)) = self.hit(tree, layout, r, c, at, Bias::Drag) {
            self.publish(shell, Selection::new(anchor, pos));
        }
        // Auto-scroll while the cursor is near / past the viewport's top or bottom edge.
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

    /// Shift+motion: moves the focus by a grapheme (Left/Right), a word (Ctrl+Left/Right), a
    /// visual line (Up/Down), to the visual line's edge (Home/End) or to the document's edge
    /// (Ctrl+Home/End).
    fn step(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        r: &iced::Renderer,
        named: Named,
        ctrl: bool,
    ) -> Option<Selection> {
        let Selection { anchor, focus } = self.selection?;
        let c = self.collect(tree, layout, r);
        let i = c.runs.iter().position(|b| b.run == focus.run)?;
        let new = match named {
            Named::Home | Named::End if ctrl => {
                let p = if named == Named::Home {
                    c.runs.first()?
                } else {
                    c.runs.last()?
                };
                SelPos::new(p.run, if named == Named::Home { 0 } else { p.len })
            }
            Named::Home | Named::End => {
                let (s, e) =
                    self.on_run(tree, layout, r, focus.run, |s, _| s.visual_line(focus.byte));
                SelPos::new(focus.run, if named == Named::Home { s } else { e })
            }
            Named::ArrowLeft | Named::ArrowRight => {
                let left = named == Named::ArrowLeft;
                let text = self.on_run(tree, layout, r, focus.run, |s, _| s.text().to_owned());
                if left && focus.byte == 0 {
                    let p = c.runs.get(i.checked_sub(1)?)?;
                    SelPos::new(p.run, p.len)
                } else if !left && focus.byte >= text.len() {
                    let n = c.runs.get(i + 1)?;
                    SelPos::new(n.run, 0)
                } else if left && ctrl {
                    SelPos::new(focus.run, prev_word_boundary(&text, focus.byte))
                } else if ctrl {
                    SelPos::new(focus.run, next_word_boundary(&text, focus.byte))
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
        Some(Selection::new(anchor, new))
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
                let c = self.collect(tree, layout, r);
                let Some((pos, link)) = self.hit(tree, layout, r, &c, at, Bias::Drag) else {
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
                if shift && let Some(Selection { anchor, .. }) = self.selection {
                    press.anchor = anchor;
                    press.dragging = true;
                    press.link = None;
                    shell.publish((self.on_change)(Some(Selection::new(anchor, pos))));
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
                    shell.publish((self.on_change)(Some(Selection::new(
                        SelPos::new(pos.run, a),
                        SelPos::new(pos.run, b),
                    ))));
                } else if self.selection.is_some() {
                    shell.publish((self.on_change)(None));
                }
                tree.state.downcast_mut::<RootState<L>>().press = Some(press);
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
                if self.on_right.is_some() && !shell.is_event_captured() =>
            {
                let Some(at) = cursor.position_over(layout.bounds()) else {
                    return;
                };
                let c = self.collect(tree, layout, r);
                let Some((pos, link)) = self.hit(tree, layout, r, &c, at, Bias::Drag) else {
                    return;
                };
                let inside = self.selection.is_some_and(|s| s.contains(pos));
                if link.is_none() && !inside {
                    return;
                }
                if let Some(f) = &self.on_right {
                    shell.publish(f(link, inside, at));
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if pressing => {
                if let Some(at) = cursor.position() {
                    let c = self.collect(tree, layout, r);
                    self.drag(tree, layout, r, &c, shell, at);
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
                        self.drag(tree, layout, r, &c, shell, at);
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
                let has = self.selection.is_some_and(|s| !s.is_empty());
                if !has {
                    return;
                }
                match named {
                    Named::Escape => {
                        shell.publish((self.on_change)(None));
                        shell.capture_event();
                    }
                    Named::ArrowLeft | Named::ArrowRight | Named::Home | Named::End
                        if modifiers.shift() && !modifiers.alt() =>
                    {
                        if let Some(s) = self.step(tree, layout, r, *named, modifiers.command()) {
                            self.publish(shell, s);
                        }
                        shell.capture_event();
                    }
                    Named::ArrowUp | Named::ArrowDown
                        if modifiers.shift() && !modifiers.command() && !modifiers.alt() =>
                    {
                        if let Some(s) = self.step(tree, layout, r, *named, false) {
                            self.publish(shell, s);
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
