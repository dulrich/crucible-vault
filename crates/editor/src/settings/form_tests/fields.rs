//! Field kinds: numbers, optional text, chips, typeaheads, providers, captures, secrets.

use super::*;

#[test]
fn numbers_accept_int_for_float_and_keep_bad_drafts() {
    let ed = editor_prefs::EDITOR_PATH;
    let mut st = loaded(ed, "font_size = 20\n");
    st.form_msg(
        ed,
        FormMsg::Number {
            path: p("font_size"),
            text: "18".into(),
            float: true,
        },
    );
    assert!(doc_text(&st, ed).contains("font_size = 18"));
    st.form_msg(
        ed,
        FormMsg::Number {
            path: p("font_size"),
            text: "17.5".into(),
            float: true,
        },
    );
    assert!(doc_text(&st, ed).contains("font_size = 17.5"));
    st.form_msg(
        ed,
        FormMsg::Number {
            path: p("font_size"),
            text: "17.".into(),
            float: true,
        },
    );
    st.form_msg(
        ed,
        FormMsg::Number {
            path: p("font_size"),
            text: "x".into(),
            float: true,
        },
    );
    assert!(
        doc_text(&st, ed).contains("font_size = 17"),
        "bad text not applied"
    );
    let rows = rows_of(&st, ed);
    let r = rows.iter().find(|r| r.path == p("font_size")).expect("row");
    assert!(matches!(&r.kind, Kind::Number { text, .. } if text == "x"));
    assert!(!r.errors.is_empty());
    // Out of range is a form error that blocks Save.
    st.form_msg(
        ed,
        FormMsg::Number {
            path: p("font_size"),
            text: "500".into(),
            float: true,
        },
    );
    let f = st.file(ed).expect("f");
    assert!(!f.errors().is_empty() && !f.can_save());
}

#[test]
fn empty_optional_text_removes_the_key_required_keeps_it() {
    let mut st = loaded(ACTIONS, ONE_ACTION);
    st.form_msg(
        ACTIONS,
        FormMsg::Text {
            path: p("action[0].title"),
            text: String::new(),
            required: false,
            picker: None,
        },
    );
    assert!(!doc_text(&st, ACTIONS).contains("title"));
    st.form_msg(
        ACTIONS,
        FormMsg::Text {
            path: p("action[0].id"),
            text: String::new(),
            required: true,
            picker: None,
        },
    );
    assert!(doc_text(&st, ACTIONS).contains("id = \"\""));
}

#[test]
fn chips_add_and_remove() {
    let ed = editor_prefs::EDITOR_PATH;
    let mut st = loaded(ed, "[palette]\nhidden = [\"a\"]\n");
    let at = p("palette.hidden");
    st.form_msg(
        ed,
        FormMsg::ChipDraft {
            path: at.clone(),
            text: " b ".into(),
            picker: None,
        },
    );
    st.form_msg(
        ed,
        FormMsg::ChipAdd {
            path: at.clone(),
            value: None,
        },
    );
    let list = |st: &Settings| {
        st.file(ed)
            .and_then(|f| f.doc())
            .and_then(|d| d.get_str_list(&at))
    };
    assert_eq!(list(&st), Some(vec!["a".to_owned(), "b".to_owned()]));
    assert!(st.form.draft(ed, &form::draft_key("chip", &at)).is_none());
    st.form_msg(
        ed,
        FormMsg::ChipRemove {
            path: at.clone(),
            index: 0,
        },
    );
    assert_eq!(list(&st), Some(vec!["b".to_owned()]));
}

