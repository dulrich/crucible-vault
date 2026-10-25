//! Keys pressed in the find bar must never edit the note. Drives
//! the real widget tree (headless tiny-skia renderer) so widget focus is exercised end to end.

use iced::advanced::widget::{Operation, Tree, operation};
use iced::advanced::{Shell, clipboard, layout, mouse, renderer::Headless};
use iced::keyboard::{self, Key, Location, Modifiers, key};
use iced::widget::text_editor::{Action, Motion};
use iced::{Event, Font, Pixels, Rectangle, Size};

use super::*;
use crate::message::{ExplorerMsg, FindMsg, PaletteMsg, SourceMsg};
use crate::reading::ViewMode;

const PATH: &str = "frl/a.md";
const BODY: &str = "- one\n- two foo";

pub(super) fn renderer() -> iced::Renderer {
    let fut = <iced::Renderer as Headless>::new(Font::default(), Pixels(16.0), Some("tiny-skia"));
    let mut fut = std::pin::pin!(fut);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(r) = fut.as_mut().poll(&mut cx) {
            return r.expect("headless renderer");
        }
    }
}

/// A window-less UI: the app's view tree kept across rebuilds like iced's `UserInterface`.
struct Ui {
    renderer: iced::Renderer,
    tree: Option<Tree>,
    /// Window height (800 unless a test needs a tall page on screen).
    height: f32,
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
        let limits = layout::Limits::new(Size::ZERO, Size::new(1200.0, self.height));
        let node = el.as_widget_mut().layout(tree, &self.renderer, &limits);
        f(&mut el, tree, layout::Layout::new(&node), &self.renderer)
    }

    fn focus(&mut self, app: &App, id: iced::widget::Id) {
        self.with(app, |el, tree, l, r| {
            let mut op = operation::focusable::focus::<()>(id);
            el.as_widget_mut()
                .operate(tree, l, r, &mut op as &mut dyn Operation<()>);
        });
    }

    /// Sends `event` through the tree, then feeds the published messages to the app.
    fn send(&mut self, app: &mut App, event: &Event) {
        self.send_at(app, event, mouse::Cursor::Unavailable);
    }

    fn send_at(&mut self, app: &mut App, event: &Event, cursor: mouse::Cursor) {
        let height = self.height;
        let msgs = self.with(app, |el, tree, l, r| {
            let mut msgs = Vec::new();
            let mut shell = Shell::new(&mut msgs);
            let viewport = Rectangle::with_size(Size::new(1200.0, height));
            el.as_widget_mut().update(
                tree,
                event,
                l,
                cursor,
                r,
                &mut clipboard::Null,
                &mut shell,
                &viewport,
            );
            msgs
        });
        for m in msgs {
            // The runtime side of `FindMsg::Focus`'s task (`operation::focus(input)`), which
            // the window-less harness cannot execute from the returned `Task`.
            let refocus = match &m {
                Message::Find(FindMsg::Focus(p)) => Some(crate::find::input_id(*p)),
                _ => None,
            };
            let _ = app.send(m);
            if let Some(id) = refocus {
                self.focus(app, id);
            }
        }
    }
}

impl Ui {
    fn click(&mut self, app: &mut App, at: iced::Point) {
        let c = mouse::Cursor::Available(at);
        self.send_at(
            app,
            &Event::Mouse(mouse::Event::CursorMoved { position: at }),
            c,
        );
        self.send_at(
            app,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            c,
        );
        self.send_at(
            app,
            &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            c,
        );
    }
}

fn press(k: Key, text: Option<&str>) -> Event {
    Event::Keyboard(keyboard::Event::KeyPressed {
        key: k.clone(),
        modified_key: k,
        physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
        location: Location::Standard,
        modifiers: Modifiers::empty(),
        text: text.map(Into::into),
        repeat: false,
    })
}

fn text_of(app: &App) -> String {
    app.active_doc()
        .map(crate::source::SourceBuffer::text)
        .expect("doc")
}

