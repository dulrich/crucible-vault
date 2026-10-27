use super::*;

/// plan editor-ui-round-3 WP-5 (audit E1): the active find match maps to (block, nth occurrence).
#[test]
fn active_find_match_maps_to_its_block_and_occurrence() {
    use crate::message::FindMsg;
    let p = "wp5/f.md";
    let (_t, d) = setup(&[(p, b"alpha foo\n\nbeta foo foo\n")]);
    let mut app = app_on(&d, p);
    reading(&mut app, p, ViewMode::Reading);
    let _ = app.send(Message::Reading(ReadingMsg::Tick));
    let _ = app.send(Message::Reading(ReadingMsg::Tick));
    assert!(app.reading.doc(&app.docs, p).is_some(), "rendered");
    let _ = app.send(Message::Find(FindMsg::Open));
    let _ = app.send(Message::Find(FindMsg::Query("foo".into())));
    assert_eq!(strong_hit(&app, p), Some((0, 0)));
    let _ = app.send(Message::Find(FindMsg::Next));
    assert_eq!(
        strong_hit(&app, p),
        Some((1, 0)),
        "second block, first occurrence"
    );
    let _ = app.send(Message::Find(FindMsg::Next));
    assert_eq!(strong_hit(&app, p), Some((1, 1)));
    // Painting: exactly one occurrence of the second block is the active one.
    drop(app.view());
    let _ = app.send(Message::Find(FindMsg::Close));
    assert_eq!(strong_hit(&app, p), None);
}

const PLACE_NOTE: &[u8] =
    b"---\na: 1\n---\n# H\n\npara\n\n- x\n- y\n\n```\ncode\nmore\n```\n\nlast\n";

/// plan editor-ui-round-3 WP-6: Source -> Reading carries the Source top line to its block.
#[test]
fn source_to_reading_carries_top_line_to_its_block() {
    let p = "wp6/s.md";
    let (_t, d) = setup(&[(p, PLACE_NOTE)]);
    let mut app = app_on(&d, p);
    reading(&mut app, p, ViewMode::Source);
    // Plan round-5 WP-1: a 3-line viewport, so line 11 is a reachable top.
    viewport(&mut app, p, 3);
    // Wheel down 11 lines: line 11 ("code") is at the top.
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Scroll {
        lines: 11,
    })));
    assert_eq!(app.active_doc().expect("doc").top_line(), 11);
    reading(&mut app, p, ViewMode::Reading);
    let model = &app.reading.doc(&app.docs, p).expect("doc").model;
    assert_eq!(model.block_for_line(11), Some(3), "the code block");
    assert_eq!(
        app.reading.top_block(&app.docs, p, app.view_of(p)),
        Some(3),
        "the scroll task targets it"
    );
}

/// plan editor-ui-round-3 WP-6: Reading -> Source scrolls Source to the top block's first line.
#[test]
fn reading_to_source_scrolls_to_the_top_blocks_first_line() {
    let p = "wp6/r.md";
    let (_t, d) = setup(&[(p, PLACE_NOTE)]);
    let mut app = app_on(&d, p);
    assert_eq!(app.reading.mode(&app.docs, p), ViewMode::Reading);
    let blocks = app
        .reading
        .doc(&app.docs, p)
        .expect("doc")
        .model
        .blocks
        .len();
    // A probe lands with the list (block 2, source lines 7..9) at the viewport top.
    let mut block_ys: Vec<Option<f32>> = (0..blocks).map(|i| Some(i as f32 * 50.0)).collect();
    block_ys[0] = Some(30.0);
    let view = app.view_of(p);
    let _ = app.send(Message::Reading(ReadingMsg::Probed {
        path: p.to_owned(),
        view,
        probe: Some(Probe {
            offset_y: 100.0,
            content_y: 0.0,
            block_ys,
            ..Probe::default()
        }),
        scroll_to: None,
    }));
    assert_eq!(app.reading.top_block(&app.docs, p, app.view_of(p)), Some(2));
    viewport(&mut app, p, 3);
    reading(&mut app, p, ViewMode::Source);
    assert_eq!(app.active_doc().expect("doc").top_line(), 7);
    // And back: Source line 7 maps to the list again.
    reading(&mut app, p, ViewMode::Split);
    assert_eq!(app.reading.top_block(&app.docs, p, app.view_of(p)), Some(2));
}

