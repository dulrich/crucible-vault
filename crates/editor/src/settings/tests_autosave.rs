use super::*;

// ---- plan editor-feedback-round-2 WP-2: auto-save ----

fn seq(app: &App, path: &str) -> u64 {
    app.settings.file(path).map_or(0, |f| f.edit_seq())
}

fn due(app: &mut App, path: &str) {
    let n = seq(app, path);
    let _ = app.send(s(SettingsMsg::AutoSaveDue(path.into(), n)));
}

fn disk(d: &FakeDaemon, path: &str) -> Option<String> {
    d.read_file(path)
        .ok()
        .map(|(b, _)| String::from_utf8(b).expect("utf8"))
}

#[test]
fn autosave_valid_edit_writes_once_after_the_debounce() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let _ = app.send(font(30.0));
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert_eq!(
        f.scheduled_seq(),
        f.edit_seq(),
        "a debounce timer was armed"
    );
    assert!(disk(&d, EDITOR_PATH).is_none(), "nothing before the timer");
    due(&mut app, EDITOR_PATH);
    let (_, h) = d.read_file(EDITOR_PATH).expect("written");
    assert!(disk(&d, EDITOR_PATH).is_some_and(|t| t.contains("font_size = 30")));
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert!(!f.dirty() && f.base() == Some(&h));
    assert_eq!(f.save_status().map(|s| s.0).as_deref(), Some("saved"));
    // The same timer again writes nothing more.
    due(&mut app, EDITOR_PATH);
    assert_eq!(d.read_file(EDITOR_PATH).expect("r").1, h);
}

#[test]
fn autosave_rapid_edits_coalesce_into_one_write() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let _ = app.send(font(20.0));
    let first = seq(&app, EDITOR_PATH);
    let _ = app.send(font(22.0));
    let _ = app.send(font(24.0));
    // The superseded timers fire: no write.
    let _ = app.send(s(SettingsMsg::AutoSaveDue(EDITOR_PATH.into(), first)));
    let _ = app.send(s(SettingsMsg::AutoSaveDue(EDITOR_PATH.into(), first + 1)));
    assert!(disk(&d, EDITOR_PATH).is_none(), "stale timers never write");
    due(&mut app, EDITOR_PATH);
    let text = disk(&d, EDITOR_PATH).expect("written");
    assert!(text.contains("font_size = 24"), "{text}");
}

#[test]
fn autosave_invalid_edit_never_writes() {
    let (_tmp, d) = fake();
    let kinds = "_custom/crucible-vault/kinds.toml";
    let seed = "[kinds.book]\nfolder = \"books\"\n";
    d.write_note(kinds, seed, &ContentHash::from_hex(""))
        .expect("seed");
    let mut app = app_over(&d, None);
    let _ = app.send(s(SettingsMsg::Open));
    let _ = app.send(s(SettingsMsg::Edit(
        kinds.into(),
        DocEdit::Set(
            Path::parse("kinds.book.folder").expect("p"),
            ScalarValue::Int(1),
        ),
    )));
    let f = app.settings.file(kinds).expect("f");
    assert!(f.dirty() && !f.errors().is_empty(), "{:?}", f.errors());
    assert_ne!(
        f.scheduled_seq(),
        f.edit_seq(),
        "no timer for an invalid state"
    );
    assert!(
        f.save_status()
            .is_some_and(|(l, warn)| warn && l.starts_with("not saved: ")),
        "{:?}",
        f.save_status()
    );
    due(&mut app, kinds);
    let _ = app.send(s(app.settings.due_now(kinds)));
    let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
    assert_eq!(
        disk(&d, kinds).as_deref(),
        Some(seed),
        "last valid content kept"
    );
}

#[test]
fn autosave_edit_during_an_inflight_write_requeues() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let _ = app.send(font(30.0));
    // The first write is in flight (not run inline).
    app.rpc_exec = crate::app::RpcExec::Pool;
    due(&mut app, EDITOR_PATH);
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert!(f.saving());
    let first = f.doc().expect("doc").to_string();
    let base = f.base().cloned();
    let _ = app.send(font(32.0));
    due(&mut app, EDITOR_PATH);
    assert!(app.settings.file(EDITOR_PATH).is_some_and(|f| f.queued()));
    // The first write lands; the queued one runs right after.
    app.queue_rpcs();
    let r = write_config(d.as_ref(), EDITOR_PATH, &first, base.as_ref());
    let _ = app.send(s(SettingsMsg::Saved(EDITOR_PATH.into(), r)));
    let text = disk(&d, EDITOR_PATH).expect("written");
    assert!(text.contains("font_size = 32"), "{text}");
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert!(!f.dirty() && !f.saving() && !f.queued());
}