/// A source-mode note with its caret on a list line, the editor focused (the user was
/// typing), then Ctrl+F with a query typed into the bar.
fn setup() -> (tempfile::TempDir, App, Ui) {
    let (tmp, d) = fake();
    let d = Arc::new(d);
    d.create_note(PATH, BODY).expect("create");
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    let _ = app.send(Message::Explorer(ExplorerMsg::Open(PATH.into())));
    let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
        path: PATH.into(),
        mode: ViewMode::Source,
    }));
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Move(
        Motion::DocumentEnd,
    ))));
    let mut ui = Ui {
        renderer: renderer(),
        tree: None,
        height: 800.0,
    };
    ui.focus(&app, crate::source::editor_id(PATH));
    let _ = app.send(Message::Palette(PaletteMsg::Key(
        crate::palette::chord::parse("Ctrl+F").expect("chord"),
    )));
    assert!(app.panes.get(app.focus).is_some_and(|ps| ps.find.is_some()));
    // FindMsg::Open's task: focus the bar's input.
    ui.focus(&app, crate::find::input_id(app.focus));
    let _ = app.send(Message::Find(FindMsg::Query("foo".into())));
    (tmp, app, ui)
}

/// Note at (600, 400); find input at (400, 140); the bar's ChevronDown (Next) at (1131, 141).
const IN_NOTE: iced::Point = iced::Point::new(600.0, 400.0);
const IN_INPUT: iced::Point = iced::Point::new(400.0, 140.0);
const ON_BAR: iced::Point = iced::Point::new(1131.0, 141.0);

#[test]
fn find_input_click_takes_focus_from_the_note() {
    let (_tmp, mut app, mut ui) = setup();
    // Click into the note, then back into the find input: the editor must lose focus.
    ui.click(&mut app, IN_NOTE);
    ui.click(&mut app, IN_INPUT);
    for _ in 0..3 {
        ui.send(&mut app, &press(Key::Named(key::Named::Enter), Some("\r")));
    }
    ui.send(&mut app, &press(Key::Named(key::Named::Tab), Some("\t")));
    ui.send(&mut app, &press(Key::Named(key::Named::Backspace), None));
    ui.send(&mut app, &press(Key::Character("x".into()), Some("x")));
    assert_eq!(text_of(&app), BODY, "find-bar keys leaked into the note");
    let q = app.panes.get(app.focus).and_then(|ps| ps.find.clone());
    assert_eq!(
        q.map(|b| b.query).as_deref(),
        Some("fox"),
        "keys went to the bar"
    );
}

#[test]
fn find_bar_click_then_return_never_edits_the_note() {
    let (_tmp, mut app, mut ui) = setup();
    // Click into the note, then on the bar beside the input (Next chevron), then Return x12.
    ui.click(&mut app, IN_NOTE);
    ui.click(&mut app, ON_BAR);
    for _ in 0..12 {
        ui.send(&mut app, &press(Key::Named(key::Named::Enter), Some("\r")));
    }
    assert_eq!(text_of(&app), BODY, "Return leaked into the note");
}

/// Collects text and text-input bounds (headless locator for widgets without ids).
#[derive(Default)]
struct Spots {
    texts: Vec<(Rectangle, String)>,
    inputs: Vec<Rectangle>,
}

impl Operation<()> for Spots {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<()>)) {
        operate(self);
    }
    fn text(&mut self, _id: Option<&iced::widget::Id>, b: Rectangle, t: &str) {
        self.texts.push((b, t.to_owned()));
    }
    fn text_input(
        &mut self,
        _id: Option<&iced::widget::Id>,
        b: Rectangle,
        _s: &mut dyn operation::TextInput,
    ) {
        self.inputs.push(b);
    }
}

fn spots(ui: &mut Ui, app: &App) -> Spots {
    ui.with(app, |el, tree, l, r| {
        let mut op = Spots::default();
        el.as_widget_mut()
            .operate(tree, l, r, &mut op as &mut dyn Operation<()>);
        op
    })
}

