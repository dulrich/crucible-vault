//! The Source↔Reading scroll matrix. Drives the real widget tree
//! (headless tiny-skia) and executes every programmatic scroll / probe the app emits
//! (`reading::ScrollIntent`) against the laid-out tree, so positions are measured post-layout:
//! Reading = the scrollable's offset and the block at the viewport top (the app's `ProbeOp`);
//! Source = the editor's real scroll, read from the caret rectangle the focused `text_editor`
//! reports to the input method (never the `top_line` estimate).

use iced::advanced::widget::operation::scrollable as op;
use iced::advanced::widget::{Operation, Tree, operation};
use iced::advanced::{InputMethod, Shell, clipboard, layout, mouse};
use iced::keyboard::{self, Key, Location, Modifiers, key};
use iced::widget::text_editor::{Action, Motion};
use iced::{Event, Rectangle, Size, Vector};

use super::*;
use crate::message::{FindMsg, ReadingMsg};
use crate::reading::{ScrollIntent, ViewMode};

const PATH: &str = "srm/long.md";
const W: f32 = 1000.0;
const H: f32 = 700.0;

/// 160 blocks: headings, short paragraphs and every fourth paragraph long enough to wrap
/// several times; "needle" sits in a few blocks spread through the note.
fn body() -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    for i in 0..80 {
        let _ = write!(s, "## Section {i}\n\n");
        if i % 4 == 1 {
            for w in 0..160 {
                let _ = write!(s, "word{i}x{w} ");
            }
            s.push_str("end\n\n");
        } else if i % 13 == 5 {
            let _ = write!(s, "Paragraph {i} has the needle in it.\n\n");
        } else {
            let _ = write!(s, "Paragraph {i} is short.\n\n");
        }
    }
    s
}

struct Ui {
    renderer: iced::Renderer,
    tree: Option<Tree>,
    /// The note's render model (Source mode keeps no reading doc), mapping lines to blocks.
    model: editor_model::render::RenderModel,
}

fn intents() -> Vec<ScrollIntent> {
    crate::reading::INTENTS.with(|i| std::mem::take(&mut *i.borrow_mut()))
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
            el.as_widget_mut().operate(tree, l, r, o);
        });
    }

    /// Sends `event`, feeds the messages to the app, then runs the emitted intents.
    /// Returns the input method the tree requested.
    fn send(&mut self, app: &mut App, event: &Event, cursor: mouse::Cursor) -> InputMethod {
        let (msgs, im) = self.with(app, |el, tree, l, r| {
            let mut msgs = Vec::new();
            let mut shell = Shell::new(&mut msgs);
            el.as_widget_mut().update(
                tree,
                event,
                l,
                cursor,
                r,
                &mut clipboard::Null,
                &mut shell,
                &Rectangle::with_size(Size::new(W, H)),
            );
            let im = shell.input_method().clone();
            (msgs, im)
        });
        for m in msgs {
            let _ = app.send(m);
        }
        self.drain(app);
        im
    }

    fn redraw(&mut self, app: &mut App) -> InputMethod {
        self.send(
            app,
            &Event::Window(iced::window::Event::RedrawRequested(
                std::time::Instant::now(),
            )),
            mouse::Cursor::Unavailable,
        )
    }

    fn update(&mut self, app: &mut App, m: Message) {
        let _ = app.send(m);
        self.drain(app);
    }

    /// Executes the app's pending scroll / probe intents on the laid-out tree (probe
    /// answers are fed back, and their own intents run in turn).
    fn drain(&mut self, app: &mut App) {
        for _ in 0..50 {
            let pending = intents();
            if pending.is_empty() {
                return;
            }
            for i in pending {
                self.apply(app, i);
            }
        }
        panic!("intent loop never settled");
    }

    fn apply(&mut self, app: &mut App, i: ScrollIntent) {
        let id = crate::find::reading_scroll_id;
        match i {
            ScrollIntent::SnapTo { path, view, y } => {
                let mut o = op::snap_to::<()>(
                    id(&path, view),
                    op::RelativeOffset {
                        x: Some(0.0),
                        y: Some(y),
                    },
                );
                self.operate(app, &mut o);
            }
            ScrollIntent::ScrollTo { path, view, y } => {
                let mut o = op::scroll_to::<()>(
                    id(&path, view),
                    op::AbsoluteOffset {
                        x: None,
                        y: Some(y),
                    },
                );
                self.operate(app, &mut o);
            }
            ScrollIntent::Step { path, view, dy } => {
                let mut o =
                    op::scroll_by::<()>(id(&path, view), op::AbsoluteOffset { x: 0.0, y: dy });
                self.operate(app, &mut o);
            }
            ScrollIntent::Page { path, view, down } => {
                let g = self.geometry(app, &path, view).expect("reading scrollable");
                let dy = crate::page::page_delta(g.bounds.height, down);
                let mut o =
                    op::scroll_by::<()>(id(&path, view), op::AbsoluteOffset { x: 0.0, y: dy });
                self.operate(app, &mut o);
            }
            ScrollIntent::SourceRelayout { path, lines } => {
                // After the next layout (the caret move scrolls the editor there).
                self.with(app, |_, _, _, _| ());
                let _ = app.send(Message::Source(crate::message::SourceMsg::Viewport {
                    path,
                    lines,
                }));
            }
            ScrollIntent::Probe {
                path,
                view,
                blocks,
                scroll_to,
            } => {
                let mut p = crate::reading::probe_op(&path, view, blocks);
                self.operate(app, &mut operation::black_box(&mut p));
                let probe = match p.finish() {
                    operation::Outcome::Some(p) => Some(p),
                    _ => None,
                };
                let _ = app.send(Message::Reading(ReadingMsg::Probed {
                    path,
                    view,
                    probe,
                    scroll_to,
                }));
            }
        }
    }

    fn geometry(&mut self, app: &App, path: &str, view: u64) -> Option<Geometry> {
        let mut g = GeomOp {
            id: crate::find::reading_scroll_id(path, view),
            out: None,
        };
        self.operate(app, &mut g);
        g.out
    }
}

