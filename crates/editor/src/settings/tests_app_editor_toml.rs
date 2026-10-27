use super::*;

// ---- app: editor.toml ----

#[test]
fn absent_editor_toml_defaults_and_save_creates_exact_name() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    assert_eq!(app.prefs, Prefs::default());
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert_eq!(*f.state(), LoadState::Absent);
    assert!(!f.dirty(), "the surround seed alone is not an edit");
    let _ = app.send(font(30.0));
    assert!(d.read_file(EDITOR_PATH).is_err(), "no write before Save");
    assert!(
        (app.prefs.font_size - 30.0).abs() < f32::EPSILON,
        "applied live"
    );
    let _ = app.send(s(app.settings.due_now(EDITOR_PATH)));
    let (bytes, h) = d.read_file(EDITOR_PATH).expect("created");
    let text = String::from_utf8(bytes).expect("utf8");
    assert!(text.contains("font_size = 30"), "{text}");
    assert!(
        text.contains("surround = \"med\""),
        "the seed rides along: {text}"
    );
    assert_eq!(
        config_dir_entries(&d),
        ["editor.toml"],
        "exact name, no copy"
    );
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert_eq!(f.base(), Some(&h));
    assert!(!f.dirty() && *f.state() == LoadState::Loaded);
}

#[test]
fn editor_toml_appeared_since_load_is_a_conflict() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let _ = app.send(font(30.0));
    let theirs = "font_size = 12\n";
    d.write_note(EDITOR_PATH, theirs, &ContentHash::from_hex(""))
        .expect("external create");
    let _ = app.send(s(app.settings.due_now(EDITOR_PATH)));
    assert_eq!(d.read_file(EDITOR_PATH).expect("r").0, theirs.as_bytes());
    assert_eq!(
        config_dir_entries(&d),
        ["editor.toml"],
        "never a suffixed copy"
    );
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert_eq!(f.notice(), Some(CHANGED_ON_DISK));
    assert!(!f.dirty(), "reloaded");
    assert!((app.prefs.font_size - 12.0).abs() < f32::EPSILON);
}

#[test]
fn editor_toml_cas_conflict_reloads_without_overwrite() {
    let (_tmp, d) = fake();
    let h0 = d
        .write_note(
            EDITOR_PATH,
            "font_size = 20 # mine\n",
            &ContentHash::from_hex(""),
        )
        .expect("seed");
    let mut app = app_over(&d, None);
    assert!((app.prefs.font_size - 20.0).abs() < f32::EPSILON);
    let _ = app.send(pref("delete_mode", ScalarValue::Str("permanent".into())));
    d.write_note(EDITOR_PATH, "font_size = 26\n", &h0)
        .expect("external");
    let _ = app.send(s(app.settings.due_now(EDITOR_PATH)));
    assert_eq!(d.read_file(EDITOR_PATH).expect("r").0, b"font_size = 26\n");
    assert_eq!(
        app.settings.file(EDITOR_PATH).and_then(|f| f.notice()),
        Some(CHANGED_ON_DISK)
    );
    assert!((app.prefs.font_size - 26.0).abs() < f32::EPSILON);
    assert_eq!(
        app.prefs.delete_mode,
        crate::daemon::DeleteMode::Trash,
        "the lost edit is not applied"
    );
}

#[test]
fn external_edit_is_picked_up_live() {
    let (_tmp, d) = fake();
    let h0 = d
        .write_note(EDITOR_PATH, "font_size = 20\n", &ContentHash::from_hex(""))
        .expect("seed");
    let mut app = app_over(&d, None);
    let h1 = d
        .write_note(EDITOR_PATH, "font_size = 40\nui_scale = 2.0\n", &h0)
        .expect("external");
    changed(&mut app, EDITOR_PATH, Some(h1.as_str().to_owned()));
    assert!((app.prefs.font_size - 40.0).abs() < f32::EPSILON);
    assert!((app.scale_factor() - 2.0).abs() < f32::EPSILON);
    // Our own write's echo (same hash) is not a reload.
    let _ = app.send(pref("ui_scale", ScalarValue::Float(1.25)));
    changed(&mut app, EDITOR_PATH, Some(h1.as_str().to_owned()));
    assert!(app.settings.file(EDITOR_PATH).is_some_and(|f| f.dirty()));
}