/// Plan round-6-feedback-todos WP-8: a mouse click on an Ignore Shorts typeahead candidate
/// adds the chip, and the list stays open (chip adders keep it open for the next pick).
#[test]
fn ignore_shorts_candidate_click_adds_the_chip() {
    use crate::settings::{Section, SettingsMsg};
    const PIPES: &str = "_custom/crucible-vault/pipelines.toml";
    const SOURCES: &str = "_custom/crucible-vault/sources.toml";
    let (_tmp, d) = fake();
    let d = Arc::new(d);
    d.create_note(SOURCES, "[[youtube]]\nchannel = \"Chan\"\nid = \"UC123\"\n")
        .expect("sources");
    d.create_note(PIPES, "[ignore_shorts]\nchannels = []\n")
        .expect("pipes");
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    let _ = app.send(Message::Settings(SettingsMsg::Open));
    let _ = app.send(Message::Settings(SettingsMsg::Section(Section::Pipelines)));
    let mut ui = Ui {
        renderer: renderer(),
        tree: None,
        height: 3000.0,
    };
    // The chip adder: the 160 px input level with the field's help text.
    let s = spots(&mut ui, &app);
    let help = s
        .texts
        .iter()
        .find(|(_, t)| t == "Channel ids or names")
        .map(|(b, _)| b.center_y())
        .expect("channels help");
    let input = s
        .inputs
        .iter()
        .filter(|b| (b.width - 160.0).abs() < 0.5)
        .min_by(|a, b| {
            (a.center_y() - help)
                .abs()
                .total_cmp(&(b.center_y() - help).abs())
        })
        .copied()
        .expect("chip adder input");
    ui.click(&mut app, input.center());
    ui.send(&mut app, &press(Key::Character("c".into()), Some("c")));
    assert!(
        app.settings
            .form
            .typeahead
            .as_ref()
            .is_some_and(|a| a.ta.open),
        "typing opened the list"
    );
    // A second keystroke still reaches the input (focus survived the list appearing).
    ui.send(&mut app, &press(Key::Character("h".into()), Some("h")));
    let draft = app
        .settings
        .form
        .draft(
            PIPES,
            &crate::settings::form::draft_key(
                "chip",
                &cv_schema::Path::parse("ignore_shorts.channels").expect("p"),
            ),
        )
        .map(str::to_owned);
    assert_eq!(draft.as_deref(), Some("ch"), "input kept focus");
    let s = spots(&mut ui, &app);
    let cand = s
        .texts
        .iter()
        .find(|(_, t)| t == "Chan (UC123)")
        .map(|(b, _)| *b)
        .expect("candidate shown");
    ui.click(&mut app, cand.center());
    let doc = app
        .settings
        .file(PIPES)
        .and_then(|f| f.doc())
        .map(ToString::to_string)
        .unwrap_or_default();
    assert!(doc.contains("\"UC123\""), "chip added: {doc}");
    assert!(
        app.settings
            .form
            .typeahead
            .as_ref()
            .is_some_and(|a| a.ta.open),
        "chip adders keep the list open"
    );
}