#[derive(Clone, Copy, Debug)]
struct Geometry {
    bounds: Rectangle,
    content: Rectangle,
    offset: f32,
}

struct GeomOp {
    id: iced::widget::Id,
    out: Option<Geometry>,
}

impl Operation<()> for GeomOp {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<()>)) {
        operate(self);
    }
    fn scrollable(
        &mut self,
        id: Option<&iced::widget::Id>,
        bounds: Rectangle,
        content: Rectangle,
        t: Vector,
        _s: &mut dyn operation::Scrollable,
    ) {
        if id == Some(&self.id) {
            self.out = Some(Geometry {
                bounds,
                content,
                offset: t.y,
            });
        }
    }
}

/// A measured place: the top block, and the edge when one is reached (`Some(true)` = end).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Place {
    block: usize,
    edge: Option<bool>,
}

fn view(app: &App) -> u64 {
    app.view_of(PATH)
}

fn block_of_line(ui: &Ui, line: usize) -> usize {
    ui.model.block_for_line(line).expect("block")
}

/// Reading's measured place.
fn reading_place(ui: &mut Ui, app: &App) -> Place {
    let v = view(app);
    let g = ui
        .geometry(app, PATH, v)
        .expect("reading scrollable laid out");
    let n = app.reading.doc(PATH).map_or(0, |d| d.model.blocks.len());
    let mut p = crate::reading::probe_op(PATH, v, n);
    ui.operate(app, &mut operation::black_box(&mut p));
    let operation::Outcome::Some(probe) = p.finish() else {
        panic!("probe found no scrollable");
    };
    let max = (g.content.height - g.bounds.height).max(0.0);
    let edge = if g.offset <= 0.5 {
        Some(false)
    } else if g.offset >= max - 0.5 {
        Some(true)
    } else {
        None
    };
    Place {
        block: probe.anchor_block(0.0).unwrap_or(0),
        edge,
    }
}

fn caret_y(im: &InputMethod) -> f32 {
    match im {
        InputMethod::Enabled { cursor, .. } => cursor.y,
        InputMethod::Disabled => panic!("source editor reports no caret (not focused?)"),
    }
}

