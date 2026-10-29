//! Overlay, side collapse, borders, history, view mode and the dirty slot.

use super::*;
use crate::message::{ExplorerMsg, PaletteMsg, PanesMsg, ReadingMsg, SwitcherMsg};
use crate::reading::ViewMode;

type Fixture = (tempfile::TempDir, Arc<FakeDaemon>, App);

fn app_with(notes: &[&str], state: Option<std::path::PathBuf>) -> Fixture {
    let (tmp, d) = fake();
    let d = Arc::new(d);
    for n in notes {
        d.create_note(n, "x").expect("create");
    }
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), state);
    app.queue_rpcs();
    (tmp, d, app)
}

fn open(app: &mut App, p: &str) {
    let _ = app.send(Message::Explorer(ExplorerMsg::Open(p.into())));
}

fn active(app: &App) -> Option<String> {
    app.active_tab().and_then(|t| t.path().map(str::to_owned))
}

fn history(app: &App) -> Vec<String> {
    app.panes
        .get(app.focus)
        .map(|ps| ps.history.entries().to_vec())
        .unwrap_or_default()
}

#[test]
fn back_forward_walks_the_pane_history_without_pushing() {
    let (_tmp, _d, mut app) = app_with(&["h3/a.md", "h3/b.md", "h3/c.md"], None);
    for p in ["h3/a.md", "h3/b.md", "h3/c.md"] {
        open(&mut app, p);
    }
    assert_eq!(history(&app), ["h3/a.md", "h3/b.md", "h3/c.md"]);
    let pane = app.focus;
    let _ = app.send(Message::Panes(PanesMsg::Back(pane)));
    assert_eq!(active(&app).as_deref(), Some("h3/b.md"));
    let _ = app.send(Message::Panes(PanesMsg::BackFocused));
    assert_eq!(active(&app).as_deref(), Some("h3/a.md"));
    let _ = app.send(Message::Panes(PanesMsg::BackFocused));
    assert_eq!(active(&app).as_deref(), Some("h3/a.md"), "no-op at start");
    let _ = app.send(Message::Panes(PanesMsg::ForwardFocused));
    assert_eq!(active(&app).as_deref(), Some("h3/b.md"));
    assert_eq!(history(&app).len(), 3, "back/forward never push");
    // Re-opening the current entry does not duplicate it; a new open drops forward.
    open(&mut app, "h3/b.md");
    open(&mut app, "h3/a.md");
    assert_eq!(history(&app), ["h3/a.md", "h3/b.md", "h3/a.md"]);
    let _ = app.send(Message::Panes(PanesMsg::Forward(pane)));
    assert_eq!(active(&app).as_deref(), Some("h3/a.md"));
}

#[test]
fn rename_retargets_history_entries() {
    let (_tmp, _d, mut app) = app_with(&["h3r/one.md", "h3r/other.md"], None);
    open(&mut app, "h3r/one.md");
    open(&mut app, "h3r/other.md");
    let _ = app.send(Message::Reading(ReadingMsg::TitleInput {
        path: "h3r/other.md".into(),
        value: "two".into(),
    }));
    let _ = app.send(Message::Reading(ReadingMsg::TitleSubmit {
        path: "h3r/other.md".into(),
    }));
    assert_eq!(history(&app), ["h3r/one.md", "h3r/two.md"]);
}

#[test]
fn ctrl_e_toggles_source_and_reading_only() {
    assert_eq!(ViewMode::Source.toggle(), ViewMode::Reading);
    assert_eq!(ViewMode::Reading.toggle(), ViewMode::Source);
    assert_eq!(ViewMode::Split.toggle(), ViewMode::Source);
    let (_tmp, _d, mut app) = app_with(&["m3/a.md"], None);
    open(&mut app, "m3/a.md");
    let _ = app.send(Message::Reading(ReadingMsg::SetMode {
        path: "m3/a.md".into(),
        mode: ViewMode::Split,
    }));
    let _ = app.send(Message::Reading(ReadingMsg::CycleMode));
    assert_eq!(app.reading.mode(&app.docs, "m3/a.md"), ViewMode::Source);
    let _ = app.send(Message::Reading(ReadingMsg::CycleMode));
    assert_eq!(app.reading.mode(&app.docs, "m3/a.md"), ViewMode::Reading);
}