/// Plan round-7-feedback-decisions WP-5: Ignore Shorts is a vertical add/drop list. A picked
/// channel leaves the suggestions; after the save lands and its FileChanged echo arrives,
/// typing in the adder again and clicking a candidate still adds it; a remove round-trips to
/// the expected TOML string array.
#[test]
fn ignore_shorts_list_add_again_across_save_echo() {
    use crate::settings::{FormMsg, Section, SettingsMsg};
    const PIPES: &str = "_custom/crucible-vault/pipelines.toml";
    const SOURCES: &str = "_custom/crucible-vault/sources.toml";
    let (_tmp, d) = fake();
    let d = Arc::new(d);
    d.create_note(
        SOURCES,
        "[[youtube]]\nchannel = \"Chan\"\nid = \"UC123\"\n\n[[youtube]]\nchannel = \"Chao\"\nid = \"UC456\"\n",
    )
    .expect("sources");
    d.create_note(PIPES, "[ignore_shorts]\nchannels = []\n")
        .expect("pipes");
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    let _ = app.send(Message::Settings(SettingsMsg::Open));
    let _ = app.send(Message::Settings(SettingsMsg::Section(Section::Pipelines)));
    let mut ui = Ui {
        renderer: renderer(),
        tree: None,
        height: 3000.0,
    };
    let chans = cv_schema::Path::parse("ignore_shorts.channels").expect("p");
    let adder = |ui: &mut Ui, app: &App| {
        let s = spots(ui, app);
        let help = s
            .texts
            .iter()
            .find(|(_, t)| t == "Channel ids or names")
            .map(|(b, _)| b.center_y())
            .expect("channels help");
        s.inputs
            .iter()
            .filter(|b| (b.width - 160.0).abs() < 0.5)
            .min_by(|a, b| {
                (a.center_y() - help)
                    .abs()
                    .total_cmp(&(b.center_y() - help).abs())
            })
            .copied()
            .expect("list adder input")
    };
    let pick = |ui: &mut Ui, app: &mut App, label: &str| {
        let input = adder(ui, app);
        ui.click(app, input.center());
        ui.send(app, &press(Key::Character("c".into()), Some("c")));
        let s = spots(ui, app);
        let cand = s
            .texts
            .iter()
            .find(|(_, t)| t == label)
            .map(|(b, _)| *b)
            .unwrap_or_else(|| panic!("candidate {label} shown"));
        ui.click(app, cand.center());
    };
    let save = |app: &mut App| {
        let _ = app.send(Message::Settings(app.settings.due_now(PIPES)));
        let (bytes, h) = d.read_file(PIPES).expect("saved");
        let _ = app.send(Message::Status(StatusMsg::Notification(
            Notification::FileChanged {
                path: PIPES.to_owned(),
                kind: cv_protocol::ChangeKind::Modified,
                hash: Some(h.as_str().to_owned()),
                origin: cv_protocol::Origin::Daemon,
                from: None,
                replaced_by: None,
            },
        )));
        String::from_utf8(bytes).expect("utf8")
    };
    pick(&mut ui, &mut app, "Chan (UC123)");
    // The picked channel leaves the suggestions.
    let left: Vec<String> = app
        .settings
        .ta_visible(&[])
        .into_iter()
        .map(|c| c.value)
        .collect();
    assert!(!left.iter().any(|v| v == "UC123"), "{left:?}");
    assert!(left.iter().any(|v| v == "UC456"), "{left:?}");
    let text = save(&mut app);
    assert!(text.contains("channels = [\"UC123\"]"), "{text}");
    // Across the save and its echo: pick again.
    pick(&mut ui, &mut app, "Chao (UC456)");
    let text = save(&mut app);
    assert!(text.contains("channels = [\"UC123\", \"UC456\"]"), "{text}");
    // The list shows one row per channel (vertical: distinct rows).
    let s = spots(&mut ui, &app);
    let ys: Vec<f32> = ["Chan (UC123)", "Chao (UC456)"]
        .iter()
        .map(|l| {
            s.texts
                .iter()
                .find(|(_, t)| t == l)
                .map(|(b, _)| b.center_y())
                .expect("row")
        })
        .collect();
    assert!(ys[1] > ys[0] + 10.0, "vertical rows: {ys:?}");
    let _ = app.send(Message::Settings(SettingsMsg::Form(
        PIPES.into(),
        FormMsg::ChipRemove {
            path: chans,
            index: 0,
        },
    )));
    let text = save(&mut app);
    assert!(text.contains("channels = [\"UC456\"]"), "{text}");
}

/// Plan round-7 WP-3: the dedicated Log icon-button opens the Log tab and resets unread; a
/// click on the notice text opens it too.
#[test]
fn log_button_and_notice_open_the_log() {
    let mut app = App::new(None, None);
    app.status
        .notify(crate::app::Severity::Warning, "first trouble");
    let unread = app.status.log.unread;
    assert!(unread >= 1);
    let mut ui = Ui {
        renderer: renderer(),
        tree: None,
        height: 800.0,
    };
    let s = spots(&mut ui, &app);
    let label = format!("☰ Log {unread}");
    let at = s
        .texts
        .iter()
        .find(|(_, t)| *t == label)
        .map(|(b, _)| b.center())
        .expect("log button");
    ui.click(&mut app, at);
    assert!(
        app.active_tab()
            .is_some_and(|t| t.kind == crate::app::TabKind::Log)
    );
    assert_eq!(app.status.log.unread, 0, "opening resets unread");
    // Back to no Log tab focused; the notice itself is clickable.
    let _ = app.send(Message::Tabs(crate::message::TabsMsg::CloseActive));
    assert!(
        !app.active_tab()
            .is_some_and(|t| t.kind == crate::app::TabKind::Log)
    );
    app.status
        .notify(crate::app::Severity::Error, "second trouble");
    let s = spots(&mut ui, &app);
    let at = s
        .texts
        .iter()
        .find(|(_, t)| t.contains("second trouble"))
        .map(|(b, _)| b.center())
        .expect("notice text");
    ui.click(&mut app, at);
    assert!(
        app.active_tab()
            .is_some_and(|t| t.kind == crate::app::TabKind::Log)
    );
    assert_eq!(app.status.log.unread, 0);
}