/// The live daemon indexes incrementally without bumping the generation, so a generation-keyed
/// resolver never saw files created after the first render (localized attachments rendered as
/// broken images until restart). Created/Deleted/Renamed drop the cache; Modified keeps it.
#[test]
fn file_membership_change_drops_cached_resolver() {
    use cv_protocol::{ChangeKind, Notification, Origin};

    use crate::message::StatusMsg;

    let (_tmp, d) = setup(&[("A.md", b"![](att/late.png)\n")]);
    let mut app = app_on(&d, "A.md");
    reading(&mut app, "A.md", ViewMode::Reading);
    let changed = |app: &mut App, path: &str, kind: ChangeKind| {
        let _ = app.send(Message::Status(StatusMsg::Notification(
            Notification::FileChanged {
                path: path.to_owned(),
                kind,
                hash: None,
                origin: Origin::Daemon,
                from: None,
                replaced_by: None,
            },
        )));
    };
    for kind in [
        ChangeKind::Created,
        ChangeKind::Deleted,
        ChangeKind::Renamed,
    ] {
        app.resolver.refresh(d.as_ref());
        assert!(app.resolver.is_built());
        changed(&mut app, "att/late.png", kind);
        assert!(!app.resolver.is_built(), "{kind:?} drops the resolver");
    }
    app.resolver.refresh(d.as_ref());
    changed(&mut app, "B.md", ChangeKind::Modified);
    assert!(app.resolver.is_built(), "Modified keeps the resolver");
}

/// Reading and the panels share one resolver cache — one Created notification
/// invalidates it for both (same `Arc`, one epoch/version bump), and a rebuild by either is
/// seen by the other.
#[test]
fn one_vault_change_invalidates_the_shared_resolver_for_both_consumers() {
    use cv_protocol::{ChangeKind, Notification, Origin};

    use crate::message::StatusMsg;

    let (_tmp, d) = setup(&[("A.md", b"[[B]]\n"), ("B.md", b"b\n")]);
    let mut app = app_on(&d, "A.md");
    assert!(Arc::ptr_eq(&app.reading.resolver, &app.resolver));
    assert!(Arc::ptr_eq(&app.panels.resolver, &app.resolver));
    app.reading.resolver.refresh(d.as_ref());
    assert!(
        app.panels.resolver.is_built(),
        "a Reading rebuild serves the panels"
    );
    assert_eq!(
        app.panels.resolver.resolve("A.md", "B").as_deref(),
        Some("B.md")
    );
    let (epoch, version) = (app.resolver.epoch(), app.resolver.version());
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            path: "C.md".to_owned(),
            kind: ChangeKind::Created,
            hash: None,
            origin: Origin::Daemon,
            from: None,
            replaced_by: None,
        },
    )));
    assert!(!app.reading.resolver.is_built(), "Reading sees the drop");
    assert!(!app.panels.resolver.is_built(), "the panels see the drop");
    assert_eq!(app.resolver.epoch(), epoch + 1, "one invalidation");
    assert!(app.resolver.version() > version);
}

/// plan editor-round-4 WP-2: Ctrl+PgDn in Source tracks the last viewport. Plan round-6 WP-1:
/// the Source -> Reading half (it carried the window's first block, para 81) is now an edge
/// snap — see `source_go_to_end_then_reading_snaps_to_the_end`.
#[test]
fn go_to_bottom_in_source_tracks_the_last_viewport() {
    let p = "r4/long.md";
    let body = long_note();
    let (_t, d) = setup(&[(p, &body)]);
    let mut app = app_on(&d, p);
    reading(&mut app, p, ViewMode::Source);
    // Plan round-5 WP-1: the measured viewport (40 lines), not the 30-line estimate.
    viewport(&mut app, p, 40);
    let _ = app.send(Message::Reading(ReadingMsg::GoToEdge { bottom: true }));
    let top = app.active_doc().expect("doc").top_line();
    assert_eq!(top, 200 - 39, "the last 40 lines are the visible window");
}

