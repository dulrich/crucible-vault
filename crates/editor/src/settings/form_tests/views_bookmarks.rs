//! Round trip, the Views carve-out, the shared bookmarks file and the Actions page build time.

use super::*;

#[test]
fn form_edit_round_trip_keeps_comment_and_unknown_key_in_saved_text() {
    let (_tmp, d) = fake();
    d.write_note(ACTIONS, ONE_ACTION, &ContentHash::from_hex(""))
        .expect("seed");
    let mut app = app_over(&d);
    let _ = app.send(s(SettingsMsg::Open));
    let _ = app.send(s(SettingsMsg::Form(
        ACTIONS.into(),
        FormMsg::Text {
            path: p("action[0].title"),
            text: "Tidy up".into(),
            required: false,
            picker: None,
        },
    )));
    let _ = app.send(s(app.settings.due_now(ACTIONS)));
    let text = String::from_utf8(d.read_file(ACTIONS).expect("r").0).expect("utf8");
    assert!(text.contains("# my actions"), "{text}");
    assert!(text.contains("mystery = 7 # unknown, kept"), "{text}");
    assert!(text.contains("title = \"Tidy up\""), "{text}");
    assert!(app.settings.file(ACTIONS).is_some_and(|f| !f.dirty()));
}

const BAD_FILTER_VIEW: &str = "[[views]]\ntype = \"table\"\nname = \"T\"\nfilters = \"((\"\n";

#[test]
fn views_check_parse_error_blocks_and_compile_notice_warns() {
    let (errs, _) = views_check("_custom/crucible-vault/views/v.toml", "invented = 1\n");
    assert_eq!(errs.len(), 1);
    let (errs, warns) = views_check("_custom/crucible-vault/views/v.toml", BAD_FILTER_VIEW);
    assert!(errs.is_empty(), "{errs:?}");
    assert!(!warns.is_empty(), "compile notice");
    let (errs, warns) = views_check(
        "_custom/crucible-vault/views/v.toml",
        "[[views]]\ntype = \"table\"\nname = \"T\"\n",
    );
    assert!(errs.is_empty() && warns.is_empty());
}

#[test]
fn views_page_parse_error_blocks_save() {
    let mut st = Settings::default();
    let path = st.new_view_file("mine").expect("new");
    assert_eq!(path, "_custom/crucible-vault/views/mine.toml");
    assert!(
        st.file(&path).is_some_and(ConfigFile::can_save),
        "empty view saves"
    );
    st.edit(&path, &DocEdit::Set(p("invented"), ScalarValue::Int(1)))
        .expect("edit");
    let f = st.file(&path).expect("f");
    assert!(
        !f.errors().is_empty() && !f.can_save(),
        "parse error blocks Save"
    );
    st.edit(&path, &DocEdit::Remove(p("invented"))).expect("rm");
    assert!(st.file(&path).is_some_and(ConfigFile::can_save));
    // A compile notice is a warning only.
    let mut st = loaded("_custom/crucible-vault/views/w.toml", BAD_FILTER_VIEW);
    let w = "_custom/crucible-vault/views/w.toml";
    st.files
        .get_mut(w)
        .expect("w")
        .set_local_check(Some(views_check));
    st.edit(
        w,
        &DocEdit::Set(p("views[0].name"), ScalarValue::Str("U".into())),
    )
    .expect("edit");
    let f = st.file(w).expect("f");
    assert!(f.errors().is_empty() && !f.warnings().is_empty() && f.can_save());
}

#[test]
fn new_view_file_names_are_checked() {
    let mut st = Settings::default();
    assert!(st.new_view_file("").is_err());
    assert!(st.new_view_file("a/b").is_err());
    assert!(st.new_view_file("ok.toml").is_ok());
    assert!(st.new_view_file("ok").is_err(), "exists");
}