#[test]
fn malformed_editor_toml_falls_back_and_is_kept() {
    let (_tmp, d) = fake();
    let bad = "font_size = [oops\n";
    d.write_note(EDITOR_PATH, bad, &ContentHash::from_hex(""))
        .expect("seed");
    let mut app = app_over(&d, None);
    assert_eq!(app.prefs, Prefs::default());
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert!(matches!(f.state(), LoadState::Malformed(_)));
    let _ = app.send(font(30.0));
    let _ = app.send(s(app.settings.due_now(EDITOR_PATH)));
    assert_eq!(d.read_file(EDITOR_PATH).expect("r").0, bad.as_bytes());
    // Start over + Save replaces it through CAS on the malformed content.
    let _ = app.send(s(SettingsMsg::StartOver(EDITOR_PATH.into())));
    let _ = app.send(font(30.0));
    let _ = app.send(s(app.settings.due_now(EDITOR_PATH)));
    let text = String::from_utf8(d.read_file(EDITOR_PATH).expect("r").0).expect("utf8");
    assert!(text.contains("font_size = 30"), "{text}");
}

#[test]
fn surround_migrates_once_from_ui_state() {
    let (tmp, d) = fake();
    let state = tmp.path().join("state/editor.json");
    std::fs::create_dir_all(state.parent().expect("p")).expect("dir");
    std::fs::write(&state, br#"{"surround":"light"}"#).expect("state");
    let mut app = app_over(&d, Some(state.clone()));
    assert_eq!(app.prefs.surround, Surround::Light, "seeded from UiState");
    assert!(d.read_file(EDITOR_PATH).is_err(), "seeding never writes");
    assert_eq!(
        app.settings
            .file(EDITOR_PATH)
            .and_then(|f| f.doc())
            .and_then(|doc| doc.get_str(&Path::root().key("surround"))),
        Some("light")
    );
    // After a save, editor.toml is the source (the local copy no longer wins).
    let _ = app.send(pref("surround", ScalarValue::Str("dark".into())));
    let _ = app.send(s(app.settings.due_now(EDITOR_PATH)));
    std::fs::write(&state, br#"{"surround":"med"}"#).expect("state");
    let app = app_over(&d, Some(state));
    assert_eq!(app.prefs.surround, Surround::Dark);
}

#[test]
fn theme_cycle_edits_editor_toml_in_memory_only() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Theme(crate::message::ThemeMsg::Cycle));
    assert_eq!(app.prefs.surround, Surround::Light);
    assert!(app.settings.file(EDITOR_PATH).is_some_and(|f| f.dirty()));
    assert!(d.read_file(EDITOR_PATH).is_err(), "no write without Save");
}

#[test]
fn font_size_change_resizes_source_and_reading_live() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let before = crate::reading::view::scaled(
        crate::reading::view::block_size(None),
        app.prefs.text_scale(),
    );
    let _ = app.send(font(44.0));
    assert!(
        (app.prefs.font_size - 44.0).abs() < f32::EPSILON,
        "source size"
    );
    let after = crate::reading::view::scaled(
        crate::reading::view::block_size(None),
        app.prefs.text_scale(),
    );
    assert!((after - 2.0 * before).abs() < 1e-4, "{before} → {after}");
    // ui_scale is clamped for iced.
    app.prefs.ui_scale = 9.0;
    assert!((app.scale_factor() - 3.0).abs() < f32::EPSILON);
}

