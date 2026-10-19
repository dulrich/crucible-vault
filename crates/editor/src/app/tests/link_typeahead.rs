//! The `[[` note typeahead in Source.

use iced::keyboard;
use iced::widget::text_editor::{self, Action, Binding, Edit, KeyPress};

use super::*;
use crate::message::{ExplorerMsg, LinkKey, ReadingMsg, SourceMsg};
use crate::reading::ViewMode;

type Fixture = (tempfile::TempDir, Arc<FakeDaemon>, App);

const NOTE: &str = "w9/host.md";

fn setup() -> Fixture {
    let (tmp, d) = fake();
    let d = Arc::new(d);
    d.create_note(NOTE, "").expect("create");
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    let _ = app.send(Message::Explorer(ExplorerMsg::Open(NOTE.into())));
    let _ = app.send(Message::Reading(ReadingMsg::SetMode {
        path: NOTE.into(),
        mode: ViewMode::Source,
    }));
    app.explorer.paths = [NOTE, "a/Alpha.md", "b/Beta.md", "x/Gamma.md", "y/Gamma.md"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    (tmp, d, app)
}

fn typ(app: &mut App, s: &str) {
    for c in s.chars() {
        let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
            Edit::Insert(c),
        ))));
    }
}

fn key(app: &mut App, k: LinkKey) {
    let _ = app.send(Message::Source(SourceMsg::LinkKey(k)));
}

fn text(app: &App) -> String {
    app.doc(NOTE).expect("doc").text()
}

fn labels(app: &App) -> Vec<String> {
    app.link_ta
        .as_ref()
        .expect("open")
        .visible
        .iter()
        .map(|c| c.value.clone())
        .collect()
}

#[test]
fn double_bracket_opens_and_typing_narrows() {
    let (_t, _d, mut app) = setup();
    typ(&mut app, "see [");
    assert!(app.link_ta.is_none());
    typ(&mut app, "[");
    assert!(labels(&app).len() >= 4);
    typ(&mut app, "alp");
    assert_eq!(labels(&app).first().map(String::as_str), Some("a/Alpha.md"));
    assert_eq!(app.link_ta.as_ref().map(|a| a.query.as_str()), Some("alp"));
    typ(&mut app, "]]");
    assert!(app.link_ta.is_none(), "`]]` closes");
}

#[test]
fn enter_inserts_stem_or_path_form_and_closes() {
    let (_t, _d, mut app) = setup();
    typ(&mut app, "[[alph");
    key(&mut app, LinkKey::Enter);
    assert!(app.link_ta.is_none());
    assert_eq!(text(&app), "[[Alpha]]");
    assert_eq!(app.doc(NOTE).expect("doc").cursor().position.column, 9);
    // Namesakes: x/Gamma.md wins the stem, y/Gamma.md gets the path form.
    typ(&mut app, " [[y/gamma");
    assert_eq!(labels(&app).first().map(String::as_str), Some("y/Gamma.md"));
    key(&mut app, LinkKey::Tab);
    assert_eq!(text(&app), "[[Alpha]] [[y/Gamma]]");
}

#[test]
fn escape_closes_and_keeps_text_keys_do_not_leak() {
    let (_t, _d, mut app) = setup();
    typ(&mut app, "[[que");
    key(&mut app, LinkKey::Down);
    key(&mut app, LinkKey::Up);
    key(&mut app, LinkKey::Down);
    assert_eq!(text(&app), "[[que");
    key(&mut app, LinkKey::Escape);
    assert!(app.link_ta.is_none());
    assert_eq!(text(&app), "[[que");
    // The binding: with the list open, the keys become LinkKeys (never buffer edits).
    let kp = |k: keyboard::key::Named| KeyPress {
        key: keyboard::Key::Named(k),
        modified_key: keyboard::Key::Named(k),
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        modifiers: keyboard::Modifiers::empty(),
        text: None,
        status: text_editor::Status::Focused { is_hovered: false },
    };
    for (n, want) in [
        (keyboard::key::Named::ArrowUp, LinkKey::Up),
        (keyboard::key::Named::ArrowDown, LinkKey::Down),
        (keyboard::key::Named::Enter, LinkKey::Enter),
        (keyboard::key::Named::Tab, LinkKey::Tab),
        (keyboard::key::Named::Escape, LinkKey::Escape),
    ] {
        assert!(matches!(
            crate::source::test_key_binding_link(kp(n)),
            Some(Binding::Custom(Message::Source(SourceMsg::LinkKey(k)))) if k == want
        ));
    }
    assert!(!matches!(
        crate::source::test_key_binding(kp(keyboard::key::Named::ArrowDown)),
        Some(Binding::Custom(Message::Source(SourceMsg::LinkKey(_))))
    ));
}

#[test]
fn zero_candidates_enter_falls_through_and_backspace_past_closes() {
    let (_t, _d, mut app) = setup();
    typ(&mut app, "[[zzzqqq");
    assert!(labels(&app).is_empty());
    key(&mut app, LinkKey::Enter);
    assert!(app.link_ta.is_none());
    assert_eq!(text(&app), "[[zzzqqq\n");
    typ(&mut app, "[[");
    assert!(app.link_ta.is_some());
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Backspace,
    ))));
    assert!(app.link_ta.is_none());
}

#[test]
fn works_on_split_source_side_and_closes_on_reading() {
    let (_t, _d, mut app) = setup();
    let _ = app.send(Message::Reading(ReadingMsg::SetMode {
        path: NOTE.into(),
        mode: ViewMode::Split,
    }));
    typ(&mut app, "[[bet");
    assert_eq!(labels(&app).first().map(String::as_str), Some("b/Beta.md"));
    key(&mut app, LinkKey::Enter);
    assert_eq!(text(&app), "[[Beta]]");
    typ(&mut app, "[[");
    assert!(app.link_ta.is_some());
    let _ = app.send(Message::Reading(ReadingMsg::SetMode {
        path: NOTE.into(),
        mode: ViewMode::Reading,
    }));
    assert!(app.link_ta.is_none());
}