#[test]
fn mode_menu_opens_and_closes_on_select() {
    let (_tmp, _d, mut app) = app_with(&["m3m/a.md"], None);
    open(&mut app, "m3m/a.md");
    let pane = app.focus;
    let _ = app.send(Message::Panes(PanesMsg::ModeMenu(Some(pane))));
    assert_eq!(app.mode_menu, Some(pane));
    drop(app.view());
    let _ = app.send(Message::Reading(ReadingMsg::SetMode {
        path: "m3m/a.md".into(),
        mode: ViewMode::Split,
    }));
    assert_eq!(app.mode_menu, None);
    assert_eq!(app.reading.mode(&app.docs, "m3m/a.md"), ViewMode::Split);
}

#[test]
fn overlay_close_message_closes_switcher_and_palette() {
    let (_tmp, _d, mut app) = app_with(&[], None);
    assert!(app.overlay_close().is_none());
    let _ = app.send(Message::Switcher(SwitcherMsg::Open));
    drop(app.view());
    let close = app.overlay_close().expect("switcher open");
    assert!(matches!(close, Message::Switcher(SwitcherMsg::Close)));
    let _ = app.send(close);
    assert!(!app.switcher.open);
    let _ = app.send(Message::Palette(PaletteMsg::Open));
    drop(app.view());
    let close = app.overlay_close().expect("palette open");
    assert!(matches!(close, Message::Palette(PaletteMsg::Close)));
    let _ = app.send(close);
    assert!(!app.palette.open);
    assert!(app.overlay_close().is_none());
}

/// plan editor-ui-round-3 WP-5: Esc clears a non-empty input first, then closes.
#[test]
fn escape_clears_then_closes_switcher_and_palette() {
    let (_tmp, _d, mut app) = app_with(&[], None);
    let _ = app.send(Message::Switcher(SwitcherMsg::Open));
    let empty = app.switcher.results.clone();
    let _ = app.send(Message::Switcher(SwitcherMsg::Query("zzqq".into())));
    let refocus = app.send(Message::Switcher(SwitcherMsg::Escape));
    assert!(
        refocus.units() > 0,
        "editor-round-4 WP-1: clear re-focuses the input"
    );
    assert!(app.switcher.open, "first Esc only clears");
    assert!(app.switcher.query.is_empty());
    assert_eq!(app.switcher.results, empty, "back to the empty-query list");
    let _ = app.send(Message::Switcher(SwitcherMsg::Escape));
    assert!(!app.switcher.open);

    let _ = app.send(Message::Palette(PaletteMsg::Open));
    let empty = app.palette.results.clone();
    let _ = app.send(Message::Palette(PaletteMsg::Query("zzqq".into())));
    let refocus = app.send(Message::Palette(PaletteMsg::Escape));
    assert!(
        refocus.units() > 0,
        "editor-round-4 WP-1: clear re-focuses the input"
    );
    assert!(app.palette.open, "first Esc only clears");
    assert!(app.palette.query.is_empty());
    assert_eq!(app.palette.results, empty);
    let _ = app.send(Message::Palette(PaletteMsg::Escape));
    assert!(!app.palette.open);
}

#[test]
fn side_collapse_and_borders_persist() {
    let tmp = tempfile::tempdir().expect("tmp");
    let state = tmp.path().join("state/editor.json");
    let (_tmp, d, mut app) = app_with(&[], Some(state.clone()));
    assert!(app.ui.side.0.is_some() && !app.ui.pane_borders);
    let _ = app.send(Message::Panes(PanesMsg::ToggleSide));
    let _ = app.send(Message::Panes(PanesMsg::ToggleBorders));
    drop(app.view());
    let _ = app.send(Message::PersistDue);
    let dc: Arc<dyn DaemonClient> = d as Arc<dyn DaemonClient>;
    let again = App::new(Some(dc), Some(state));
    assert!(again.ui.side.0.is_none() && again.ui.pane_borders);
}