#[test]
fn sync_views_registers_view_files_from_the_path_list() {
    let mut st = Settings::default();
    st.sync_views(&[
        "_custom/crucible-vault/views/b.toml".into(),
        "_custom/crucible-vault/views/a.toml".into(),
        "_custom/crucible-vault/views/sub/x.toml".into(),
        "_custom/crucible-vault/actions.toml".into(),
        "notes/a.md".into(),
    ]);
    assert_eq!(
        st.view_paths(),
        [
            "_custom/crucible-vault/views/a.toml",
            "_custom/crucible-vault/views/b.toml"
        ]
    );
    assert_eq!(
        st.view_selected.as_deref(),
        Some("_custom/crucible-vault/views/a.toml")
    );
    assert!(
        st.file("_custom/crucible-vault/views/a.toml")
            .is_some_and(|f| f.local_check().is_some())
    );
}

#[test]
fn new_view_file_saves_at_the_exact_name() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d);
    let _ = app.send(s(SettingsMsg::Open));
    let _ = app.send(s(SettingsMsg::Section(Section::Views)));
    let _ = app.send(s(SettingsMsg::NewViewName("fresh".into())));
    let _ = app.send(s(SettingsMsg::NewViewCreate));
    let path = "_custom/crucible-vault/views/fresh.toml";
    assert_eq!(app.settings.view_selected.as_deref(), Some(path));
    let _ = app.send(s(SettingsMsg::Edit(
        path.into(),
        DocEdit::InsertRecord(
            p("views"),
            0,
            vec![
                ("type".into(), ScalarValue::Str("table".into())),
                ("name".into(), ScalarValue::Str("All".into())),
            ],
        ),
    )));
    let _ = app.send(s(app.settings.due_now(path)));
    let text = String::from_utf8(d.read_file(path).expect("saved").0).expect("utf8");
    assert!(text.contains("name = \"All\""), "{text}");
    assert!(
        app.settings
            .file(path)
            .is_some_and(|f| !f.dirty() && f.base().is_some())
    );
}

#[test]
fn bookmarks_page_and_panel_share_one_hash() {
    let (_tmp, d) = fake();
    d.create_note("wp3bm/a.md", "a").expect("create");
    d.create_note("wp3bm/b.md", "b").expect("create");
    let mut app = app_over(&d);
    let _ = app.send(Message::Tabs(TabsMsg::Open("wp3bm/a.md".into())));
    let _ = app.send(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::AddActive)));
    let disk = |d: &FakeDaemon| d.read_file(BOOKMARKS_PATH).expect("r");
    let base = |app: &App| {
        app.settings
            .file(BOOKMARKS_PATH)
            .and_then(|f| f.base().cloned())
    };
    assert_eq!(
        base(&app),
        Some(disk(&d).1),
        "panel save moved the shared hash"
    );
    assert_eq!(app.bookmarks.items.len(), 1);
    // The page edits the same doc and saves without conflict.
    let _ = app.send(s(SettingsMsg::Open));
    let _ = app.send(s(SettingsMsg::Edit(
        BOOKMARKS_PATH.into(),
        DocEdit::Set(p("bookmark[0].name"), ScalarValue::Str("Alpha".into())),
    )));
    // While the page is dirty the panel waits.
    let _ = app.send(Message::Tabs(TabsMsg::Open("wp3bm/b.md".into())));
    let _ = app.send(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::AddActive)));
    assert!(
        app.status
            .error_text()
            .is_some_and(|e| e.contains("unsaved"))
    );
    let _ = app.send(s(app.settings.due_now(BOOKMARKS_PATH)));
    assert!(
        app.settings
            .file(BOOKMARKS_PATH)
            .is_some_and(|f| f.notice().is_none() && !f.dirty())
    );
    assert_eq!(base(&app), Some(disk(&d).1));
    assert_eq!(
        app.bookmarks.items[0].name, "Alpha",
        "panel sees the page save"
    );
    // And the panel saves again on top of the page's save (no conflict).
    app.status.notice = None;
    let _ = app.send(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::AddActive)));
    assert!(app.status.error_text().is_none(), "{:?}", app.status.notice);
    let items = crate::bookmarks::parse(&disk(&d).0).expect("parse");
    assert_eq!(items.len(), 2);
    assert_eq!(base(&app), Some(disk(&d).1));
}