#[test]
fn palette_hidden_and_search_prefs_apply() {
    let (_tmp, d) = fake();
    d.write_note(
        EDITOR_PATH,
        "[palette]\nhidden = [\"quit\"]\n[search]\nlimit = 7\ndebounce_ms = 0\n",
        &ContentHash::from_hex(""),
    )
    .expect("seed");
    let app = app_over(&d, None);
    assert!(
        !app.palette
            .entries
            .iter()
            .any(|e| e.id == crate::palette::EntryId::Client(crate::palette::ClientCmd::Quit))
    );
    assert_eq!(app.prefs.search_limit, 7);
    assert_eq!(app.prefs.search_debounce(), std::time::Duration::ZERO);
}

#[test]
fn explorer_trash_always_confirms_with_orphan_ticks() {
    use crate::dialog::DialogMsg;
    let (_tmp, d) = fake();
    d.write_note(
        EDITOR_PATH,
        "confirm_trash = false\n",
        &ContentHash::from_hex(""),
    )
    .expect("seed");
    d.write_note("wp3del/own.png", "o", &ContentHash::from_hex(""))
        .expect("own");
    d.write_note("wp3del/shared.png", "s", &ContentHash::from_hex(""))
        .expect("shared");
    d.create_note("wp3del/m.md", "![[shared.png]]\n")
        .expect("m");
    d.create_note("wp3del/one.md", "![[own.png]] ![[shared.png]]\n")
        .expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Refresh));
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp3del/one.md".into())));
    let _ = app.send(Message::Explorer(ExplorerMsg::Select(
        "wp3del/one.md".into(),
    )));
    let _ = app.send(Message::Explorer(ExplorerMsg::Trash));
    let c = app
        .dialog
        .as_ref()
        .and_then(|d| match d {
            crate::dialog::Dialog::DeleteConfirm(c) => Some(c),
            _ => None,
        })
        .expect("delete dialog (legacy confirm_trash = false is ignored)");
    assert_eq!(c.ticked(), ["wp3del/own.png"]);
    assert_eq!(c.items.len(), 2);
    // Escape = Cancel: nothing deleted.
    let _ = app.send(Message::Dialog(DialogMsg::Cancel));
    assert!(app.dialog.is_none());
    assert!(d.read_file("wp3del/one.md").is_ok(), "not yet trashed");
    // Again, and the referenced item cannot be ticked.
    let _ = app.send(Message::Explorer(ExplorerMsg::Trash));
    let shared_ix = app
        .dialog
        .as_ref()
        .and_then(|d| match d {
            crate::dialog::Dialog::DeleteConfirm(c) => Some(c),
            _ => None,
        })
        .and_then(|c| c.items.iter().position(|(i, _)| i.referenced_elsewhere))
        .expect("shared listed");
    let _ = app.send(Message::Dialog(DialogMsg::Toggle(shared_ix)));
    assert_eq!(
        app.dialog
            .as_ref()
            .and_then(|d| match d {
                crate::dialog::Dialog::DeleteConfirm(c) => Some(c),
                _ => None,
            })
            .map(crate::dialog::DeleteConfirm::ticked),
        Some(vec!["wp3del/own.png".to_owned()])
    );
    // Enter is Cancel on this destructive dialog.
    let _ = app.send(Message::Dialog(DialogMsg::Default));
    assert!(d.read_file("wp3del/one.md").is_ok());
    let _ = app.send(Message::Explorer(ExplorerMsg::Trash));
    let _ = app.send(Message::Dialog(DialogMsg::Ok));
    assert!(d.read_file("wp3del/one.md").is_err(), "deleted after OK");
    assert!(d.read_file("wp3del/own.png").is_err(), "orphan deleted");
    assert!(d.read_file("wp3del/shared.png").is_ok(), "shared kept");
    assert!(
        d.vault_root().join(".trash/wp3del/one.md").is_file(),
        "trash mode"
    );
    assert!(
        crate::panes::find(&app.panes, "wp3del/one.md").is_none(),
        "open tab closed"
    );
}