/// Plan round-17 WP-6: the activity strip — a click on the open section's button closes the
/// sidebar, another section's swaps, the strip's active section is the open one, and
/// `ToggleSide` reopens the last open section.
#[test]
fn activity_strip_toggles_swaps_and_reopens_last() {
    use crate::ui_state::SideSection as S;
    let (_tmp, _d, mut app) = app_with(&[], None);
    assert_eq!(app.ui.side.0, Some(S::Files));
    let click = |app: &mut App, s: S| {
        let m = super::super::view::strip_message(app.ui.side.0, s);
        let _ = app.send(m);
    };
    click(&mut app, S::Files);
    assert_eq!(app.ui.side.0, None, "open section's button closes");
    click(&mut app, S::Files);
    click(&mut app, S::Outline);
    assert_eq!(app.ui.side.0, Some(S::Outline), "swap");
    assert!(app.panels.visible && app.panels.kind == crate::panels::PanelKind::Outline);
    let _ = app.send(Message::Panes(PanesMsg::ToggleSide));
    assert!(app.ui.side.0.is_none() && !app.panels.visible);
    let _ = app.send(Message::Panes(PanesMsg::ToggleSide));
    assert_eq!(app.ui.side.0, Some(S::Outline), "reopens the last section");
    click(&mut app, S::Bookmarks);
    assert!(!app.panels.visible, "a non-panel section hides the panels");
    drop(app.view());
}

/// Plan round-17 WP-6: `Show(Search)` opens the Search section; the palette's `toggle_panels`
/// still resolves (to the sidebar toggle) and every section has a `panel.<id>` show command.
#[test]
fn show_search_opens_its_section_and_palette_keeps_toggle() {
    use crate::message::PanelsMsg;
    use crate::ui_state::SideSection as S;
    let (_tmp, _d, mut app) = app_with(&[], None);
    let _ = app.send(Message::Panels(PanelsMsg::Show(
        crate::panels::PanelKind::Search,
    )));
    assert_eq!(app.ui.side.0, Some(S::Search));
    assert!(app.panels.visible);
    let _ = app.send(Message::Panels(PanelsMsg::Toggle));
    assert_eq!(
        app.ui.side.0, None,
        "PanelsMsg::Toggle is the sidebar toggle"
    );
    let table = crate::palette::commands::table();
    for id in [
        "toggle_panels",
        "panel.files",
        "panel.bookmarks",
        "panel.tags",
    ] {
        assert!(table.iter().any(|c| c.id == id), "{id}");
    }
    let files = crate::palette::commands::ClientCmd::Panel(S::Files);
    let _ = app.send(files.message(app.focus));
    assert_eq!(app.ui.side.0, Some(S::Files));
}

#[test]
fn palette_lists_the_pane_border_toggle() {
    let cmd = crate::palette::ClientCmd::TogglePaneBorders;
    assert!(crate::palette::ClientCmd::all().contains(&cmd));
    assert_eq!(cmd.title().0, "Toggle pane borders (debug)");
    assert!(matches!(
        cmd.message(pane_grid_pane()),
        Message::Panes(PanesMsg::ToggleBorders)
    ));
}

fn pane_grid_pane() -> iced::widget::pane_grid::Pane {
    let (_tmp, _d, app) = app_with(&[], None);
    app.focus
}

#[test]
fn dirty_marker_lives_on_the_tab_and_source_slots_are_stable() {
    assert_eq!(
        crate::source::SLOTS.iter().position(|s| *s == "editor"),
        Some(0)
    );
    let (_tmp, _d, mut app) = app_with(&["d3/t.md"], None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("d3/t.md".into())));
    let tab = app.active_tab().expect("tab");
    // WP-6: the title text never carries the marker; the marker is a separate glyph slot.
    let ps = app.panes.get(app.focus).expect("pane");
    assert_eq!(app.tab_header_title(ps, tab), "t");
    assert_eq!(
        crate::header::marker(
            app.active_doc().expect("doc").is_dirty(),
            app.active_doc().expect("doc").is_saving()
        ),
        ""
    );
    let _ = app.send(Message::Reading(ReadingMsg::SetMode {
        path: "d3/t.md".into(),
        mode: ViewMode::Source,
    }));
    drop(app.view());
    let _ = app.send(Message::Source(crate::message::SourceMsg::Action(
        iced::widget::text_editor::Action::Edit(iced::widget::text_editor::Edit::Insert('y')),
    )));
    assert!(app.active_doc().expect("doc").is_dirty());
    let tab = app.active_tab().expect("tab");
    let ps = app.panes.get(app.focus).expect("pane");
    assert_eq!(app.tab_header_title(ps, tab), "t", "no marker text");
    assert_eq!(crate::header::marker(true, false), "•");
    drop(app.view());
}