fn src(app: &mut App) -> &mut iced::widget::text_editor::Content {
    app.doc_mut(PATH).expect("doc").content_mut()
}

fn focus_source(ui: &mut Ui, app: &App) {
    let mut f = operation::focusable::focus::<()>(crate::source::editor_id(PATH));
    ui.operate(app, &mut f);
}

/// Source's measured place: the source line at the editor's viewport top, from the real
/// scroll (caret offset vs. the fully-scrolled-up caret, in visual rows, mapped to a source
/// line by walking visual rows down from the start). Leaves scroll (to the row; the end
/// exactly) and caret as found.
fn source_place(ui: &mut Ui, app: &mut App) -> Place {
    source_place_at(ui, app, 0)
}

/// [`source_place`] of the line `extra` visual rows below the viewport top (plan
/// round-8 WP-4: the anchor height).
fn source_place_at(ui: &mut Ui, app: &mut App, extra: i32) -> Place {
    focus_source(ui, app);
    // With a selection (find in Source) the editor reports the first *visible* selection
    // rectangle to the input method — (0, 0) once the calibration scroll hides it
    // (iced_widget 0.14.2 text_editor.rs:359-364) — so measure with it collapsed (the
    // caret stays put, so the view does not move) and put the selection back below.
    let saved = src(app).cursor();
    src(app).move_to(iced::widget::text_editor::Cursor {
        position: saved.position,
        selection: None,
    });
    let y_a = caret_y(&ui.redraw(app));
    // At the end when one more row down changes nothing.
    src(app).perform(Action::Scroll { lines: 1 });
    let at_end = (caret_y(&ui.redraw(app)) - y_a).abs() < 0.5;
    if !at_end {
        src(app).perform(Action::Scroll { lines: -1 });
    }
    src(app).perform(Action::Scroll { lines: -1_000_000 });
    let y_top = caret_y(&ui.redraw(app));
    src(app).perform(Action::Scroll { lines: 1 });
    let lh = y_top - caret_y(&ui.redraw(app));
    assert!(lh > 1.0, "the note must overflow the source viewport");
    #[allow(clippy::cast_possible_truncation)]
    let rows = ((y_top - y_a) / lh + 0.05).floor().max(0.0) as i32;
    let restore = |ui: &mut Ui, app: &mut App| {
        src(app).perform(Action::Scroll { lines: -1_000_000 });
        let _ = ui.redraw(app);
        let by = if at_end { 1_000_000 } else { rows };
        src(app).perform(Action::Scroll { lines: by });
        let y = caret_y(&ui.redraw(app));
        assert!(
            (y - y_a).abs() < lh,
            "source scroll restored: a {y_a} top {y_top} lh {lh} rows {rows} now {y}"
        );
    };
    // Rows to source line: walk the caret `rows` visual rows down from the start. Twice:
    // lines cosmic has not shaped yet (a find jump skips them) count as one row each on
    // the first walk, which shapes them; the second walk counts wrapped rows exactly.
    for _ in 0..2 {
        src(app).move_to(iced::widget::text_editor::Cursor {
            position: iced::widget::text_editor::Position { line: 0, column: 0 },
            selection: None,
        });
        for _ in 0..rows + extra {
            src(app).perform(Action::Move(Motion::Down));
        }
    }
    let line = src(app).cursor().position.line;
    src(app).move_to(saved);
    let _ = ui.redraw(app); // the caret move scrolls to it here
    restore(ui, app);
    let edge = if rows == 0 {
        Some(false)
    } else if at_end {
        Some(true)
    } else {
        None
    };
    Place {
        block: block_of_line(ui, line),
        edge,
    }
}

fn place(ui: &mut Ui, app: &mut App) -> Place {
    if app.reading.mode(PATH) == ViewMode::Source {
        source_place(ui, app)
    } else {
        reading_place(ui, app)
    }
}