/// Plan round-12-feedback-todos WP-2: the `[[serial]]` prefix adder keeps focus across
/// keystrokes, and after Enter adds a chip (the strip grows; the input must not move in the tree).
#[test]
fn serial_prefix_adder_keeps_focus_across_keystrokes() {
    use crate::settings::{Section, SettingsMsg};
    const SOURCES: &str = "_custom/crucible-vault/sources.toml";
    let (_tmp, d) = fake();
    let d = Arc::new(d);
    d.create_note(SOURCES, "[[serial]]\nname = \"Work\"\n")
        .expect("sources");
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    let _ = app.send(Message::Settings(SettingsMsg::Open));
    let _ = app.send(Message::Settings(SettingsMsg::Section(Section::Sources)));
    app.settings.form.expanded.insert((
        SOURCES.to_owned(),
        cv_schema::Path::parse("serial[0]").expect("p"),
    ));
    let mut ui = Ui {
        renderer: renderer(),
        tree: None,
        height: 3000.0,
    };
    let s = spots(&mut ui, &app);
    let help = s
        .texts
        .iter()
        .find(|(_, t)| t.starts_with("Source-URL prefixes"))
        .map(|(b, _)| b.center_y())
        .expect("prefix help");
    let input = s
        .inputs
        .iter()
        .filter(|b| (b.width - 160.0).abs() < 0.5)
        .min_by(|a, b| {
            (a.center_y() - help)
                .abs()
                .total_cmp(&(b.center_y() - help).abs())
        })
        .copied()
        .expect("prefix adder input");
    ui.click(&mut app, input.center());
    ui.send(&mut app, &press(Key::Character("h".into()), Some("h")));
    ui.send(&mut app, &press(Key::Character("t".into()), Some("t")));
    let draft = app
        .settings
        .form
        .draft(
            SOURCES,
            &crate::settings::form::draft_key(
                "chip",
                &cv_schema::Path::parse("serial[0].prefix").expect("p"),
            ),
        )
        .map(str::to_owned);
    assert_eq!(draft.as_deref(), Some("ht"), "input kept focus");
    // Enter adds the chip (the strip grows), and the next keystroke still reaches the input.
    ui.send(&mut app, &press(Key::Named(key::Named::Enter), Some("\r")));
    ui.send(&mut app, &press(Key::Character("x".into()), Some("x")));
    ui.send(&mut app, &press(Key::Character("y".into()), Some("y")));
    let draft = app
        .settings
        .form
        .draft(
            SOURCES,
            &crate::settings::form::draft_key(
                "chip",
                &cv_schema::Path::parse("serial[0].prefix").expect("p"),
            ),
        )
        .map(str::to_owned);
    assert_eq!(
        draft.as_deref(),
        Some("xy"),
        "input kept focus after a chip was added"
    );
}

/// The prefix adder of `serial[0]` as laid out (the width-160 input nearest the prefix help).
fn serial_prefix_adder(ui: &mut Ui, app: &App) -> Option<Rectangle> {
    let s = spots(ui, app);
    let help = s
        .texts
        .iter()
        .find(|(_, t)| t.starts_with("Source-URL prefixes"))
        .map(|(b, _)| b.center_y())?;
    s.inputs
        .iter()
        .filter(|b| (b.width - 160.0).abs() < 0.5 || b.height < 1.0)
        .min_by(|a, b| {
            (a.center_y() - help)
                .abs()
                .total_cmp(&(b.center_y() - help).abs())
        })
        .copied()
}

fn assert_on_screen(b: Rectangle, what: &str) {
    assert!(
        b.width >= 100.0 && b.height >= 20.0 && b.x >= 0.0 && b.x + b.width <= 1200.0,
        "{what}: prefix adder not visible: {b:?}"
    );
}

