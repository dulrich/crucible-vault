//! Keymap, client commands and find in note.

use iced::widget::text_editor::{Action, Edit};

use super::*;
use crate::message::{ExplorerMsg, FindMsg, PaletteMsg, SourceMsg, TabsMsg};
use crate::palette::chord::{self, Chord};
use crate::reading::ViewMode;

type Fixture = (tempfile::TempDir, Arc<FakeDaemon>, App);

pub(super) fn app_with(notes: &[(&str, &str)]) -> Fixture {
    let (tmp, d) = fake();
    let d = Arc::new(d);
    for (n, body) in notes {
        d.create_note(n, body).expect("create");
    }
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    (tmp, d, app)
}

pub(super) fn open(app: &mut App, p: &str) {
    let _ = app.send(Message::Explorer(ExplorerMsg::Open(p.into())));
}

pub(super) fn type_char(app: &mut App, c: char) {
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Insert(c),
    ))));
}

fn info(id: &str, keys: &[&str]) -> ActionInfo {
    ActionInfo {
        id: id.into(),
        title: id.into(),
        group: None,
        keys: keys.iter().map(|k| (*k).to_owned()).collect(),
        params: vec![],
        needs: vec![],
        durable: false,
    }
}

fn c(s: &str) -> Chord {
    chord::parse(s).expect("chord")
}

/// Registers `id` on the fake daemon, counting its runs.
fn register_counting(d: &Arc<FakeDaemon>, id: &str) -> Arc<std::sync::Mutex<Vec<String>>> {
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    d.register_action(
        info(id, &[]),
        Arc::new(move |p: &RunActionParams| {
            if let Ok(mut g) = seen.lock() {
                g.push(
                    p.params
                        .get("period")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                );
            }
            Ok(None)
        }),
    )
    .expect("register");
    calls
}

#[test]
fn keymap_has_no_duplicate_chord_and_reserves_settings_chords() {
    let map = crate::palette::keymap();
    let mut seen = std::collections::HashSet::new();
    for (k, what) in &map {
        let parsed = chord::parse(k).unwrap_or_else(|| panic!("unparseable {k}"));
        assert!(seen.insert(parsed), "duplicate chord {k} ({what})");
    }
    for k in [
        "Ctrl+Shift+P",
        "Ctrl+P",
        "Ctrl+O",
        "Ctrl+R",
        "Ctrl+F",
        "Ctrl+Shift+F",
        "Ctrl+W",
        "Ctrl+Q",
        "Alt+1",
        "Alt+8",
        "Ctrl+Shift+,",
    ] {
        assert!(chord::reserved(&c(k)), "{k} reserved");
    }
    // plan editor-settings-tab WP-2: Ctrl+, left RESERVED for the Open settings command.
    assert!(!chord::reserved(&c("Ctrl+,")));
    let km = crate::palette::Keymap::default();
    let client = |k: &str| km.get(&c(k)).cloned();
    use crate::palette::{Bound, ClientCmd};
    assert_eq!(
        client("Ctrl+,"),
        Some(Bound::Client(ClientCmd::OpenSettings))
    );
    assert_eq!(
        client("Ctrl+P"),
        Some(Bound::Client(ClientCmd::QuickSwitcher))
    );
    assert_eq!(
        client("Ctrl+Shift+P"),
        Some(Bound::Client(ClientCmd::CommandPaletteKey))
    );
    assert_eq!(client("Alt+9"), None);
}

#[test]
fn actions_toml_key_colliding_with_a_client_default_is_refused() {
    let km = crate::palette::resolve(
        &std::collections::BTreeMap::new(),
        &[info("a", &["ctrl+w"]), info("b", &["ctrl+alt+l"])],
    );
    let (map, notes) = (km.actions(), km.notices);
    assert!(!map.contains_key(&c("Ctrl+W")));
    assert_eq!(map.get(&c("Ctrl+Alt+L")).map(String::as_str), Some("b"));
    // Plan editor-feedback-round-2 WP-1: the collision binds neither claimant.
    assert_eq!(
        notes,
        [
            "close_tab: Ctrl+W conflicts with a (not bound)",
            "a: Ctrl+W conflicts with close_tab (not bound)"
        ]
    );
}

#[test]
fn default_action_chords_run_presets_and_are_noops_when_absent() {
    let (_tmp, d, mut app) = app_with(&[("wp4k/a.md", "x")]);
    // No actions registered: Ctrl+O / Ctrl+R do nothing, and no "Open today" entry exists.
    let _ = app.send(Message::Palette(PaletteMsg::Key(c("Ctrl+O"))));
    let _ = app.send(Message::Palette(PaletteMsg::Key(c("Ctrl+R"))));
    assert!(app.status.error_text().is_none(), "{:?}", app.status.notice);
    assert!(!app.palette.entries.iter().any(|e| e.title == "Open today"));
    let periods = register_counting(&d, "period.open");
    let lints = register_counting(&d, "lint-note");
    app.palette.invalidate();
    let _ = app.send(Message::Palette(PaletteMsg::Key(c("Ctrl+O"))));
    open(&mut app, "wp4k/a.md");
    let _ = app.send(Message::Palette(PaletteMsg::Key(c("Ctrl+R"))));
    assert_eq!(*periods.lock().expect("lock"), ["day"]);
    assert_eq!(lints.lock().expect("lock").len(), 1);
    let today = app
        .palette
        .entries
        .iter()
        .find(|e| e.title == "Open today")
        .expect("listed once period.open is loaded");
    assert_eq!(today.keys, ["Ctrl+O"]);
}

