//! plan editor-ui-round-3 WP-5/WP-9 hotkey tests: Ctrl+PgUp / Ctrl+PgDn tab cycling and bookmark commands (split from `hotkeys_tests.rs`).

use super::*;

// ---- plan editor-ui-round-3 WP-5 (audit E2): Ctrl+PgUp / Ctrl+PgDn ----

#[test]
fn page_chord_classification() {
    for (k, reserved) in [
        ("Ctrl+PageUp", false),
        ("Ctrl+PageDown", false),
        ("PageUp", true),
        ("PageDown", true),
        ("Shift+PageUp", true),
        ("Shift+PageDown", true),
        ("Ctrl+Shift+PageUp", true),
        ("Alt+PageUp", false),
    ] {
        assert_eq!(chord::text_editing(&c(k)), reserved, "{k}");
    }
}

#[test]
fn go_to_top_bottom_are_bound_defaults_and_released_by_empty_override() {
    let km = resolve(&BTreeMap::new(), &[]);
    assert_eq!(
        km.get(&c("Ctrl+PageUp")),
        Some(&Bound::Client(ClientCmd::GoToTop))
    );
    assert_eq!(
        km.get(&c("Ctrl+PageDown")),
        Some(&Bound::Client(ClientCmd::GoToBottom))
    );
    assert!(km.routed(&c("Ctrl+PageUp")));
    // `[]` releases the default, so an action may take it without a conflict.
    let acts = [info("a", &["Ctrl+PageUp"])];
    assert_eq!(
        resolve(&BTreeMap::new(), &acts).get(&c("Ctrl+PageUp")),
        None
    );
    let km = resolve(&ov(&[("go_to_top", &[])]), &acts);
    assert_eq!(km.get(&c("Ctrl+PageUp")), Some(&Bound::Action("a".into())));
    assert!(km.conflicted.is_empty(), "{:?}", km.notices);
}

#[test]
fn capture_accepts_ctrl_page_up() {
    let (_tmp, d) = fake();
    let mut app = hotkeys_app(&d);
    // Held by Go to top: another command's capture is refused as a conflict, not as reserved.
    hk(&mut app, HotkeyMsg::Record(Target::Command("quit".into())));
    key(&mut app, "Ctrl+PageUp");
    let note = app.settings.hotkeys.note.clone().unwrap_or_default();
    assert!(note.contains("Go to top"), "{note}");
    assert_eq!(keys_of(&app, "quit"), None);
    // Captured onto Go to bottom after Go to top released it, it binds.
    hk(
        &mut app,
        HotkeyMsg::Record(Target::Command("go_to_top".into())),
    );
    key(&mut app, "Alt+T");
    hk(
        &mut app,
        HotkeyMsg::Record(Target::Command("go_to_bottom".into())),
    );
    key(&mut app, "Ctrl+PageUp");
    let got = keys_of(&app, "go_to_bottom").expect("captured");
    assert_eq!(
        got.iter().map(|k| c(k)).collect::<Vec<_>>(),
        vec![c("Ctrl+PageUp")]
    );
    assert_eq!(
        app.palette.keymap.get(&c("Ctrl+PageUp")),
        Some(&Bound::Client(ClientCmd::GoToBottom))
    );
}

/// Plan editor-ui-round-3 WP-7: a capture note and entering capture leave the scrollable's
/// content (row count, per-row notices, i.e. row heights) and its stable id unchanged — the note
/// lives in the fixed `notice_bar` outside the scrollable.
#[test]
fn note_and_capture_keep_scroll_content() {
    let shape = |st: &hotkeys::HotkeyState| {
        hotkeys::rows(st, None)
            .iter()
            .map(|r| r.notices.len())
            .collect::<Vec<_>>()
    };
    let mut st = hotkeys::HotkeyState::default();
    let before = shape(&st);
    let id = body_scroll_id(Section::Hotkeys);
    st.note = Some("Ctrl+W is already assigned to Close tab".into());
    assert_eq!(shape(&st), before);
    st.record(Target::Command("quit".into()));
    assert_eq!(shape(&st), before);
    st.note = Some("reserved".into());
    assert_eq!(shape(&st), before);
    assert_eq!(body_scroll_id(Section::Hotkeys), id);
    let t = crate::theme::Surround::Dark.tokens().expect("tokens");
    let _bar: iced::Element<'_, crate::message::Message> = hotkeys::notice_bar(&st, t);
}