// ---- editor-feedback-round-2 WP-6: pane header ----

use super::workspace::title;

#[test]
fn breadcrumb_filename_edit_runs_in_source_mode_and_renames() {
    let (_tmp, d) = fake_arc();
    d.create_note("wp6ren/one.md", "body").expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp6ren/one.md".into())));
    let _ = app.send(Message::Reading(ReadingMsg::SetMode {
        path: "wp6ren/one.md".into(),
        mode: ViewMode::Source,
    }));
    assert!(!app.reading.ticks(&app.docs) && !app.reading.title_pending(&app.docs));
    title(&mut app, "wp6ren/one.md", "two");
    // Source-only, yet the blur poll and Escape listener run while the edit is pending.
    assert!(
        app.reading.ticks(&app.docs),
        "blur-poll tick in Source mode"
    );
    assert!(
        app.reading.title_pending(&app.docs),
        "Escape in Source mode"
    );
    drop(app.view());
    let _ = app.send(Message::Reading(ReadingMsg::TitleRevert));
    assert!(!app.reading.title_pending(&app.docs));
    title(&mut app, "wp6ren/one.md", "two");
    let _ = app.send(Message::Reading(ReadingMsg::TitleSubmit {
        path: "wp6ren/one.md".into(),
    }));
    assert!(
        d.read_file("wp6ren/two.md").is_ok(),
        "renamed via the daemon"
    );
    assert_eq!(app.open_paths(), ["wp6ren/two.md"]);
    assert!(!app.reading.ticks(&app.docs));
}

#[test]
fn tab_titles_follow_the_pref() {
    let (_tmp, d) = fake_arc();
    d.create_note("wp6t/a/n.md", "---\ntitle: Front Title\n---\nbody")
        .expect("create");
    d.create_note("wp6t/b/n.md", "plain").expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp6t/a/n.md".into())));
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp6t/b/n.md".into())));
    let titles = |app: &App| {
        let ps = app.panes.get(app.focus).expect("pane");
        ps.tabs
            .iter()
            .map(|t| app.tab_header_title(ps, t))
            .collect::<Vec<_>>()
    };
    assert_eq!(titles(&app), ["Front Title", "n"]);
    app.prefs.tab_title = crate::header::TabTitle::Unambiguous;
    assert_eq!(titles(&app), ["a/n", "b/n"]);
    app.prefs.tab_title = crate::header::TabTitle::Path;
    assert_eq!(titles(&app), ["wp6t/a/n.md", "wp6t/b/n.md"]);
    drop(app.view());
}

#[test]
fn ctrl_alt_arrows_step_the_focused_pane() {
    use iced::keyboard::{Event, Key, Location, Modifiers, key::Named, key::Physical};
    let ev = |n: Named, m: Modifiers| Event::KeyPressed {
        key: Key::Named(n),
        modified_key: Key::Named(n),
        physical_key: Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified),
        location: Location::Standard,
        modifiers: m,
        text: None,
        repeat: false,
    };
    let ca = Modifiers::CTRL | Modifiers::ALT;
    assert!(matches!(
        crate::palette::key_event(ev(Named::ArrowLeft, ca)),
        Some(Message::Panes(PanesMsg::BackFocused))
    ));
    assert!(matches!(
        crate::palette::key_event(ev(Named::ArrowRight, ca)),
        Some(Message::Panes(PanesMsg::ForwardFocused))
    ));
    assert!(!matches!(
        crate::palette::key_event(ev(Named::ArrowLeft, Modifiers::CTRL)),
        Some(Message::Panes(_))
    ));
}

/// Plan round-17 WP-5 (Decision 8): the four left failsafes, in order, with their messages.
#[test]
fn status_failsafes_are_the_four_buttons_in_order() {
    use crate::icons::Icon;
    let f = crate::app::view::status_failsafes();
    let labels: Vec<_> = f.iter().map(|(_, l, _)| *l).collect();
    assert_eq!(labels, ["Settings", "Logs", "Commands", "Switcher"]);
    let icons: Vec<_> = f.iter().map(|(i, _, _)| *i).collect();
    assert_eq!(
        icons,
        [Icon::Settings, Icon::Activity, Icon::Command, Icon::File]
    );
    assert!(matches!(
        f[0].2,
        Message::Settings(crate::settings::SettingsMsg::Open)
    ));
    assert!(matches!(f[1].2, Message::Log(crate::log::LogMsg::Open)));
    assert!(matches!(f[2].2, Message::Palette(PaletteMsg::Open)));
    assert!(matches!(f[3].2, Message::Switcher(SwitcherMsg::Open)));
}