#[test]
fn go_to_tab_selects_in_the_focused_pane_and_ignores_out_of_range() {
    let (_tmp, _d, mut app) = app_with(&[("wp4g/a.md", "a"), ("wp4g/b.md", "b")]);
    open(&mut app, "wp4g/a.md");
    open(&mut app, "wp4g/b.md");
    let _ = app.send(Message::Palette(PaletteMsg::Key(c("Alt+1"))));
    assert_eq!(app.active_tab().and_then(|t| t.path()), Some("wp4g/a.md"));
    let _ = app.send(Message::Tabs(TabsMsg::GoTo(2)));
    assert_eq!(app.active_tab().and_then(|t| t.path()), Some("wp4g/b.md"));
    let _ = app.send(Message::Tabs(TabsMsg::GoTo(5)));
    assert_eq!(app.active_tab().and_then(|t| t.path()), Some("wp4g/b.md"));
}

#[test]
fn find_selects_matches_in_source_with_wrap() {
    let (_tmp, _d, mut app) = app_with(&[("wp4f/a.md", "one Foo\ntwo foo\nfoo")]);
    open(&mut app, "wp4f/a.md");
    let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
        path: "wp4f/a.md".into(),
        mode: ViewMode::Source,
    }));
    let _ = app.send(Message::Palette(PaletteMsg::Key(c("Ctrl+F"))));
    assert!(app.panes.get(app.focus).is_some_and(|ps| ps.find.is_some()));
    let _ = app.send(Message::Find(FindMsg::Query("FOO".into())));
    let sel = |app: &App| app.active_doc().and_then(|t| t.selection());
    let line = |app: &App| app.active_doc().map(|t| t.cursor().position.line);
    assert_eq!(sel(&app).as_deref(), Some("Foo"));
    assert_eq!(line(&app), Some(0));
    let _ = app.send(Message::Find(FindMsg::Submit));
    assert_eq!(line(&app), Some(1));
    let _ = app.send(Message::Find(FindMsg::Next));
    assert_eq!(line(&app), Some(2));
    let _ = app.send(Message::Find(FindMsg::Next));
    assert_eq!(line(&app), Some(0), "wraps forward");
    let _ = app.send(Message::Modifiers(iced::keyboard::Modifiers::SHIFT));
    let _ = app.send(Message::Find(FindMsg::Submit));
    assert_eq!(line(&app), Some(2), "Shift+Enter wraps back");
    let bar = app
        .panes
        .get(app.focus)
        .and_then(|ps| ps.find.clone())
        .expect("bar");
    assert_eq!(bar.count(), "3/3");
    drop(app.view());
    let _ = app.send(Message::Find(FindMsg::Close));
    assert!(app.panes.get(app.focus).is_some_and(|ps| ps.find.is_none()));
}

#[test]
fn source_key_binding_passes_client_chords_through() {
    use iced::keyboard::{Key, Modifiers, key::Physical};
    use iced::widget::text_editor::{Binding, KeyPress, Status};
    let kp = |ch: &str, m: Modifiers, text: Option<&str>| KeyPress {
        key: Key::Character(ch.into()),
        modified_key: Key::Character(ch.into()),
        physical_key: Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified),
        modifiers: m,
        text: text.map(Into::into),
        status: Status::Focused { is_hovered: false },
    };
    let routed = |b: Option<Binding<Message>>| match b {
        Some(Binding::Custom(Message::Palette(PaletteMsg::Key(c)))) => Some(c.to_string()),
        _ => None,
    };
    let cs = Modifiers::CTRL | Modifiers::SHIFT;
    for (ch, m, want) in [
        ("w", Modifiers::CTRL, "Ctrl+W"),
        ("q", Modifiers::CTRL, "Ctrl+Q"),
        ("f", Modifiers::CTRL, "Ctrl+F"),
        ("r", Modifiers::CTRL, "Ctrl+R"),
        ("o", Modifiers::CTRL, "Ctrl+O"),
        ("p", Modifiers::CTRL, "Ctrl+P"),
        ("P", cs, "Ctrl+Shift+P"),
        ("F", cs, "Ctrl+Shift+F"),
        ("1", Modifiers::ALT, "Alt+1"),
        ("8", Modifiers::ALT, "Alt+8"),
        (",", Modifiers::CTRL, "Ctrl+,"),
    ] {
        assert_eq!(
            routed(crate::source::test_key_binding(kp(ch, m, Some(ch)))).as_deref(),
            Some(want),
            "{want}"
        );
    }
    // Copy and plain typing are untouched.
    assert!(matches!(
        crate::source::test_key_binding(kp("c", Modifiers::CTRL, None)),
        Some(Binding::Copy)
    ));
    assert!(matches!(
        crate::source::test_key_binding(kp("9", Modifiers::ALT, Some("9"))),
        Some(Binding::Insert('9'))
    ));
}