/// User report 2026-10-24 (after round-12 WP-2): an existing serial with prefixes shows no adder
/// to add another, and in a new serial the adder disappears once Return adds the first chip.
#[test]
fn serial_prefix_adder_stays_visible_with_chips() {
    use crate::settings::{Section, SettingsMsg};
    const SOURCES: &str = "_custom/crucible-vault/sources.toml";
    let (_tmp, d) = fake();
    let d = Arc::new(d);
    d.create_note(
        SOURCES,
        "[[serial]]\nname = \"Old\"\nprefix = [\"https://www.example-serial-host.com/stories/a-long-story-name/\", \"https://forums.example.net/threads/another-long-thread-title.12345/\"]\n\n[[serial]]\nname = \"New\"\n",
    )
    .expect("sources");
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    let _ = app.send(Message::Settings(SettingsMsg::Open));
    let _ = app.send(Message::Settings(SettingsMsg::Section(Section::Sources)));
    let mut ui = Ui {
        renderer: renderer(),
        tree: None,
        height: 3000.0,
    };
    // Existing serial with two prefixes (sorted display: "New" is row 0 on screen, file index 1).
    app.settings.form.expanded.insert((
        SOURCES.to_owned(),
        cv_schema::Path::parse("serial[0]").expect("p"),
    ));
    let b = serial_prefix_adder(&mut ui, &app).expect("adder (existing)");
    assert_on_screen(b, "existing serial with prefixes");
    app.settings.form.expanded.clear();
    // New serial: type a prefix, Return.
    app.settings.form.expanded.insert((
        SOURCES.to_owned(),
        cv_schema::Path::parse("serial[1]").expect("p"),
    ));
    let b = serial_prefix_adder(&mut ui, &app).expect("adder (new)");
    assert_on_screen(b, "new serial, no chips");
    ui.click(&mut app, b.center());
    for c in "https://www.example-serial-host.com/stories/x/".chars() {
        let s = c.to_string();
        ui.send(
            &mut app,
            &press(Key::Character(s.as_str().into()), Some(&s)),
        );
    }
    ui.send(&mut app, &press(Key::Named(key::Named::Enter), Some("\r")));
    let b = serial_prefix_adder(&mut ui, &app).expect("adder (after Return)");
    assert_on_screen(b, "new serial after Return");
}

/// The text inputs of the Settings page as `(bounds, value)` via the headless locator.
fn input_values(ui: &mut Ui, app: &App) -> Vec<(Rectangle, String)> {
    struct Vals(Vec<(Rectangle, String)>);
    impl Operation<()> for Vals {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<()>)) {
            operate(self);
        }
        fn text_input(
            &mut self,
            _id: Option<&iced::widget::Id>,
            b: Rectangle,
            s: &mut dyn operation::TextInput,
        ) {
            self.0.push((b, s.text().to_owned()));
        }
    }
    ui.with(app, |el, tree, l, r| {
        let mut op = Vals(Vec::new());
        el.as_widget_mut()
            .operate(tree, l, r, &mut op as &mut dyn Operation<()>);
        op.0
    })
}