fn setup(mode: ViewMode) -> (tempfile::TempDir, App, Ui) {
    let (tmp, d) = fake();
    let d = Arc::new(d);
    d.create_note(PATH, &body()).expect("create");
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    let _ = intents();
    let mut ui = Ui {
        renderer: super::find_keys::renderer(),
        tree: None,
        model: editor_model::render::RenderModel::default(),
    };
    ui.update(&mut app, Message::Tabs(TabsMsg::Open(PATH.into())));
    assert_eq!(app.active_tab().and_then(|t| t.path()), Some(PATH));
    // Rendered once in Reading, so every mode can map source lines to blocks.
    for m in [ViewMode::Reading, mode] {
        ui.update(
            &mut app,
            Message::Reading(ReadingMsg::SetMode {
                path: PATH.into(),
                mode: m,
            }),
        );
        ui.update(&mut app, Message::Reading(ReadingMsg::Tick));
        if let Some(d) = app.reading.doc(PATH) {
            ui.model = d.model.clone();
        }
    }
    assert!(ui.model.blocks.len() > 100, "reading doc rendered");
    let _ = src(&mut app);
    let _ = ui.redraw(&mut app);
    let _ = ui.redraw(&mut app);
    if mode == ViewMode::Source {
        focus_source(&mut ui, &app);
    }
    (tmp, app, ui)
}

const NOTE_AT: iced::Point = iced::Point::new(W * 0.6, H * 0.5);

fn wheel(ui: &mut Ui, app: &mut App, down: bool, n: usize) {
    let c = mouse::Cursor::Available(NOTE_AT);
    let _ = ui.send(
        app,
        &Event::Mouse(mouse::Event::CursorMoved { position: NOTE_AT }),
        c,
    );
    for _ in 0..n {
        let y = if down { -3.0 } else { 3.0 };
        let _ = ui.send(
            app,
            &Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0.0, y },
            }),
            c,
        );
    }
}

fn key_press(ui: &mut Ui, app: &mut App, k: key::Named) {
    let k = Key::Named(k);
    let _ = ui.send(
        app,
        &Event::Keyboard(keyboard::Event::KeyPressed {
            key: k.clone(),
            modified_key: k,
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
            text: None,
            repeat: false,
        }),
        mouse::Cursor::Unavailable,
    );
}

#[derive(Clone, Copy, Debug)]
enum Input {
    Wheel,
    Drag,
    Page,
    Line,
    Edge,
    Find,
}

/// Applies `input` (`down` = towards the end) the way the user drives it in the current
/// mode; "up" starts from well down the note.
fn drive(ui: &mut Ui, app: &mut App, input: Input, down: bool) {
    let source = app.reading.mode(PATH) == ViewMode::Source;
    if !down {
        wheel(ui, app, true, 40);
    }
    match input {
        Input::Wheel => wheel(ui, app, down, 12),
        Input::Drag => {
            let v = view(app);
            let y = if down { 0.55 } else { 0.25 };
            ui.apply(
                app,
                ScrollIntent::SnapTo {
                    path: PATH.into(),
                    view: v,
                    y,
                },
            );
            ui.update(
                app,
                Message::Reading(ReadingMsg::Scrolled {
                    path: PATH.into(),
                    view: v,
                    y,
                }),
            );
        }
        Input::Page => {
            for _ in 0..4 {
                if source {
                    let k = if down {
                        key::Named::PageDown
                    } else {
                        key::Named::PageUp
                    };
                    key_press(ui, app, k);
                } else {
                    ui.update(app, Message::Panes(crate::message::PanesMsg::Page(down)));
                }
            }
        }
        Input::Line => {
            for _ in 0..30 {
                if source {
                    let k = if down {
                        key::Named::ArrowDown
                    } else {
                        key::Named::ArrowUp
                    };
                    key_press(ui, app, k);
                } else {
                    let m = if down {
                        crate::message::PaletteMsg::Down
                    } else {
                        crate::message::PaletteMsg::Up
                    };
                    ui.update(app, Message::Palette(m));
                }
            }
        }
        Input::Edge => ui.update(app, Message::Reading(ReadingMsg::GoToEdge { bottom: down })),
        Input::Find => {
            ui.update(app, Message::Find(FindMsg::Open));
            ui.update(app, Message::Find(FindMsg::Query("needle".into())));
            for _ in 0..3 {
                let m = if down { FindMsg::Next } else { FindMsg::Prev };
                ui.update(app, Message::Find(m));
            }
        }
    }
    let _ = ui.redraw(app);
}

