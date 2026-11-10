//! The app state model: tab kind, per-document state and debounced persistence.

use super::*;
use crate::message::PanesMsg;
use crate::ui_state::TabKind;

fn kinds(app: &App) -> Vec<TabKind> {
    app.panes
        .iter()
        .flat_map(|(_, ps)| ps.tabs.iter().filter_map(|t| t.persisted(&app.docs)))
        .collect()
}

/// B4 defect: activating the Log tab must not re-apply the Settings body scroll. The
/// activation's `update` returns no task at all (the note left is clean, the persist
/// debounce is test-driven).
#[test]
fn activating_the_log_tab_runs_no_settings_scroll_restore() {
    let (_tmp, d) = fake_arc();
    d.create_note("r15/a.md", "# A\n").expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Log(crate::log::LogMsg::Open));
    let _ = app.send(Message::Tabs(TabsMsg::Open("r15/a.md".into())));
    let pane = app.focus;
    let log = app
        .panes
        .get(pane)
        .and_then(|ps| {
            ps.tabs
                .iter()
                .position(|t| t.kind == crate::app::TabKind::Log)
        })
        .expect("log tab");
    assert!(app.active_note().is_some(), "the note is active");
    let task = app.update(Message::Tabs(TabsMsg::Select(pane, log)));
    assert!(
        app.active_tab()
            .is_some_and(|t| t.kind == crate::app::TabKind::Log)
    );
    assert_eq!(
        task.units(),
        0,
        "no Settings restore_scroll on Log activation"
    );
    // Settings activation still restores its body offset.
    let _ = app.send(Message::Settings(crate::settings::SettingsMsg::Open));
    let _ = app.send(Message::Tabs(TabsMsg::Select(pane, log)));
    let settings = app
        .panes
        .get(pane)
        .and_then(|ps| {
            ps.tabs
                .iter()
                .position(|t| t.kind == crate::app::TabKind::Settings)
        })
        .expect("settings tab");
    let task = app.update(Message::Tabs(TabsMsg::Select(pane, settings)));
    assert!(task.units() > 0, "Settings re-applies its place");
}

/// An `editor.json` written by an earlier build (bare-string tabs, `/:settings` / `/:log`
/// sentinels, flat fields) restores its Settings, Log and note tabs.
#[test]
fn old_editor_json_restores_settings_log_and_note_tabs() {
    let (tmp, d) = fake_arc();
    d.create_note("r15/old.md", "# Old\n").expect("create");
    let state = tmp.path().join("state/editor.json");
    std::fs::create_dir_all(state.parent().expect("dir")).expect("mkdir");
    std::fs::write(
        &state,
        br#"{"tabs":["r15/old.md","/:settings","/:log"],"active_tab":0,"modes":{"r15/old.md":"source"},"surround":"dark","recent":["r15/old.md"],"panes":{"root":{"kind":"leaf","tabs":["r15/old.md","/:settings","/:log"],"active":0},"focused":0},"expanded":[]}"#,
    )
    .expect("old");
    let app = app_over(&d, Some(state.clone()));
    assert_eq!(
        kinds(&app),
        [
            TabKind::Note("r15/old.md".into()),
            TabKind::Settings,
            TabKind::Log
        ]
    );
    assert_eq!(
        app.reading.mode(&app.docs, "r15/old.md"),
        crate::reading::ViewMode::Source
    );
    assert!(app.status.error_text().is_none());
    assert_eq!(app.ui_writes, 0, "restoring writes nothing");
    // A flat-only (WP-1) file restores too.
    std::fs::write(&state, br#"{"tabs":["/:log","r15/old.md"],"active_tab":1}"#).expect("flat");
    let app = app_over(&d, Some(state));
    assert_eq!(
        kinds(&app),
        [TabKind::Log, TabKind::Note("r15/old.md".into())]
    );
}

/// SM12 (a, c): a burst of focus changes schedules one debounced write; the debounce
/// writes once, and an unchanged snapshot never writes.
#[test]
fn focus_burst_writes_once_after_the_debounce() {
    let (tmp, d) = fake_arc();
    let state = tmp.path().join("state/editor.json");
    let mut app = app_over(&d, Some(state.clone()));
    let first = app.focus;
    let _ = app.send(Message::Panes(PanesMsg::SplitRight));
    let second = app.focus;
    assert_ne!(first, second);
    for i in 0..20 {
        let p = if i % 2 == 0 { first } else { second };
        let _ = app.send(Message::Panes(PanesMsg::Focus(p)));
    }
    assert_eq!(
        app.ui_writes, 0,
        "nothing written before the debounce fires"
    );
    assert!(!state.exists());
    let _ = app.send(Message::PersistDue);
    assert_eq!(app.ui_writes, 1, "one write for the burst");
    assert_eq!(crate::ui_state::load(&state), app.ui, "the latest snapshot");
    // (c) unchanged: no write.
    let _ = app.send(Message::Panes(PanesMsg::Focus(app.focus)));
    let _ = app.send(Message::PersistDue);
    assert_eq!(app.ui_writes, 1, "unchanged snapshot → no write");
}