#[test]
fn path_field_typeahead_picks_and_escape_restores() {
    use crate::widget::typeahead::Key;
    let mut st = loaded(
        BOOKMARKS_PATH,
        "[[bookmark]]\nname = \"n\"\npath = \"a.md\"\n",
    );
    let paths = vec!["notes/x.md".to_owned(), "a.md".to_owned()];
    let at = p("bookmark[0].path");
    let typed = |st: &mut Settings, t: &str| {
        st.form_msg_paths(
            BOOKMARKS_PATH,
            FormMsg::Text {
                path: at.clone(),
                text: t.into(),
                required: true,
                picker: Some(form::Picker::Path),
            },
            &paths,
        );
    };
    st.form_msg(BOOKMARKS_PATH, FormMsg::Toggle(p("bookmark[0]")));
    let rows = rows_of(&st, BOOKMARKS_PATH);
    assert!(rows.iter().any(|r| matches!(
        &r.kind,
        Kind::Text {
            picker: Some(form::Picker::Path),
            ..
        }
    )));
    // Typing is a query: nothing written; Escape on an invalid text restores the committed.
    typed(&mut st, "zzz");
    assert!(st.form.typeahead.as_ref().is_some_and(|a| a.ta.open));
    assert!(doc_text(&st, BOOKMARKS_PATH).contains("path = \"a.md\""));
    st.typeahead_key(Key::Escape, &paths);
    assert!(st.form.typeahead.is_none());
    assert!(doc_text(&st, BOOKMARKS_PATH).contains("path = \"a.md\""));
    // Enter picks the highlighted candidate.
    typed(&mut st, "notx");
    assert_eq!(st.ta_visible(&paths)[0].value, "notes/x.md");
    st.typeahead_key(Key::Enter, &paths);
    assert!(doc_text(&st, BOOKMARKS_PATH).contains("path = \"notes/x.md\""));
    // Click picks; `x` clears.
    st.form_msg_paths(
        BOOKMARKS_PATH,
        FormMsg::Pick {
            path: at.clone(),
            value: "a.md".into(),
        },
        &paths,
    );
    assert!(doc_text(&st, BOOKMARKS_PATH).contains("path = \"a.md\""));
    st.form_msg(
        BOOKMARKS_PATH,
        FormMsg::Clear {
            path: at.clone(),
            required: true,
        },
    );
    assert!(doc_text(&st, BOOKMARKS_PATH).contains("path = \"\""));
}

#[test]
fn typeahead_widget_contract() {
    use crate::widget::typeahead::{Candidate, Key, Outcome, Typeahead, filter};
    let cands = vec![
        Candidate::plain("alpha"),
        Candidate::plain("beta"),
        Candidate::plain("alphabet"),
    ];
    let mut ta = Typeahead::begin("beta");
    ta.typed();
    let vis = filter("alp", &cands, 8);
    assert_eq!(vis.len(), 2);
    assert_eq!(
        ta.key(Key::Escape, "alp", &vis, false),
        Outcome::Restore("beta".into())
    );
    assert!(!ta.open);
    ta.typed();
    assert_eq!(ta.key(Key::Escape, "alpha", &vis, true), Outcome::None);
    assert_eq!(ta.committed, "alpha");
    ta.typed();
    ta.key(Key::Down, "alp", &vis, false);
    assert_eq!(
        ta.key(Key::Enter, "alp", &vis, false),
        Outcome::Commit(vis[1].value.clone())
    );
    assert_eq!(ta.pick("beta"), Outcome::Commit("beta".into()));
    assert_eq!(ta.clear(), Outcome::Commit(String::new()));
    assert_eq!(ta.committed, "");
    // Enter with nothing visible commits the typed text (explicit free text).
    ta.typed();
    assert_eq!(
        ta.key(Key::Enter, "custom", &[], false),
        Outcome::Commit("custom".into())
    );
}

#[test]
fn folder_field_offers_folders() {
    let mut st = loaded(
        "_custom/crucible-vault/captures.toml",
        "[periods.day]\nfolder = \"journal\"\n",
    );
    st.section = Section::Captures;
    let paths = vec!["journal/2026/a.md".to_owned(), "notes/b.md".to_owned()];
    let at = p("periods.day.folder");
    let picker = picker_of("_custom/crucible-vault/captures.toml", &at);
    assert_eq!(picker, Some(form::Picker::Folder));
    st.form_msg_paths(
        "_custom/crucible-vault/captures.toml",
        FormMsg::Text {
            path: at,
            text: "jour".into(),
            required: true,
            picker,
        },
        &paths,
    );
    let vals: Vec<String> = st.ta_visible(&paths).into_iter().map(|c| c.value).collect();
    assert!(vals.contains(&"journal".to_owned()));
    assert!(vals.contains(&"journal/2026".to_owned()));
    assert!(!vals.iter().any(|v| v.ends_with(".md")));
}

