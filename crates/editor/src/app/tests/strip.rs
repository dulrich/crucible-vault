//! Plan round-21 WP-1: the tab strip's keyboard focus, wheel mapping and activation scroll.

use iced::keyboard::{Event, Key, Location, Modifiers, key::Named, key::Physical};

use super::shared_docs::{setup, type_x};
use super::*;
use crate::reading::tests::{app_on, setup as reading_setup};

pub(super) fn press(n: Named, m: Modifiers) -> Event {
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

fn tab_count(app: &App) -> usize {
    app.panes.iter().map(|(_, ps)| ps.tabs.len()).sum()
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

#[test]
fn tab_strip_fraction_and_activation_scroll() {
    use crate::app::scroll::strip_fraction;
    assert_eq!(strip_fraction(0, 1), 0.0);
    assert_eq!(strip_fraction(0, 5), 0.0);
    assert_eq!(strip_fraction(4, 5), 1.0);
    assert!((strip_fraction(2, 5) - 0.5).abs() < f32::EPSILON);
    let files: Vec<(String, Vec<u8>)> = (0..30)
        .map(|i| (format!("n{i}.md"), b"x\n".to_vec()))
        .collect();
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_slice()))
        .collect();
    let (_t, d) = reading_setup(&refs);
    let mut app = app_on(&d, "n0.md");
    for (p, _) in &files[1..] {
        let _ = app.send(Message::Tabs(TabsMsg::Open(p.clone())));
    }
    assert_eq!(tab_count(&app), 30);
    // Opening a tab snaps the strip; the many-tab bar still builds.
    let task = app.send(Message::Tabs(TabsMsg::Open("n3.md".into())));
    assert!(task.units() > 0);
    drop(app.view());
    let _ = app.send(Message::Tabs(TabsMsg::DragPress(app.focus, 5)));
    assert_eq!(app.active_tab().and_then(|t| t.path()), Some("n5.md"));
}
