//! Plan round-21 WP-1: search-modal Escape (one transition per physical press, on the captured
//! and the ignored path) and the tab strip's keyboard focus / wheel mapping.

use iced::keyboard::{Event, Key, Location, Modifiers, key::Named, key::Physical};

use super::shared_docs::{setup, type_x};
use super::*;
use crate::message::{PaletteMsg, PanelsMsg, SwitcherMsg};
use crate::settings::editor_prefs::SearchLocation;

fn press(n: Named, m: Modifiers) -> Event {
    Event::KeyPressed {
        key: Key::Named(n),
        modified_key: Key::Named(n),
        physical_key: Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified),
        location: Location::Standard,
        modifiers: m,
        text: None,
        repeat: false,
    }
}

/// An Escape no widget captured: `keyboard::listen` feeds both global listeners.
fn escape_ignored(app: &mut App) {
    let ev = press(Named::Escape, Modifiers::empty());
    for m in [
        crate::switcher::key_event(ev.clone()),
        crate::palette::key_event(ev),
    ] {
        let _ = app.send(m.expect("both listeners map Escape"));
    }
}

/// An Escape the focused input captured: only the captured-escape listener sees it.
fn escape_captured(app: &mut App) {
    let ev = iced::Event::Keyboard(press(Named::Escape, Modifiers::empty()));
    let m = crate::switcher::captured_escape(
        ev,
        iced::event::Status::Captured,
        iced::window::Id::unique(),
    );
    let _ = app.send(m.expect("captured Escape maps"));
}

fn modal_app() -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
    let (tmp, d, mut app) = setup(&["se/a.md", "se/b.md"]);
    app.prefs.search_location = SearchLocation::Modal;
    let _ = app.send(Message::Panels(PanelsMsg::Search));
    assert!(app.panels.search_modal);
    let _ = app.send(Message::Panels(PanelsMsg::SearchInput("needle".into())));
    (tmp, d, app)
}

#[test]
fn search_modal_escape_is_two_stage_on_the_ignored_path() {
    let (_t, _d, mut app) = modal_app();
    escape_ignored(&mut app);
    assert!(app.panels.search_modal, "first press only clears");
    assert!(app.panels.search.query.is_empty());
    escape_ignored(&mut app);
    assert!(!app.panels.search_modal, "second press closes");
}

#[test]
fn search_modal_escape_is_two_stage_on_the_captured_path() {
    let (_t, _d, mut app) = modal_app();
    escape_captured(&mut app);
    assert!(app.panels.search_modal && app.panels.search.query.is_empty());
    escape_captured(&mut app);
    assert!(!app.panels.search_modal);
}

#[test]
fn search_modal_empty_query_closes_in_one_press() {
    let (_t, _d, mut app) = modal_app();
    let _ = app.send(Message::Panels(PanelsMsg::SearchInput(String::new())));
    escape_ignored(&mut app);
    assert!(!app.panels.search_modal);
    assert!(!app.switcher.open && !app.palette.open, "no stray modal");
}

#[test]
fn escape_with_switcher_palette_or_dialog_open_is_unchanged() {
    let (_t, _d, mut app) = modal_app();
    let _ = app.send(Message::Switcher(SwitcherMsg::Open));
    assert!(app.switcher.open && !app.panels.search_modal);
    escape_ignored(&mut app);
    assert!(!app.switcher.open, "the switcher closes");
    let _ = app.send(Message::Palette(PaletteMsg::Open));
    assert!(app.palette.open);
    escape_ignored(&mut app);
    assert!(!app.palette.open, "the palette closes");
    let _ = app.send(Message::Panels(PanelsMsg::Search));
    app.dialog = Some(crate::dialog::Dialog::Unsaved {
        losses: crate::dialog::UnsavedEdits::default(),
        then: crate::dialog::Discard::Quit,
    });
    escape_ignored(&mut app);
    assert!(app.dialog.is_none(), "the dialog is cancelled");
    assert!(app.panels.search_modal, "and the modal beneath it stays");
}

#[test]
fn strip_focus_arrows_scroll_the_strip_not_the_editor() {
    let (_t, _d, mut app) = setup(&["st/a.md", "st/b.md"]);
    let pane = app.focus;
    let arrow = |n| crate::palette::key_event(press(n, Modifiers::empty())).expect("mapped");
    // Without strip focus the key is inert for the strip.
    let t = app.send(arrow(Named::ArrowRight));
    assert_eq!(t.units(), 0);
    // A header press focuses the strip (and unfocuses the widgets).
    let t = app.send(Message::Tabs(TabsMsg::DragPress(pane, 0)));
    assert!(t.units() > 0);
    assert_eq!(app.strip_focus, Some(pane));
    let _ = app.send(Message::Tabs(TabsMsg::DragRelease));
    let before = app.active_note().map(|t| t.path.to_owned());
    let text = |app: &App| crate::app::buf(&app.docs, "st/a.md").map(|d| d.text());
    let body = text(&app);
    for n in [Named::ArrowRight, Named::ArrowLeft] {
        let t = app.send(arrow(n));
        assert!(t.units() > 0, "{n:?} scrolls the strip");
    }
    assert_eq!(app.active_note().map(|t| t.path.to_owned()), before);
    assert_eq!(text(&app), body, "the buffer is untouched");
    // Editor input takes the focus away; the arrows are inert again.
    type_x(&mut app);
    assert_eq!(app.strip_focus, None);
    assert_eq!(app.send(arrow(Named::ArrowLeft)).units(), 0);
    // Modified arrows are not strip keys.
    assert!(!matches!(
        crate::palette::key_event(press(Named::ArrowLeft, Modifiers::CTRL)),
        Some(Message::Tabs(TabsMsg::StripKey(_)))
    ));
}

#[test]
fn strip_wheel_maps_vertical_delta_to_a_sideways_offset() {
    use crate::app::scroll::wheel_to_strip_offset as off;
    use iced::mouse::ScrollDelta::{Lines, Pixels};
    assert!(
        off(Lines { x: 0.0, y: -1.0 }) > 0.0,
        "wheel down scrolls right"
    );
    assert!(off(Lines { x: 0.0, y: 1.0 }) < 0.0);
    assert!((off(Pixels { x: 0.0, y: -30.0 }) - 30.0).abs() < f32::EPSILON);
    let (_t, _d, mut app) = setup(&["sw/a.md"]);
    let pane = app.focus;
    let t = app.send(Message::Tabs(TabsMsg::StripWheel(pane, 60.0)));
    assert!(t.units() > 0);
}
