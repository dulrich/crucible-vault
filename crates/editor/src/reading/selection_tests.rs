//! Plan round-5 WP-7: headless Reading selection over the render model (pattern: the spike
//! harness `docs/spikes/reading-select/src/main.rs` and `crate::tests::find_return_leak`): the
//! real `view::reading` tree on the headless tiny-skia renderer, driven by synthetic events.

use iced::advanced::renderer::Headless;
use iced::advanced::widget::Tree;
use iced::advanced::{Shell, clipboard, layout, mouse};
use iced::keyboard::{self, Key, Location, Modifiers, key};
use iced::{Event, Font, Pixels, Point, Rectangle, Size};

use editor_model::visible::visible_runs as runs;

use super::selection::{self, copy_text};
use super::view::LinkTarget;
use super::*;
use crate::message::{Message, ReadingMsg};
use crate::widget::selectable::{self, RunBox, SelPos, Selection};

const PATH: &str = "sel/a.md";
const VIEW: u64 = 7;
const W: f32 = 800.0;
const H: f32 = 600.0;

fn renderer() -> iced::Renderer {
    let fut = <iced::Renderer as Headless>::new(Font::default(), Pixels(16.0), Some("tiny-skia"));
    let mut fut = std::pin::pin!(fut);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(r) = fut.as_mut().poll(&mut cx) {
            return r.expect("headless renderer");
        }
    }
}

fn buffer(text: &str) -> SourceBuffer {
    SourceBuffer::new(
        PATH,
        text.as_bytes(),
        crate::daemon::ContentHash::of(text.as_bytes()),
    )
}

struct H {
    store: ReadingStore,
    views: crate::views::ViewStore,
    r: iced::Renderer,
    tree: Option<Tree>,
    /// Messages other than the selection's own (link clicks).
    other: Vec<Message>,
    /// `SelectDone` count (PRIMARY writes).
    primary: usize,
}

impl H {
    fn new(text: &str) -> Self {
        let mut store = ReadingStore::new();
        store.register(&buffer(text), ViewMode::Reading, true, None);
        Self {
            store,
            views: crate::views::ViewStore::default(),
            r: renderer(),
            tree: None,
            other: Vec::new(),
            primary: 0,
        }
    }

    fn with<T>(
        &mut self,
        f: impl FnOnce(
            &mut iced::Element<'_, Message>,
            &mut Tree,
            layout::Layout<'_>,
            &iced::Renderer,
        ) -> T,
    ) -> T {
        let t = crate::theme::Surround::Med.tokens().expect("tokens");
        let mut el = view::reading(&self.store, &self.views, PATH, VIEW, t, 1.0, true, None);
        let tree = match &mut self.tree {
            Some(tr) => {
                tr.diff(&el);
                tr
            }
            None => self.tree.insert(Tree::new(&el)),
        };
        let limits = layout::Limits::new(Size::ZERO, Size::new(W, H));
        let node = el.as_widget_mut().layout(tree, &self.r, &limits);
        f(&mut el, tree, layout::Layout::new(&node), &self.r)
    }

    fn send(&mut self, ev: &Event, at: Point) {
        let msgs = self.with(|el, tree, l, r| {
            let mut msgs = Vec::new();
            let mut shell = Shell::new(&mut msgs);
            el.as_widget_mut().update(
                tree,
                ev,
                l,
                mouse::Cursor::Available(at),
                r,
                &mut clipboard::Null,
                &mut shell,
                &Rectangle::with_size(Size::new(W, H)),
            );
            msgs
        });
        for m in msgs {
            match m {
                Message::Reading(rm @ ReadingMsg::Select { .. }) => {
                    let _ = update(&mut self.store, rm, &[], None, None);
                }
                Message::Reading(ReadingMsg::SelectDone { .. }) => self.primary += 1,
                m => self.other.push(m),
            }
        }
    }

    fn boxes(&mut self) -> Vec<RunBox> {
        self.with(selectable::probe_runs::<_, LinkTarget>)
    }

    /// The window point of `pos` (the view is not scrolled: content = window coordinates).
    fn pt(&mut self, pos: SelPos) -> Point {
        let p = self
            .with(|el, tree, l, r| selectable::probe_caret::<_, LinkTarget>(el, tree, l, r, pos))
            .expect("run on screen");
        Point::new(p.x + 0.5, p.y)
    }