#[test]
fn panel_edit_keeps_comments_in_bookmarks_toml() {
    let (_tmp, d) = fake();
    d.create_note("wp3bc/a.md", "a").expect("create");
    d.write_note(
        BOOKMARKS_PATH,
        "# mine\n[[bookmark]]\nname = \"x\" # keep\npath = \"x.md\"\n",
        &ContentHash::from_hex(""),
    )
    .expect("seed");
    let mut app = app_over(&d);
    assert_eq!(app.bookmarks.items.len(), 1);
    let _ = app.send(Message::Tabs(TabsMsg::Open("wp3bc/a.md".into())));
    let _ = app.send(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::AddActive)));
    let text = String::from_utf8(d.read_file(BOOKMARKS_PATH).expect("r").0).expect("utf8");
    assert!(text.contains("# mine") && text.contains("# keep"), "{text}");
    assert_eq!(app.bookmarks.items.len(), 2);
}

/// A synthetic `actions.toml` of about the live file's size (~250 lines).
fn big_actions() -> String {
    let mut s = String::from("# synthetic actions\n");
    for i in 0..15 {
        s.push_str(&format!(
            "\n[[action]]\nid = \"act{i}\"\ntitle = \"Action {i}\"\ngroup = \"G\"\nkeys = [\"Ctrl+Alt+{}\"]\n\n[[action.params]]\nname = \"p\"\nlabel = \"P\"\n\n[[action.steps]]\ntype = \"move\"\nto = \"x/{i}\"\n\n[[action.steps]]\ntype = \"lint\"\n",
            i % 10
        ));
    }
    s
}

#[test]
fn actions_page_build_timing() {
    let text = big_actions();
    let lines = text.lines().count();
    assert!((230..=290).contains(&lines), "{lines} lines");
    let mut st = loaded(ACTIONS, &text);
    st.section = Section::Actions;
    let paths: Vec<String> = Vec::new();
    let t = crate::theme::Surround::default().tokens().expect("tokens");
    // Warm once, then take the best of 5 builds (the open frame's view build).
    drop(page(&st, &paths, t));
    let mut best = f64::MAX;
    for _ in 0..5 {
        let t0 = std::time::Instant::now();
        let el = page(&st, &paths, t);
        best = best.min(t0.elapsed().as_secs_f64() * 1e3);
        drop(el);
    }
    eprintln!("actions page build ({lines} lines, collapsed): {best:.3} ms");
    #[cfg(not(debug_assertions))]
    assert!(best < 16.0, "{best} ms");
    // Fully expanded (every action and step): the worst case, reported only.
    for i in 0..15 {
        st.form.toggle(ACTIONS, &p(&format!("action[{i}]")));
        for j in 0..2 {
            st.form
                .toggle(ACTIONS, &p(&format!("action[{i}].steps[{j}]")));
        }
    }
    let t0 = std::time::Instant::now();
    drop(page(&st, &paths, t));
    eprintln!(
        "actions page build (fully expanded): {:.3} ms",
        t0.elapsed().as_secs_f64() * 1e3
    );
}

#[test]
fn form_rows_use_the_ctx_form_state() {
    let doc = ConfigDoc::parse("[[action]]\nid = \"a\"\n").expect("doc");
    let mut fs = FormState::default();
    fs.toggle(ACTIONS, &p("action[0]"));
    let rows = form::rows(
        &Ctx {
            file: ACTIONS,
            doc: &doc,
            errors: &[],
            form: &fs,
        },
        cv_schema::schema_for("actions.toml").expect("schema"),
    );
    assert!(rows.iter().any(|r| r.path == p("action[0].id")));
}