// ---- plan editor-ui-round-3 WP-9: bookmark commands, chords and missing targets ----

const BM: &str = "_custom/crucible-vault/bookmarks.toml";

const TWO: &str = "[[bookmark]]\nname = \"A\"\npath = \"wp9/a.md\"\n\n[[bookmark]]\nname = \"B\"\npath = \"wp9/b.md\"\n";

fn bm_app(d: &Arc<FakeDaemon>, toml: &str) -> App {
    d.create_note("wp9/a.md", "a").expect("a");
    d.create_note("wp9/b.md", "b").expect("b");
    put(d, BM, toml);
    hotkeys_app(d)
}

fn bm_on_disk(d: &FakeDaemon) -> Vec<crate::bookmarks::Bookmark> {
    crate::bookmarks::parse(&std::fs::read(d.vault_root().join(BM)).expect("read")).expect("parse")
}

fn bm_msg(app: &mut App, m: crate::bookmarks::BookmarkMsg) {
    let _ = app.send(Message::Panes(crate::message::PanesMsg::Bookmark(m)));
}

#[test]
fn bookmark_commands_join_palette_and_hotkeys() {
    let (_tmp, d) = fake();
    let app = bm_app(&d, TWO);
    assert_eq!(crate::palette::bookmark_id("wp9/a.md"), "bookmark:wp9/a.md");
    assert!(
        app.palette
            .entries
            .iter()
            .any(|e| e.title == "Open bookmark: A"
                && e.id == crate::palette::EntryId::Bookmark("wp9/a.md".into()))
    );
    let rows = hotkeys::rows(&app.settings.hotkeys, None);
    let r = rows
        .iter()
        .find(|r| r.target == Target::Bookmark("wp9/b.md".into()))
        .expect("row");
    assert_eq!(
        (r.title.as_str(), r.source),
        ("Open bookmark: B", KeySource::BookmarksToml)
    );
}

#[test]
fn assigning_a_bookmark_chord_writes_keys_and_survives_reorder() {
    let (_tmp, d) = fake();
    let mut app = bm_app(&d, TWO);
    hk(
        &mut app,
        HotkeyMsg::Record(Target::Bookmark("wp9/a.md".into())),
    );
    key(&mut app, "Alt+9");
    let on_disk = bm_on_disk(&d);
    assert_eq!(on_disk[0].keys, ["Alt+9"], "written at once");
    assert!(on_disk[1].keys.is_empty());
    assert_eq!(
        app.palette.keymap.get(&c("Alt+9")),
        Some(&Bound::Bookmark("wp9/a.md".into()))
    );
    // Removing a sibling: the chord stays with the bookmark.
    bm_msg(&mut app, crate::bookmarks::BookmarkMsg::Remove(1));
    let on_disk = bm_on_disk(&d);
    assert_eq!(on_disk[0].path, "wp9/a.md");
    assert_eq!(on_disk[0].keys, ["Alt+9"]);
    assert_eq!(
        app.palette.keymap.get(&c("Alt+9")),
        Some(&Bound::Bookmark("wp9/a.md".into()))
    );
    // The chord opens it.
    key(&mut app, "Alt+9");
    assert_eq!(app.active_tab().and_then(|t| t.path()), Some("wp9/a.md"));
    // Clear drops the keys.
    hk(
        &mut app,
        HotkeyMsg::Clear(Target::Bookmark("wp9/a.md".into())),
    );
    assert!(bm_on_disk(&d).iter().all(|b| b.keys.is_empty()));
    assert_eq!(app.palette.keymap.get(&c("Alt+9")), None);
}