#[test]
fn delete_note_command_honours_permanent_mode() {
    let (_tmp, d) = fake();
    d.write_note(
        EDITOR_PATH,
        "delete_mode = \"permanent\"\n",
        &ContentHash::from_hex(""),
    )
    .expect("seed");
    d.create_note("wp3perm/a.md", "x\n").expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp3perm/a.md".into())));
    let _ = app.send(Message::Explorer(ExplorerMsg::DeleteActive));
    let _ = app.send(Message::Dialog(crate::dialog::DialogMsg::Ok));
    assert!(d.read_file("wp3perm/a.md").is_err());
    assert!(
        !d.vault_root().join(".trash").exists(),
        "permanent: not in .trash"
    );
}

// ---- app: daemon feedback ----

#[test]
fn config_reloaded_errors_route_to_pages_and_status_refreshes() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let _ = app.send(s(SettingsMsg::Open));
    let _ = app.send(s(SettingsMsg::Section(Section::Status)));
    assert!(app.settings.status.result.is_some(), "status on open");
    app.settings.status.result = None;
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::ConfigReloaded {
            errors: vec!["providers.toml: bad (line 2)".into(), "mystery".into()],
        },
    )));
    assert_eq!(
        app.settings
            .file(Section::Providers.file().expect("p"))
            .expect("f")
            .reload_errors(),
        ["providers.toml: bad (line 2)"]
    );
    assert_eq!(app.settings.status.unrouted, ["mystery"]);
    assert!(app.settings.status.result.is_some(), "refreshed on reload");
}

#[test]
fn bookmarks_absent_then_appeared_is_a_conflict_not_a_copy() {
    let (_tmp, d) = fake();
    d.create_note("wp2bm/a.md", "a").expect("create");
    let mut app = app_over(&d, None);
    assert!(
        app.settings
            .file(BOOKMARKS_PATH)
            .is_some_and(|f| f.base().is_none())
    );
    let theirs = "[[bookmark]]\nname = \"t\"\npath = \"t.md\"\n";
    d.write_note(BOOKMARKS_PATH, theirs, &ContentHash::from_hex(""))
        .expect("external");
    let _ = app.send(Message::Tabs(TabsMsg::Open("wp2bm/a.md".into())));
    let _ = app.send(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::AddActive)));
    assert_eq!(d.read_file(BOOKMARKS_PATH).expect("r").0, theirs.as_bytes());
    assert_eq!(
        config_dir_entries(&d),
        ["bookmarks.toml"],
        "no suffixed copy"
    );
    assert!(
        app.status
            .error_text()
            .is_some_and(|e| e.contains("changed on disk"))
    );
    assert_eq!(app.bookmarks.items.len(), 1, "reloaded theirs");
}

#[test]
fn settings_save_of_a_daemon_file_round_trips_comments() {
    let (_tmp, d) = fake();
    let kinds = Section::Kinds.file().expect("p");
    d.write_note(
        kinds,
        "# my kinds\n[kinds.book]\nfolder = \"books\" # keep\n",
        &ContentHash::from_hex(""),
    )
    .expect("seed");
    let mut app = app_over(&d, None);
    let _ = app.send(s(SettingsMsg::Open));
    let _ = app.send(s(SettingsMsg::Edit(
        kinds.into(),
        DocEdit::Set(
            Path::parse("kinds.book.folder").expect("p"),
            ScalarValue::Str("library".into()),
        ),
    )));
    assert!(app.settings.file(kinds).is_some_and(|f| f.dirty()));
    let _ = app.send(s(app.settings.due_now(kinds)));
    let text = String::from_utf8(d.read_file(kinds).expect("r").0).expect("utf8");
    assert!(
        text.contains("# my kinds") && text.contains("# keep"),
        "{text}"
    );
    assert!(text.contains("folder = \"library\""), "{text}");
}