/// Plan round-17 WP-5: the glyphs were read from the vendored font's glyph names.
#[test]
fn command_and_file_icons_use_the_font_codepoints() {
    use crate::icons::Icon;
    assert_eq!(Icon::Command.glyph(), '\u{e09e}');
    assert_eq!(Icon::File.glyph(), '\u{e0c4}');
}

/// Plan round-17 WP-5 (Decisions 7, 9): the chrome actions stay on the command palette.
#[test]
fn removed_chrome_actions_stay_reachable_from_the_palette() {
    let ids: Vec<&str> = crate::palette::commands::table()
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    for id in [
        "close_pane",
        "surround",
        "split_right",
        "split_down",
        "toggle_panels",
        "quick_switcher",
        "command_palette",
        "open_settings",
    ] {
        assert!(ids.contains(&id), "{id} missing from the palette");
    }
}

#[test]
fn legacy_menu_bar_key_still_loads() {
    use crate::settings::editor_prefs::parse;
    let med = crate::theme::Surround::Med;
    assert!(parse("menu_bar = \"hidden\"\n", med).is_ok());
}

/// Plan round-12-feedback-todos Decision 9: opening the switcher closes the palette and vice versa.
#[test]
fn centre_modals_are_exclusive() {
    let (_tmp, _d, mut app) = app_with(&[], None);
    let _ = app.send(Message::Palette(PaletteMsg::Open));
    let _ = app.send(Message::Switcher(SwitcherMsg::Open));
    assert!(app.switcher.open && !app.palette.open);
    drop(app.view());
    let _ = app.send(Message::Palette(PaletteMsg::Open));
    assert!(app.palette.open && !app.switcher.open);
    assert!(matches!(
        app.overlay_close(),
        Some(Message::Palette(PaletteMsg::Close))
    ));
    drop(app.view());
}

/// Plan round-12-feedback-todos WP-3: the search command opens the modal under `modal`, the side panel under `panel`.
#[test]
fn search_command_opens_modal_or_panel_per_location() {
    use crate::message::PanelsMsg;
    use crate::settings::editor_prefs::SearchLocation;
    let (_tmp, _d, mut app) = app_with(&[], None);
    app.panels.visible = false;
    let _ = app.send(Message::Panels(PanelsMsg::Search));
    assert!(!app.panels.search_modal && app.panels.visible);
    assert_eq!(app.panels.kind, crate::panels::PanelKind::Search);

    app.prefs.search_location = SearchLocation::Modal;
    app.panels.visible = false;
    let _ = app.send(Message::Panels(PanelsMsg::Search));
    assert!(app.panels.search_modal && !app.panels.visible);
    assert!(matches!(
        app.overlay_close(),
        Some(Message::Panels(PanelsMsg::CloseSearchModal))
    ));
    drop(app.view());
    let _ = app.send(Message::Panels(PanelsMsg::Open("a.md".into())));
    assert!(
        !app.panels.search_modal,
        "opening a result closes the modal"
    );
}

/// Plan round-12-feedback-todos WP-3: the search modal is one of the exclusive centre modals.
#[test]
fn search_modal_is_exclusive_with_palette_and_switcher() {
    use crate::message::PanelsMsg;
    use crate::settings::editor_prefs::SearchLocation;
    let (_tmp, _d, mut app) = app_with(&[], None);
    app.prefs.search_location = SearchLocation::Modal;
    let _ = app.send(Message::Palette(PaletteMsg::Open));
    let _ = app.send(Message::Panels(PanelsMsg::Search));
    assert!(app.panels.search_modal && !app.palette.open && !app.switcher.open);
    let _ = app.send(Message::Switcher(SwitcherMsg::Open));
    assert!(app.switcher.open && !app.panels.search_modal);
    let _ = app.send(Message::Panels(PanelsMsg::Search));
    assert!(app.panels.search_modal && !app.switcher.open);
    let _ = app.send(Message::Palette(PaletteMsg::Open));
    assert!(app.palette.open && !app.panels.search_modal);
    drop(app.view());
}