#[test]
fn bookmark_chord_taken_elsewhere_is_refused() {
    let (_tmp, d) = fake();
    let mut app = bm_app(&d, TWO);
    let before = std::fs::read_to_string(d.vault_root().join(BM)).expect("r");
    hk(
        &mut app,
        HotkeyMsg::Record(Target::Bookmark("wp9/a.md".into())),
    );
    key(&mut app, "Ctrl+W");
    let note = app.settings.hotkeys.note.clone().unwrap_or_default();
    assert!(note.contains("Ctrl+W is already assigned to"), "{note}");
    assert_eq!(
        std::fs::read_to_string(d.vault_root().join(BM)).expect("r"),
        before
    );
    // And a command cannot take a bookmark's chord.
    hk(
        &mut app,
        HotkeyMsg::Record(Target::Bookmark("wp9/a.md".into())),
    );
    key(&mut app, "Alt+9");
    hk(&mut app, HotkeyMsg::Record(Target::Command("quit".into())));
    key(&mut app, "Alt+9");
    let note = app.settings.hotkeys.note.clone().unwrap_or_default();
    assert!(
        note.contains("already assigned to Open bookmark: A"),
        "{note}"
    );
    assert_eq!(keys_of(&app, "quit"), None);
}

#[test]
fn bookmark_chord_claimed_twice_at_load_registers_neither() {
    let (_tmp, d) = fake();
    let toml = "[[bookmark]]\nname = \"A\"\npath = \"wp9/a.md\"\nkeys = [\"Alt+9\"]\n\n[[bookmark]]\nname = \"B\"\npath = \"wp9/b.md\"\nkeys = [\"Alt+9\"]\n";
    let app = bm_app(&d, toml);
    let km = &app.palette.keymap;
    assert_eq!(km.get(&c("Alt+9")), None);
    assert!(km.conflicted.contains("bookmark:wp9/a.md"));
    assert!(km.conflicted.contains("bookmark:wp9/b.md"));
}

#[test]
fn missing_bookmarks_on_load_and_after_delete() {
    let (_tmp, d) = fake();
    let toml = format!("{TWO}\n[[bookmark]]\nname = \"G\"\npath = \"wp9/gone.md\"\n");
    let mut app = bm_app(&d, &toml);
    let set = |app: &App| app.bookmarks.missing.iter().cloned().collect::<Vec<_>>();
    assert_eq!(set(&app), ["wp9/gone.md"]);
    d.delete("wp9/b.md").expect("delete");
    let _ = app.send(Message::Status(crate::message::StatusMsg::Notification(
        cv_protocol::Notification::FileChanged {
            path: "wp9/b.md".into(),
            kind: cv_protocol::ChangeKind::Deleted,
            hash: None,
            origin: cv_protocol::Origin::External,
            from: None,
            replaced_by: None,
        },
    )));
    // The daemon's rebuild lands as a new generation (refreshed there too).
    let n = d.generation().expect("gen");
    let _ = app.send(Message::Status(crate::message::StatusMsg::Notification(
        cv_protocol::Notification::IndexGeneration { n },
    )));
    assert_eq!(set(&app), ["wp9/b.md", "wp9/gone.md"]);
    // Opening a missing bookmark (panel click or command): a notice, no tab.
    let tabs = |app: &App| app.panes.iter().map(|(_, p)| p.tabs.len()).sum::<usize>();
    let before = tabs(&app);
    bm_msg(
        &mut app,
        crate::bookmarks::BookmarkMsg::Open("wp9/gone.md".into()),
    );
    assert_eq!(
        app.status.error_text(),
        Some("bookmark target missing: wp9/gone.md")
    );
    assert_eq!(tabs(&app), before);
}

/// Plan round-5 WP-1 (item 4d): each section keeps its own body offset and re-applies it on
/// section activation (iced scrollable state is positional).
#[test]
fn settings_sections_keep_their_body_offsets() {
    use crate::message::Message;
    let mut app = crate::app::App::new(None, None);
    let _ = app.send(Message::Settings(SettingsMsg::Open));
    let _ = app.send(Message::Settings(SettingsMsg::BodyScrolled(
        Section::Hotkeys,
        120.0,
    )));
    assert_eq!(
        app.settings.body_offsets.get(&Section::Hotkeys),
        Some(&120.0)
    );
    let t = app.send(Message::Settings(SettingsMsg::Section(Section::Editor)));
    assert!(t.units() > 0, "the section's offset is re-applied");
    assert_eq!(
        app.settings.body_offsets.get(&Section::Hotkeys),
        Some(&120.0)
    );
}