    fn mv(&mut self, at: Point) {
        self.send(
            &Event::Mouse(mouse::Event::CursorMoved { position: at }),
            at,
        );
    }

    fn down(&mut self, at: Point) {
        self.mv(at);
        self.send(
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            at,
        );
    }

    fn up(&mut self, at: Point) {
        self.send(
            &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            at,
        );
    }

    fn drag(&mut self, a: Point, b: Point) {
        self.down(a);
        for k in 1..=8 {
            #[allow(clippy::cast_precision_loss)]
            let t = k as f32 / 8.0;
            self.mv(Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t));
        }
        self.up(b);
    }

    fn click(&mut self, at: Point) {
        self.down(at);
        self.up(at);
    }

    fn key(&mut self, named: key::Named, m: Modifiers) {
        let k = Key::Named(named);
        let ev = Event::Keyboard(keyboard::Event::KeyPressed {
            key: k.clone(),
            modified_key: k,
            physical_key: key::Physical::Code(key::Code::ArrowRight),
            location: Location::Standard,
            modifiers: m,
            text: None,
            repeat: false,
        });
        self.send(&ev, Point::ORIGIN);
    }

    fn text(&self) -> Option<String> {
        self.store.selection_text(PATH, VIEW)
    }

    fn run_text(&self, run: usize) -> String {
        self.store.doc(PATH).expect("doc").runs[run].text.clone()
    }
}

#[test]
fn run_table_matches_the_drawn_runs() {
    let md = "---\ntags: [a, b]\ndone: true\n---\n# Head\n\nA [[Link|lbl]] `c` #t\n\n- one\n- [x] two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```\nx\ny\n```\n\n![alt](https://e.x/i.png)\n\n> [!note] Title\n> body\n";
    let mut h = H::new(md);
    let table = h.store.doc(PATH).expect("doc").runs.clone();
    let boxes = h.boxes();
    let drawn: Vec<(usize, usize)> = boxes.iter().map(|b| (b.run, b.len)).collect();
    let want: Vec<(usize, usize)> = table
        .iter()
        .enumerate()
        .filter(|(_, r)| !r.image)
        .map(|(i, r)| (i, r.text.len()))
        .collect();
    assert_eq!(drawn, want);
}

#[test]
fn cross_block_drag_selects_and_copies_with_a_blank_line() {
    let mut h = H::new("First para here\n\nSecond para there\n");
    let (a, b) = (SelPos::new(0, 6), SelPos::new(1, 6));
    let (pa, pb) = (h.pt(a), h.pt(b));
    h.drag(pa, pb);
    assert_eq!(h.store.selection(PATH, VIEW), Some(Selection::new(a, b)));
    assert_eq!(h.text().as_deref(), Some("para here\n\nSecond"));
    assert_eq!(h.primary, 1, "release writes PRIMARY");
    // Dragging into the gap below the last block snaps to the document end.
    let start = h.pt(SelPos::new(0, 0));
    h.drag(start, Point::new(start.x + 40.0, H - 5.0));
    assert_eq!(
        h.text().as_deref(),
        Some("First para here\n\nSecond para there")
    );
}

#[test]
fn double_click_selects_a_word_and_triple_click_the_run() {
    let mut h = H::new("alpha beta gamma\n");
    let at = h.pt(SelPos::new(0, 7));
    h.click(at);
    assert_eq!(h.text(), None, "a single click selects nothing");
    h.click(at);
    assert_eq!(h.text().as_deref(), Some("beta"));
    h.click(at);
    assert_eq!(h.text().as_deref(), Some("alpha beta gamma"));
}

#[test]
fn copy_text_of_list_table_code_and_image() {
    let md = "- one\n- [x] two\n  1. sub\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```\nfn x() {\n    y\n}\n```\n\n![pic](https://e.x/i.png)\n";
    let model = render::lower(md);
    let rs = runs(&model);
    let all = selection::all(&rs).expect("runs");
    assert_eq!(
        copy_text(&rs, all),
        "- one\n- [x] two\n  1. sub\n\na\tb\n1\t2\n\nfn x() {\n    y\n}\n\npic"
    );
    // A selection starting mid-item carries no marker for that item.
    assert_eq!(
        copy_text(&rs, Selection::new(SelPos::new(0, 1), SelPos::new(1, 2))),
        "ne\n- [x] tw"
    );
}