/// plan editor-round-4 WP-2: Reading at its last block, then Reading -> Source lands on the
/// last lines.
#[test]
fn reading_at_bottom_then_source_lands_on_the_last_lines() {
    let p = "r4/long2.md";
    let body = long_note();
    let (_t, d) = setup(&[(p, &body)]);
    let mut app = app_on(&d, p);
    let blocks = app
        .reading
        .doc(&app.docs, p)
        .expect("doc")
        .model
        .blocks
        .len();
    let block_ys: Vec<Option<f32>> = (0..blocks).map(|i| Some(i as f32 * 20.0)).collect();
    let view = app.view_of(p);
    let _ = app.send(Message::Reading(ReadingMsg::Probed {
        path: p.to_owned(),
        view,
        probe: Some(Probe {
            offset_y: (blocks - 1) as f32 * 20.0,
            content_y: 0.0,
            block_ys,
            ..Probe::default()
        }),
        scroll_to: None,
    }));
    assert_eq!(
        app.reading.top_block(&app.docs, p, app.view_of(p)),
        Some(blocks - 1)
    );
    // Plan round-5 WP-1: the last block starts at line 198, but the editor cannot scroll past
    // its end — the carry clamps to the last full viewport (cause B: it used to set 198 and
    // show two lines and blank space).
    viewport(&mut app, p, 40);
    reading(&mut app, p, ViewMode::Source);
    let doc = app.active_doc().expect("doc");
    let lines = doc.text().matches('\n').count() + 1;
    assert_eq!(doc.top_line(), lines - 40);
    assert_eq!(
        doc.top_line() + doc.visible_lines(),
        lines,
        "the window ends at the last line"
    );
}

/// Plan round-5 WP-1 (item 1, cause b): two panes showing one note page / probe their own
/// reading scrollable — the id was per path, so a PageDown scrolled both (and a probe read the
/// last match).
#[test]
fn reading_scroll_ids_are_per_tab_not_per_path() {
    assert_ne!(
        crate::find::reading_scroll_id("a.md", 1),
        crate::find::reading_scroll_id("a.md", 2)
    );
    assert_ne!(
        crate::reading::block_id("a.md", 1, 0),
        crate::reading::block_id("a.md", 2, 0)
    );
}

/// Plan round-5 WP-1 (item 4): a Reading tab blurred and re-selected keeps its place — the
/// re-created scrollable's first `on_scroll` (offset 0) re-applies it instead of overwriting.
#[test]
fn tab_resume_keeps_the_reading_place() {
    let (a, b) = ("r5/a.md", "r5/b.md");
    let body = long_note();
    let (_t, d) = setup(&[(a, &body), (b, b"other\n")]);
    let mut app = app_on(&d, a);
    let view = app.view_of(a);
    let blocks = app
        .reading
        .doc(&app.docs, a)
        .expect("doc")
        .model
        .blocks
        .len();
    let block_ys: Vec<Option<f32>> = (0..blocks).map(|i| Some(i as f32 * 20.0)).collect();
    let probe = |offset_y: f32| Probe {
        offset_y,
        content_y: 0.0,
        block_ys: block_ys.clone(),
        ..Probe::default()
    };
    let _ = app.send(Message::Reading(ReadingMsg::Probed {
        path: a.to_owned(),
        view,
        probe: Some(probe(40.0 * 20.0)),
        scroll_to: None,
    }));
    assert_eq!(app.reading.top_block(&app.docs, a, view), Some(40));
    let _ = app.send(Message::Tabs(TabsMsg::Open(b.to_owned())));
    assert!(!app.reading.is_restoring(&app.docs, view));
    let pane = app.focus;
    let task = app.send(Message::Tabs(TabsMsg::Select(pane, 0)));
    assert_eq!(app.view_of(a), view);
    assert!(task.units() > 0, "activation issues the restore probe");
    assert!(app.reading.is_restoring(&app.docs, view));
    // The re-created scrollable reports offset 0 first: the place is re-applied, not lost.
    let _ = app.send(Message::Reading(ReadingMsg::Scrolled {
        path: a.to_owned(),
        view,
        y: 0.0,
    }));
    assert!(!app.reading.is_restoring(&app.docs, view));
    assert_eq!(app.reading.top_block(&app.docs, a, view), Some(40));
    // Afterwards scroll reports update the place normally.
    let _ = app.send(Message::Reading(ReadingMsg::Probed {
        path: a.to_owned(),
        view,
        probe: Some(probe(10.0 * 20.0)),
        scroll_to: None,
    }));
    assert_eq!(app.reading.top_block(&app.docs, a, view), Some(10));
}