/// Same top block, or the same edge when the edge was reached.
fn same(a: Place, b: Place) -> bool {
    a.block == b.block || (a.edge.is_some() && a.edge == b.edge)
}

/// Runs one case; `Err(why)` when the place did not survive.
fn case(origin: ViewMode, input: Input, down: bool, switch: bool) -> Result<(), String> {
    let (_tmp, mut app, mut ui) = setup(origin);
    drive(&mut ui, &mut app, input, down);
    let before = place(&mut ui, &mut app);
    if matches!(input, Input::Find) {
        ui.update(&mut app, Message::Find(FindMsg::Close));
        let _ = ui.redraw(&mut app);
        let closed = place(&mut ui, &mut app);
        if !same(before, closed) {
            return Err(format!("find close: {before:?} -> {closed:?}"));
        }
    }
    if !switch {
        return Ok(());
    }
    let to = match origin {
        ViewMode::Source => ViewMode::Reading,
        ViewMode::Reading | ViewMode::Split => ViewMode::Source,
    };
    ui.update(
        &mut app,
        Message::Reading(ReadingMsg::SetMode {
            path: PATH.into(),
            mode: to,
        }),
    );
    let _ = ui.redraw(&mut app);
    let _ = ui.redraw(&mut app);
    let switched = place(&mut ui, &mut app);
    if !same(before, switched) {
        return Err(format!("mode switch: {before:?} -> {switched:?}"));
    }
    Ok(())
}

fn run(cases: &[(ViewMode, Input, bool, bool)]) {
    let mut failed = Vec::new();
    for &(origin, input, down, switch) in cases {
        let dir = if down { "down" } else { "up" };
        let name = format!("{origin:?}/{input:?}/{dir}");
        if let Err(why) = case(origin, input, down, switch) {
            failed.push(format!("{name}: {why}"));
        }
    }
    assert!(
        failed.is_empty(),
        "{} of {} cases failed:\n{}",
        failed.len(),
        cases.len(),
        failed.join("\n")
    );
}

const INPUTS: [Input; 6] = [
    Input::Wheel,
    Input::Drag,
    Input::Page,
    Input::Line,
    Input::Edge,
    Input::Find,
];

/// Reading → Source after Ctrl+PgUp/PgDn lands at the edge; find navigation in Reading
/// and in Split (the no-visible-occurrence path) keeps its place when find closes.
#[test]
fn reading_place_survives_edge_switch_and_find_close() {
    let mut cases = Vec::new();
    for down in [true, false] {
        cases.push((ViewMode::Reading, Input::Edge, down, true));
        cases.push((ViewMode::Reading, Input::Find, down, false));
        cases.push((ViewMode::Split, Input::Find, down, false));
    }
    run(&cases);
}

/// Reading → Source block carry after wheel, scrollbar drag, PageUp/PageDown, line steps
/// and find navigation, both directions.
#[test]
fn reading_block_carries_to_source() {
    let mut cases = Vec::new();
    for input in [
        Input::Wheel,
        Input::Drag,
        Input::Page,
        Input::Line,
        Input::Find,
    ] {
        for down in [true, false] {
            cases.push((ViewMode::Reading, input, down, true));
        }
    }
    run(&cases);
}

/// Source → Reading: wheel, PageUp/PageDown, arrow steps and Ctrl+PgUp/PgDn × both
/// directions.
#[test]
fn source_place_survives_switch_and_find_close() {
    let mut cases = Vec::new();
    for input in INPUTS {
        if matches!(input, Input::Drag | Input::Find) {
            continue;
        }
        for down in [true, false] {
            cases.push((ViewMode::Source, input, down, true));
        }
    }
    run(&cases);
}