#[test]
fn multiline_code_block_maps_hard_lines_to_copy_offsets() {
    let mut h = H::new("```\nfn a()\nlet b = 1;\nend here\n```\n");
    let code = h.run_text(0);
    assert_eq!(code, "fn a()\nlet b = 1;\nend here");
    let a = code.find("b =").expect("b");
    let b = code.find(" here").expect("end");
    let (pa, pb) = (h.pt(SelPos::new(0, a)), h.pt(SelPos::new(0, b)));
    assert!(pb.y > pa.y, "the end is on a later hard line");
    h.drag(pa, pb);
    assert_eq!(
        h.store.selection(PATH, VIEW),
        Some(Selection::new(SelPos::new(0, a), SelPos::new(0, b)))
    );
    assert_eq!(h.text().as_deref(), Some("b = 1;\nend"));
}

#[test]
fn link_click_without_drag_emits_the_link_message() {
    let mut h = H::new("see [[Target]] now\n");
    let p = h.pt(SelPos::new(0, 5));
    h.down(p);
    h.mv(Point::new(p.x + 2.0, p.y));
    h.up(Point::new(p.x + 2.0, p.y));
    assert!(matches!(
        h.other.as_slice(),
        [Message::Reading(ReadingMsg::OpenLink { target, .. })] if target == "Target"
    ));
    assert_eq!(h.text(), None);
    assert_eq!(h.primary, 0);
    // A drag starting on the link selects instead.
    h.other.clear();
    // (A press away from the last click, so it is not a double-click.)
    let start = h.pt(SelPos::new(0, 7));
    let end = h.pt(SelPos::new(0, h.run_text(0).len()));
    h.drag(start, end);
    assert!(h.other.is_empty(), "a drag is never a link click");
    assert_eq!(h.text().as_deref(), Some("get now"));
}

#[test]
fn shift_arrows_extend_and_escape_clears() {
    let mut h = H::new("one two\n\nthree\n");
    let (a, b) = (h.pt(SelPos::new(0, 0)), h.pt(SelPos::new(0, 3)));
    h.drag(a, b);
    assert_eq!(h.text().as_deref(), Some("one"));
    h.key(key::Named::ArrowRight, Modifiers::SHIFT);
    assert_eq!(h.text().as_deref(), Some("one "));
    h.key(key::Named::ArrowDown, Modifiers::SHIFT);
    let f = h.store.selection(PATH, VIEW).expect("sel").focus;
    assert_eq!(f.run, 1, "Shift+Down moves into the next block");
    h.key(key::Named::Escape, Modifiers::empty());
    assert_eq!(h.store.selection(PATH, VIEW), None);
}

#[test]
fn selection_is_cleared_when_the_doc_re_renders() {
    let mut h = H::new("alpha beta\n");
    h.store.set_selection(
        PATH,
        VIEW,
        Some(Selection::new(SelPos::new(0, 0), SelPos::new(0, 5))),
    );
    assert_eq!(h.text().as_deref(), Some("alpha"));
    h.store.recompute(&buffer("changed text\n"), None);
    assert_eq!(h.store.selection(PATH, VIEW), None);
    assert!(h.boxes().len() == 1);
}

#[test]
fn ctrl_a_and_ctrl_c_take_the_focused_reading_view() {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    std::fs::create_dir_all(vault.join("sel")).expect("mkdir");
    std::fs::write(vault.join(PATH), "one\n\ntwo\n").expect("write");
    let d: Arc<dyn DaemonClient> =
        Arc::new(crate::daemon::FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake"));
    let mut app = crate::app::App::new(Some(Arc::clone(&d)), None);
    let _ = app.send(Message::Tabs(crate::message::TabsMsg::Open(
        PATH.to_owned(),
    )));
    let _ = app.send(Message::Reading(ReadingMsg::SetMode {
        path: PATH.to_owned(),
        mode: ViewMode::Reading,
    }));
    let view = app.view_of(PATH);
    let chord = |s: &str| crate::palette::chord::parse(s).expect("chord");
    let _ = app.send(Message::Palette(crate::message::PaletteMsg::Key(chord(
        "Ctrl+A",
    ))));
    assert_eq!(
        app.reading.selection_text(PATH, view).as_deref(),
        Some("one\n\ntwo")
    );
    // Source mode: Ctrl+A is not a Reading chord.
    let _ = app.send(Message::Reading(ReadingMsg::SetMode {
        path: PATH.to_owned(),
        mode: ViewMode::Source,
    }));
    assert_eq!(app.reading.selection_text(PATH, view), None);
}