/// Plan round-13-feedback-todos WP-1 (user report 2026-10-25: Settings "flashing" while editing
/// Serials): typing a name that changes its sort position must neither move the edited item on
/// screen nor move focus to another item's input, keystroke to keystroke.
#[test]
fn serial_name_typing_keeps_position_and_focus() {
    use crate::settings::{Section, SettingsMsg};
    const SOURCES: &str = "_custom/crucible-vault/sources.toml";
    let (_tmp, d) = fake();
    let d = Arc::new(d);
    d.create_note(
        SOURCES,
        "[[serial]]\nname = \"Alpha\"\n\n[[serial]]\nname = \"Mid\"\n\n[[serial]]\nname = \"A\"\n",
    )
    .expect("sources");
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    let _ = app.send(Message::Settings(SettingsMsg::Open));
    let _ = app.send(Message::Settings(SettingsMsg::Section(Section::Sources)));
    // The third serial ("A", displayed first) is open; "Ax..." sorts after "Alpha".
    app.settings.form.expanded.insert((
        SOURCES.to_owned(),
        cv_schema::Path::parse("serial[2]").expect("p"),
    ));
    let mut ui = Ui {
        renderer: renderer(),
        tree: None,
        height: 3000.0,
    };
    let name_input = |ui: &mut Ui, app: &App, v: &str| {
        input_values(ui, app)
            .into_iter()
            .find(|(_, t)| t == v)
            .map(|(b, _)| b)
    };
    let start = name_input(&mut ui, &app, "A").expect("name input");
    ui.click(&mut app, start.center());
    for (i, c) in ["x", "y", "z"].into_iter().enumerate() {
        ui.send(&mut app, &press(Key::Character(c.into()), Some(c)));
        let want = format!("A{}", "xyz".split_at(i + 1).0);
        let b = name_input(&mut ui, &app, &want)
            .unwrap_or_else(|| panic!("keystroke {i}: no input shows {want:?} (focus moved?)"));
        assert!(
            (b.y - start.y).abs() < 0.5,
            "keystroke {i}: the edited serial moved from y={} to y={}",
            start.y,
            b.y
        );
    }
    let doc = app
        .settings
        .file(SOURCES)
        .and_then(|f| f.doc())
        .expect("doc");
    let names: Vec<_> = (0..3)
        .map(|i| {
            doc.get_str(&cv_schema::Path::parse(&format!("serial[{i}].name")).expect("p"))
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert_eq!(
        names,
        ["Alpha", "Mid", "Axyz"],
        "only the edited serial changed"
    );
}

/// Plan round-13-feedback-todos WP-2 (E1): the subscription's mapping yields the traversal
/// message and the harness runs the focus operation itself (`Ui::send` discards Tasks and runs
/// no subscriptions).
fn traverse(ui: &mut Ui, app: &mut App, back: bool) {
    let mods = if back {
        Modifiers::SHIFT
    } else {
        Modifiers::empty()
    };
    let ev = keyboard::Event::KeyPressed {
        key: Key::Named(key::Named::Tab),
        modified_key: Key::Named(key::Named::Tab),
        physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
        location: Location::Standard,
        modifiers: mods,
        text: None,
        repeat: false,
    };
    assert!(app.settings_traversal_active(), "listener is subscribed");
    let m = crate::settings::traverse_key_event(&ev).expect("Tab maps to Traverse");
    let _ = app.send(m);
    ui.with(app, |el, tree, l, r| {
        let mut op: Box<dyn Operation<()>> = if back {
            Box::new(operation::focusable::focus_previous::<()>())
        } else {
            Box::new(operation::focusable::focus_next::<()>())
        };
        // The runtime's loop: a `Chain` outcome (count pass, then focus pass) runs again.
        loop {
            el.as_widget_mut().operate(tree, l, r, op.as_mut());
            match op.finish() {
                operation::Outcome::Chain(next) => op = next,
                _ => break,
            }
        }
    });
}

#[test]
fn settings_tab_and_shift_tab_move_between_inputs() {
    use crate::settings::{Section, SettingsMsg};
    const SOURCES: &str = "_custom/crucible-vault/sources.toml";
    let (_tmp, d) = fake();
    let d = Arc::new(d);
    d.create_note(
        SOURCES,
        "[[serial]]\nname = \"Alpha\"\n\n[[serial]]\nname = \"Mid\"\n",
    )
    .expect("sources");
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    let _ = app.send(Message::Settings(SettingsMsg::Open));
    let _ = app.send(Message::Settings(SettingsMsg::Section(Section::Sources)));
    app.settings.form.expanded.insert((
        SOURCES.to_owned(),
        cv_schema::Path::parse("serial[0]").expect("p"),
    ));
    let mut ui = Ui {
        renderer: renderer(),
        tree: None,
        height: 3000.0,
    };
    let find = |ui: &mut Ui, app: &App, v: &str| {
        input_values(ui, app)
            .into_iter()
            .find(|(_, t)| t == v)
            .map(|(b, _)| b)
    };
    let start = find(&mut ui, &app, "Alpha").expect("name input");
    ui.click(&mut app, start.center());
    traverse(&mut ui, &mut app, false);
    ui.send(&mut app, &press(Key::Character("q".into()), Some("q")));
    assert!(
        find(&mut ui, &app, "Alpha").is_some(),
        "Tab left the name input: it is unchanged {:?}",
        input_values(&mut ui, &app)
            .iter()
            .map(|x| x.1.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        input_values(&mut ui, &app)
            .iter()
            .any(|(_, t)| t.contains('q')),
        "the next input received the typed character"
    );
    traverse(&mut ui, &mut app, true);
    ui.send(&mut app, &press(Key::Character("z".into()), Some("z")));
    assert!(
        find(&mut ui, &app, "Alphaz").is_some(),
        "Shift+Tab returned to the name input"
    );
}

#[test]
fn settings_tab_listener_is_off_for_notes() {
    use crate::settings::SettingsMsg;
    let (_tmp, app, _ui) = setup();
    assert!(
        !app.settings_traversal_active(),
        "a note tab keeps Tab (indent / typeahead)"
    );
    let mut app = app;
    let _ = app.send(Message::Settings(SettingsMsg::Open));
    assert!(app.settings_traversal_active());
    let _ = app.send(Message::Palette(PaletteMsg::Open));
    assert!(!app.settings_traversal_active(), "palette keeps Tab");
}