/// Plan round-5 WP-1 (item 2): Up/Down with no palette open scroll the focused Reading pane
/// one line; nothing in Source-only, and the open palette keeps its own arrows.
#[test]
fn arrows_step_the_reading_pane_one_line() {
    use crate::message::PaletteMsg;
    use iced::keyboard::{Event, Key, Location, Modifiers, key::Named};
    let press = Event::KeyPressed {
        key: Key::Named(Named::ArrowDown),
        modified_key: Key::Named(Named::ArrowDown),
        physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::ArrowDown),
        location: Location::Standard,
        modifiers: Modifiers::empty(),
        text: None,
        repeat: false,
    };
    assert!(matches!(
        crate::palette::key_event(press),
        Some(Message::Palette(PaletteMsg::Down))
    ));
    assert!((crate::page::line_step(16.0, true) - 20.8).abs() < 1e-3);
    assert!(crate::page::line_step(16.0, false) < 0.0);
    let p = "r5/arrows.md";
    let body = long_note();
    let (_t, d) = setup(&[(p, &body)]);
    let mut app = app_on(&d, p);
    assert!(app.send(Message::Palette(PaletteMsg::Down)).units() > 0);
    reading(&mut app, p, ViewMode::Source);
    assert_eq!(app.send(Message::Palette(PaletteMsg::Down)).units(), 0);
}

/// plan editor-round-4 WP-2: plain PageDown (uncaptured) routes to the focused pane's
/// scrollable — Reading's in Reading/Split, none in Source; Ctrl+PageDown stays a chord.
#[test]
fn page_down_routes_to_the_reading_scrollable() {
    use iced::keyboard::{Event, Key, Location, Modifiers, key::Named};
    let press = |m: Modifiers| Event::KeyPressed {
        key: Key::Named(Named::PageDown),
        modified_key: Key::Named(Named::PageDown),
        physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::PageDown),
        location: Location::Standard,
        modifiers: m,
        text: None,
        repeat: false,
    };
    assert!(matches!(
        crate::palette::key_event(press(Modifiers::empty())),
        Some(Message::Panes(crate::message::PanesMsg::Page(true)))
    ));
    assert!(matches!(
        crate::palette::key_event(press(Modifiers::CTRL)),
        Some(Message::Palette(crate::message::PaletteMsg::Key(_)))
    ));
    let p = "r4/page.md";
    let body = long_note();
    let (_t, d) = setup(&[(p, &body)]);
    let mut app = app_on(&d, p);
    reading(&mut app, p, ViewMode::Reading);
    let (id, note) = app.page_target().expect("reading target");
    assert_eq!(id, crate::find::reading_scroll_id(p, app.view_of(p)));
    assert_eq!(note.as_deref(), Some(p));
    reading(&mut app, p, ViewMode::Split);
    assert!(app.page_target().is_some());
    reading(&mut app, p, ViewMode::Source);
    assert!(app.page_target().is_none());
}

/// plan editor-round-4 WP-2: PageDown pages by a viewport minus one line.
#[test]
fn page_delta_keeps_one_line_of_overlap() {
    let line = crate::page::PAGE_OVERLAP;
    assert!((crate::page::page_delta(500.0, true) - (500.0 - line)).abs() < f32::EPSILON);
    assert!((crate::page::page_delta(500.0, false) + (500.0 - line)).abs() < f32::EPSILON);
    assert!(crate::page::page_delta(10.0, true) > 0.0);
}