#[test]
fn provider_shows_name_and_stores_id() {
    use crate::widget::typeahead::Key;
    const PIPES: &str = "_custom/crucible-vault/pipelines.toml";
    let mut st = loaded(
        "_custom/crucible-vault/providers.toml",
        "[[provider]]\nid = \"oai\"\nkind = \"openai-compatible\"\nname = \"OpenAI\"\n\n[[provider.models]]\nid = \"gpt-x\"\n\n[[provider]]\nid = \"local\"\nkind = \"ollama\"\n",
    );
    let f = ConfigFile::new(PIPES, cv_schema::schema_for("pipelines.toml"));
    st.files.insert(PIPES.to_owned(), f);
    st.file_mut(PIPES).expect("pipes").apply_read(Ok((
        b"[image_describe]\n".to_vec(),
        ContentHash::from_hex("00"),
    )));
    let labels: Vec<(String, String)> = pickers::providers(&st)
        .into_iter()
        .map(|c| (c.label, c.value))
        .collect();
    assert_eq!(
        labels,
        vec![
            ("OpenAI".to_owned(), "oai".to_owned()),
            ("local".to_owned(), "local".to_owned())
        ]
    );
    let at = p("image_describe.provider");
    let picker = picker_of(PIPES, &at);
    assert_eq!(picker, Some(form::Picker::Provider));
    st.form_msg(
        PIPES,
        FormMsg::Text {
            path: at,
            text: "open".into(),
            required: false,
            picker,
        },
    );
    st.typeahead_key(Key::Enter, &[]);
    assert!(doc_text(&st, PIPES).contains("provider = \"oai\""));
    assert_eq!(
        pickers::label_of(&st, form::Picker::Provider, "oai").as_deref(),
        Some("OpenAI")
    );
    // The model picker lists the provider's declared models, labelled "declared".
    let models = pickers::declared_models(&st, Some("oai"));
    assert_eq!(models.len(), 1);
    assert_eq!(
        (models[0].value.as_str(), models[0].secondary.as_str()),
        ("gpt-x", "declared")
    );
}

#[test]
fn ignore_shorts_channel_chips_label_and_summary() {
    let st = loaded(
        "_custom/crucible-vault/sources.toml",
        "[[youtube]]\nchannel = \"Chan\"\nid = \"UC123\"\n",
    );
    let chans = pickers::channels(&st);
    assert_eq!(pickers::channel_label(&chans, "UC123"), "Chan (UC123)");
    assert_eq!(pickers::channel_label(&chans, "Free Name"), "Free Name");
    assert_eq!(
        picker_of(
            "_custom/crucible-vault/pipelines.toml",
            &p("ignore_shorts.channels")
        ),
        Some(form::Picker::Channel)
    );
    let rows = rows_of(&st, "_custom/crucible-vault/sources.toml");
    assert!(rows.iter().any(|r| matches!(
        &r.kind,
        Kind::Item { summary, .. } if summary == "Chan"
    )));
}

#[test]
fn enums_free_tables_and_mistyped_values_render() {
    let text = "[[action]]\nid = \"a\"\nrisk = \"user\"\ntitle = 3\n\n[[action.steps]]\ntype = \"set_props\"\nprops = { a = 1 }\n";
    let mut st = loaded(ACTIONS, text);
    st.form_msg(ACTIONS, FormMsg::Toggle(p("action[0]")));
    st.form_msg(ACTIONS, FormMsg::Toggle(p("action[0].steps[0]")));
    let rows = rows_of(&st, ACTIONS);
    let mistyped = rows
        .iter()
        .find(|r| r.path == p("action[0].title"))
        .expect("title");
    assert!(matches!(&mistyped.kind, Kind::ReadOnly { toml } if toml == "3"));
    let props = rows
        .iter()
        .find(|r| r.path == p("action[0].steps[0].props"))
        .expect("props");
    assert!(matches!(&props.kind, Kind::ReadOnly { toml } if toml.contains("a = 1")));
    assert!(rows.iter().any(|r| matches!(&r.kind, Kind::Enum { .. })));
}

#[test]
fn captures_page_puts_periods_first() {
    let text = "[[capture]]\nid = \"c\"\n\n[periods.day]\nfolder = \"daily\"\n";
    let st = loaded("_custom/crucible-vault/captures.toml", text);
    let f = st.file("_custom/crucible-vault/captures.toml").expect("f");
    let rows = pages::file_rows(&st, f);
    assert_eq!(rows[0].label, "Period notes");
    let pi = rows
        .iter()
        .position(|r| r.label == "Captures")
        .expect("captures");
    let di = rows
        .iter()
        .position(|r| r.path == p("periods.day.folder"))
        .expect("day folder");
    assert!(di < pi);
}

#[test]
fn providers_page_never_shows_a_secret_value() {
    // `PATH` stands in for a key env var: the page shows the *name*, never the value.
    let text = "[[provider]]\nid = \"o\"\nkind = \"openai-compatible\"\napi_key_env = \"PATH\"\napi_key_file = \"openai.key\"\n";
    let file = "_custom/crucible-vault/providers.toml";
    let mut st = loaded(file, text);
    st.form_msg(file, FormMsg::Toggle(p("provider[0]")));
    let shown = form::shown_text(&rows_of(&st, file));
    assert!(shown.iter().any(|s| s == "PATH"));
    assert!(shown.iter().any(|s| s == "openai.key"));
    if let Ok(v) = std::env::var("PATH")
        && !v.is_empty()
    {
        assert!(!shown.iter().any(|s| s.contains(&v)), "env value shown");
    }
}