/// The `Saved` completions waiting in the RPC queue (each is one write that ran).
fn queued_saves(app: &App) -> usize {
    match &app.rpc_exec {
        crate::app::RpcExec::Queue(q) => q
            .iter()
            .filter(|m| matches!(m, Message::Settings(SettingsMsg::Saved(..))))
            .count(),
        crate::app::RpcExec::Pool => 0,
    }
}

/// Pops the oldest queued `Saved` completion and lands it (without pumping).
fn land_one_save(app: &mut App) {
    let m = match &mut app.rpc_exec {
        crate::app::RpcExec::Queue(q) => {
            let i = q
                .iter()
                .position(|m| matches!(m, Message::Settings(SettingsMsg::Saved(..))))
                .expect("a queued save");
            q.remove(i).expect("save")
        }
        crate::app::RpcExec::Pool => panic!("queue mode"),
    };
    let _ = app.update(m);
}

#[test]
fn edit_during_an_in_flight_write_requeues_exactly_once() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let _ = app.send(font(30.0));
    // Write 1 runs; its completion stays queued (the write is "in flight").
    let _ = app.update(s(app.settings.due_now(EDITOR_PATH)));
    assert_eq!(queued_saves(&app), 1);
    assert!(
        app.settings
            .file(EDITOR_PATH)
            .is_some_and(ConfigFile::saving)
    );
    // The form stays editable during the write (no disabled-style flash per autosave).
    assert!(
        app.settings
            .file(EDITOR_PATH)
            .is_some_and(ConfigFile::editable)
    );
    // Two edits and their timers during the write: one requeue, no second write yet.
    let _ = app.update(font(32.0));
    let _ = app.update(s(app.settings.due_now(EDITOR_PATH)));
    let _ = app.update(font(34.0));
    let _ = app.update(s(app.settings.due_now(EDITOR_PATH)));
    assert_eq!(
        queued_saves(&app),
        1,
        "no write starts behind the in-flight one"
    );
    assert!(
        app.settings
            .file(EDITOR_PATH)
            .is_some_and(ConfigFile::queued)
    );
    // Write 1 lands: exactly one follow-up write runs.
    land_one_save(&mut app);
    assert_eq!(queued_saves(&app), 1, "one requeued write");
    assert!(
        app.settings
            .file(EDITOR_PATH)
            .is_some_and(|f| f.saving() && !f.queued())
    );
    // The follow-up lands: nothing more.
    land_one_save(&mut app);
    assert_eq!(queued_saves(&app), 0, "not two");
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert!(!f.dirty() && !f.saving() && !f.queued());
    assert!(disk(&d, EDITOR_PATH).is_some_and(|t| t.contains("font_size = 34")));
}

#[test]
fn autosave_cas_conflict_reloads_without_overwrite() {
    let (_tmp, d) = fake();
    let h0 = d
        .write_note(EDITOR_PATH, "font_size = 20\n", &ContentHash::from_hex(""))
        .expect("seed");
    let mut app = app_over(&d, None);
    let _ = app.send(font(30.0));
    d.write_note(EDITOR_PATH, "font_size = 26\n", &h0)
        .expect("external");
    due(&mut app, EDITOR_PATH);
    assert_eq!(disk(&d, EDITOR_PATH).as_deref(), Some("font_size = 26\n"));
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert_eq!(f.notice(), Some(CHANGED_ON_DISK));
    assert!(!f.dirty());
    assert!((app.prefs.font_size - 26.0).abs() < f32::EPSILON);
}

#[test]
fn autosave_views_parse_error_blocks_compile_warning_does_not() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let path = app.settings.new_view_file("mine").expect("new");
    let _ = app.send(s(SettingsMsg::Edit(
        path.clone(),
        DocEdit::Set(Path::root().key("invented"), ScalarValue::Int(1)),
    )));
    assert!(
        app.settings
            .file(&path)
            .is_some_and(|f| !f.errors().is_empty())
    );
    due(&mut app, &path);
    assert!(disk(&d, &path).is_none(), "a parse error blocks the write");
    let bad = "[[views]]\ntype = \"table\"\nname = \"T\"\nfilters = \"((\"\n";
    if let Some(f) = app.settings.file_mut(&path) {
        f.test_set_doc(Some(ConfigDoc::parse(bad).expect("doc")));
    }
    let _ = app.send(s(SettingsMsg::Edit(
        path.clone(),
        DocEdit::Set(
            Path::parse("views[0].name").expect("p"),
            ScalarValue::Str("U".into()),
        ),
    )));
    let f = app.settings.file(&path).expect("f");
    assert!(f.errors().is_empty() && !f.warnings().is_empty());
    due(&mut app, &path);
    let text = disk(&d, &path).expect("a compile warning does not block");
    assert!(text.contains("name = \"U\""), "{text}");
}