/// Source/Wheel → Split: the source pane stays shown, so its top must not move (the
/// switch measures the Source top with a caret hit-test and puts the caret back).
#[test]
fn source_wheel_top_survives_switch_to_split() {
    for down in [true, false] {
        let (_tmp, mut app, mut ui) = setup(ViewMode::Source);
        drive(&mut ui, &mut app, Input::Wheel, down);
        let before = source_place(&mut ui, &mut app);
        ui.update(
            &mut app,
            Message::Reading(ReadingMsg::SetMode {
                path: PATH.into(),
                mode: ViewMode::Split,
            }),
        );
        let _ = ui.redraw(&mut app);
        let _ = ui.redraw(&mut app);
        let after = source_place(&mut ui, &mut app);
        assert!(same(before, after), "down={down}: {before:?} -> {after:?}");
    }
}

/// Plan round-8 WP-4: the place under the 50% anchor (Reading: the probe's anchor block;
/// Source: the line half the measured viewport rows below the top).
fn anchor_place(ui: &mut Ui, app: &mut App) -> Place {
    if app.reading.mode(PATH) == ViewMode::Source {
        let rows = app.doc(PATH).map_or(0, |d| d.visible_lines());
        let half = i32::try_from(crate::source::rows_at(0.5, rows)).unwrap_or(0);
        return source_place_at(ui, app, half);
    }
    let top = reading_place(ui, app);
    let v = view(app);
    let n = app.reading.doc(PATH).map_or(0, |d| d.model.blocks.len());
    let mut p = crate::reading::probe_op(PATH, v, n);
    ui.operate(app, &mut operation::black_box(&mut p));
    let operation::Outcome::Some(probe) = p.finish() else {
        panic!("probe found no scrollable");
    };
    Place {
        block: probe.anchor_block(0.5).unwrap_or(0),
        edge: top.edge,
    }
}

/// Plan round-8 WP-4: with `scroll_anchor = 50`, wheels `n` steps down from `origin`, then
/// switches mode; returns the (before, after) anchor places.
fn anchored_switch(origin: ViewMode, n: usize) -> (Place, Place) {
    let (_tmp, mut app, mut ui) = setup(origin);
    app.prefs.scroll_anchor = 50;
    app.reading.set_anchor(app.prefs.anchor());
    wheel(&mut ui, &mut app, true, n);
    let _ = ui.redraw(&mut app);
    let before = anchor_place(&mut ui, &mut app);
    let to = if origin == ViewMode::Source {
        ViewMode::Reading
    } else {
        ViewMode::Source
    };
    ui.update(
        &mut app,
        Message::Reading(ReadingMsg::SetMode {
            path: PATH.into(),
            mode: to,
        }),
    );
    let _ = ui.redraw(&mut app);
    let _ = ui.redraw(&mut app);
    (before, anchor_place(&mut ui, &mut app))
}

/// Plan round-8 WP-4: mid-document, the block under the 50% anchor survives Reading →
/// Source and Source → Reading.
#[test]
fn anchor_50_carries_mid_document_both_ways() {
    // Wheel counts that put a short block (not a long wrapped paragraph spanning both the
    // top and the anchor) under the anchor, so a top carry would fail here.
    for (origin, n) in [(ViewMode::Reading, 16), (ViewMode::Source, 20)] {
        let (before, after) = anchored_switch(origin, n);
        assert!(before.edge.is_none(), "{origin:?}: mid-document {before:?}");
        assert_eq!(
            before.block, after.block,
            "{origin:?}: {before:?} -> {after:?}"
        );
    }
}

/// Plan round-8 WP-4 (audit M2): a target past the last reachable top lands at the end —
/// the anchor shift is applied to the unclamped target, then clamped once.
#[test]
fn anchor_50_near_end_lands_at_the_end() {
    for origin in [ViewMode::Reading, ViewMode::Source] {
        let (before, after) = anchored_switch(origin, 400);
        assert_eq!(before.edge, Some(true), "{origin:?}: wheeled to the end");
        assert!(
            after.edge == Some(true) || after.block == before.block,
            "{origin:?}: {before:?} -> {after:?}"
        );
    }
}

/// Source find navigation, then find close and a switch to Reading.
#[test]
fn source_find_place_carries_to_reading() {
    run(&[
        (ViewMode::Source, Input::Find, true, true),
        (ViewMode::Source, Input::Find, false, true),
    ]);
}