/// Plan round-10 WP-5: "Find by chord" routes the next chord to the filter, never to an edit.
#[test]
fn chord_search_shows_the_owning_row_and_edits_nothing() {
    let (_tmp, d) = fake();
    let mut app = hotkeys_app(&d);
    hk(&mut app, HotkeyMsg::FindChord);
    assert!(app.settings.hotkeys.searching);
    key(&mut app, "Ctrl+W");
    let st = &app.settings.hotkeys;
    assert!(!st.searching && st.recording.is_none());
    assert_eq!(st.chord_filter, Some(c("Ctrl+W")));
    assert_eq!(
        st.note, None,
        "no owner-reject note: the search is not an assignment"
    );
    let shown = hotkeys::filtered(st, hotkeys::rows(st, None));
    assert!(!shown.is_empty());
    assert!(shown.iter().all(|r| r.chords.iter().any(|k| k == "Ctrl+W")));
    assert_eq!(hotkeys::unbound_line(st, &shown), None);
    assert!(
        !app.settings.file(EDITOR_PATH).is_some_and(|f| f.dirty()),
        "the search edits no document"
    );
    assert!(!app.exiting);
    // The clear button restores the list.
    hk(&mut app, HotkeyMsg::ClearFilter);
    let st = &app.settings.hotkeys;
    assert_eq!(
        hotkeys::filtered(st, hotkeys::rows(st, None)).len(),
        hotkeys::rows(st, None).len()
    );
}

#[test]
fn chord_search_of_an_unbound_chord_says_unbound() {
    let (_tmp, d) = fake();
    let mut app = hotkeys_app(&d);
    hk(&mut app, HotkeyMsg::FindChord);
    key(&mut app, "Alt+Shift+J");
    let st = &app.settings.hotkeys;
    let shown = hotkeys::filtered(st, hotkeys::rows(st, None));
    assert!(shown.is_empty());
    assert_eq!(
        hotkeys::unbound_line(st, &shown).as_deref(),
        Some("Shift+Alt+J: unbound")
    );
    assert!(!app.settings.file(EDITOR_PATH).is_some_and(|f| f.dirty()));
}

#[test]
fn escape_cancels_the_chord_search_and_per_row_record_still_works() {
    let (_tmp, d) = fake();
    let mut app = hotkeys_app(&d);
    hk(&mut app, HotkeyMsg::FindChord);
    let _ = app.send(Message::Palette(PaletteMsg::Escape));
    assert!(!app.settings.hotkeys.searching);
    assert_eq!(app.settings.hotkeys.chord_filter, None);
    // A chord after the cancel is an ordinary key again, not a search.
    key(&mut app, "Alt+Shift+J");
    assert_eq!(app.settings.hotkeys.chord_filter, None);
    // Per-row Record is unchanged.
    hk(&mut app, HotkeyMsg::Record(Target::Command("quit".into())));
    assert!(!app.settings.hotkeys.searching);
    key(&mut app, "Alt+K");
    assert_eq!(keys_of(&app, "quit"), Some(vec!["Alt+K".into()]));
}

#[test]
fn text_filter_narrows_rows_by_title_or_id_case_insensitively() {
    let (_tmp, d) = fake();
    let mut app = hotkeys_app(&d);
    let all = hotkeys::rows(&app.settings.hotkeys, None);
    hk(&mut app, HotkeyMsg::Filter("QUIT".into()));
    let st = &app.settings.hotkeys;
    let shown = hotkeys::filtered(st, all.clone());
    assert!(!shown.is_empty() && shown.len() < all.len());
    assert!(
        shown
            .iter()
            .any(|r| r.target == Target::Command("quit".into()))
    );
    assert!(shown.iter().all(|r| {
        r.title.to_lowercase().contains("quit") || r.target.id().to_lowercase().contains("quit")
    }));
}

/// Round-17 Decision 5: the table is sorted by title, case-insensitively.
#[test]
fn hotkey_rows_are_sorted_by_title() {
    let (_tmp, d) = fake();
    let app = bm_app(&d, TWO);
    let rows = hotkeys::rows(&app.settings.hotkeys, None);
    let titles: Vec<String> = rows.iter().map(|r| r.title.to_lowercase()).collect();
    let mut sorted = titles.clone();
    sorted.sort();
    assert_eq!(titles, sorted);
    assert!(rows.len() > 2);
}