#[test]
fn autosave_flushes_on_settings_tab_close_and_quit() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let _ = app.send(s(SettingsMsg::Open));
    let _ = app.send(font(30.0));
    assert!(disk(&d, EDITOR_PATH).is_none());
    let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
    assert_eq!(settings_tabs(&app), 0);
    assert!(disk(&d, EDITOR_PATH).is_some_and(|t| t.contains("font_size = 30")));
    // Quit flushes a pending write before exiting.
    let _ = app.send(font(34.0));
    let _ = app.send(Message::Tabs(TabsMsg::Quit));
    assert!(disk(&d, EDITOR_PATH).is_some_and(|t| t.contains("font_size = 34")));
    assert!(app.exiting);
}

/// Plan usability-sprint-captures-links WP-3: `open_url_cmd` defaults to `xdg-open`, parses a
/// list, and an empty list falls back to the default.
#[test]
fn open_url_cmd_pref_default_and_override() {
    assert_eq!(editor_prefs::Prefs::default().open_url_cmd, ["xdg-open"]);
    let p = editor_prefs::parse(
        "open_url_cmd = [\"firefox\", \"--new-tab\"]\n",
        Surround::Med,
    )
    .expect("parse");
    assert_eq!(p.open_url_cmd, ["firefox", "--new-tab"]);
    let p = editor_prefs::parse("open_url_cmd = []\n", Surround::Med).expect("parse");
    assert_eq!(p.open_url_cmd, ["xdg-open"]);
}

/// Plan round-7 WP-3: a Settings save failure raises an Error notice in the bar and logs it.
#[test]
fn settings_save_failure_raises_an_error_notice() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let _ = app.send(font(30.0));
    let _ = app.send(s(SettingsMsg::Saved(
        EDITOR_PATH.into(),
        Err("disk full".into()),
    )));
    let n = app.status.notice.as_ref().expect("notice");
    assert_eq!(n.severity, crate::app::Severity::Error);
    assert_eq!(n.text, "save failed: disk full");
    let e = app.status.log.visible().next().expect("logged");
    assert_eq!(e.severity, crate::app::Severity::Error);
    assert_eq!(e.source.as_deref(), Some(EDITOR_PATH));
}

/// Plan round-8-intake-views-anchor WP-4: `scroll_anchor` parses 0–100 (default 0); out of
/// range or mistyped falls back to the default.
#[test]
fn scroll_anchor_parses_with_default() {
    let d = editor_prefs::parse("", Surround::Med).expect("parse");
    assert_eq!(d.scroll_anchor, 0);
    assert!(d.anchor().abs() < f32::EPSILON);
    let p = editor_prefs::parse("scroll_anchor = 50\n", Surround::Med).expect("parse");
    assert_eq!(p.scroll_anchor, 50);
    assert!((p.anchor() - 0.5).abs() < f32::EPSILON);
    for bad in [
        "scroll_anchor = 101\n",
        "scroll_anchor = -1\n",
        "scroll_anchor = \"x\"\n",
    ] {
        let p = editor_prefs::parse(bad, Surround::Med).expect("parse");
        assert_eq!(p.scroll_anchor, 0, "{bad}");
    }
}

/// Plan round-12-feedback-todos WP-3: `search.location` parses forgivingly.
#[test]
fn search_location_parses_modal_panel_and_unknown_falls_back_to_panel() {
    use editor_prefs::SearchLocation;
    let loc = |s: &str| {
        editor_prefs::parse(s, Surround::Med)
            .expect("parse")
            .search_location
    };
    assert_eq!(
        loc("[search]\nlocation = \"modal\"\n"),
        SearchLocation::Modal
    );
    assert_eq!(
        loc("[search]\nlocation = \"panel\"\n"),
        SearchLocation::Panel
    );
    assert_eq!(
        loc("[search]\nlocation = \"popup\"\n"),
        SearchLocation::Panel
    );
    assert_eq!(loc("[search]\nlocation = 3\n"), SearchLocation::Panel);
    assert_eq!(loc(""), SearchLocation::Panel);
}

/// Plan round-14 WP-2: `nav_open` absent → `new-tab`; `"replace"` parses; an unknown value falls
/// back to the default (the config page's schema validation raises the notice).
#[test]
fn nav_open_pref_default_replace_and_unknown() {
    let of = |body: &str| {
        editor_prefs::parse(body, Surround::Med)
            .expect("parse")
            .nav_open
    };
    assert_eq!(of(""), editor_prefs::NavOpen::NewTab);
    assert_eq!(
        of("nav_open = \"new-tab\"\n"),
        editor_prefs::NavOpen::NewTab
    );
    assert_eq!(
        of("nav_open = \"replace\"\n"),
        editor_prefs::NavOpen::Replace
    );
    assert_eq!(
        of("nav_open = \"sideways\"\n"),
        editor_prefs::NavOpen::NewTab
    );
}
