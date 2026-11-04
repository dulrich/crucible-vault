//! Plan round-21 WP-2: search-modal Escape (one transition per physical press, on the captured
//! and the ignored path).

use iced::keyboard::{Modifiers, key::Named};

use super::shared_docs::setup;
use super::strip::press;
use super::*;
use crate::message::{PaletteMsg, PanelsMsg, SwitcherMsg};
use crate::settings::editor_prefs::SearchLocation;

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
