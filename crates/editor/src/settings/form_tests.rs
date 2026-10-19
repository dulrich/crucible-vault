//! plan editor-settings-tab WP-3 tests: the schema-driven form model, the document edits it
//! emits, the Views carve-out, the shared bookmarks file and the Actions page build time.
//! Synthetic inline TOML + `FakeDaemon` only.

use std::sync::Arc;

use cv_schema::{ConfigDoc, Path};
use cv_testkit::{SyntheticVault, VaultSpec};

use super::form::{self, Ctx, FormState, Kind, Row};
use super::*;
use crate::app::App;
use crate::bookmarks::{BOOKMARKS_PATH, BookmarkMsg};
use crate::daemon::ContentHash;
use crate::daemon::FakeDaemon;
use crate::message::Message;
use crate::message::{PanesMsg, TabsMsg};
use cv_schema::Node;

const ACTIONS: &str = "_custom/crucible-vault/actions.toml";
const KINDS: &str = "_custom/crucible-vault/kinds.toml";

fn fake() -> (tempfile::TempDir, Arc<FakeDaemon>) {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    SyntheticVault::build(&vault, &VaultSpec::default()).expect("vault");
    let d = FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake");
    (tmp, Arc::new(d))
}

fn app_over(d: &Arc<FakeDaemon>) -> App {
    let dc: Arc<dyn DaemonClient> = Arc::clone(d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    app
}

fn s(m: SettingsMsg) -> Message {
    Message::Settings(m)
}

fn p(s: &str) -> Path {
    Path::parse(s).expect("path")
}

/// A settings model with `file` loaded from `text` (no daemon).
fn loaded(file: &str, text: &str) -> Settings {
    let mut st = Settings::default();
    let f = st.files.entry(file.to_owned()).or_insert_with(|| {
        ConfigFile::new(
            file,
            cv_schema::schema_for(file.rsplit('/').next().unwrap_or(file)),
        )
    });
    f.apply_read(Ok((text.as_bytes().to_vec(), ContentHash::from_hex("00"))));
    st
}

fn rows_of(st: &Settings, file: &str) -> Vec<Row> {
    let f = st.file(file).expect("file");
    pages::file_rows(st, f)
}

fn doc_text(st: &Settings, file: &str) -> String {
    st.file(file)
        .and_then(|f| f.doc())
        .map(ToString::to_string)
        .unwrap_or_default()
}

const ONE_ACTION: &str = r#"# my actions
[[action]]
id = "tidy"
title = "Tidy"
mystery = 7 # unknown, kept

[[action.steps]]
type = "move"
path = "{file}"
to = "archive"
kind = "note"
"#;

#[test]
fn record_lists_are_collapsed_by_default_with_a_summary() {
    let st = loaded(ACTIONS, ONE_ACTION);
    let rows = rows_of(&st, ACTIONS);
    let items: Vec<&Row> = rows
        .iter()
        .filter(|r| matches!(r.kind, Kind::Item { .. }))
        .collect();
    assert_eq!(items.len(), 1);
    assert!(
        matches!(&items[0].kind, Kind::Item { summary, expanded: false, .. } if summary == "tidy")
    );
    assert!(
        !rows.iter().any(|r| r.path == p("action[0].id")),
        "collapsed: no field rows"
    );
}

#[test]
fn expanded_action_shows_fields_unknown_keys_read_only_and_steps() {
    let mut st = loaded(ACTIONS, ONE_ACTION);
    st.form_msg(ACTIONS, FormMsg::Toggle(p("action[0]")));
    st.form_msg(ACTIONS, FormMsg::Toggle(p("action[0].steps[0]")));
    let rows = rows_of(&st, ACTIONS);
    let id = rows
        .iter()
        .find(|r| r.path == p("action[0].id"))
        .expect("id");
    assert!(matches!(&id.kind, Kind::Text { value, required: true, .. } if value == "tidy"));
    let unk = rows
        .iter()
        .find(|r| r.path == p("action[0].mystery"))
        .expect("unknown");
    assert!(matches!(&unk.kind, Kind::ReadOnly { toml } if toml == "7"));
    let step = rows
        .iter()
        .find(|r| r.path == p("action[0].steps[0]"))
        .expect("step");
    assert!(
        matches!(&step.kind, Kind::Item { summary, .. } if summary.starts_with("Move")),
        "{step:?}"
    );
    assert!(
        rows.iter()
            .any(|r| matches!(&r.kind, Kind::Variant { tag: Some(t), .. } if t == "move"))
    );
    assert!(rows.iter().any(|r| r.path == p("action[0].steps[0].to")));
}

#[test]
fn record_list_add_move_remove_keeps_expansion_on_its_item() {
    let text = "[[action]]\nid = \"a\"\n\n[[action]]\nid = \"b\"\n";
    let mut st = loaded(ACTIONS, text);
    let list = p("action");
    st.form_msg(ACTIONS, FormMsg::Toggle(p("action[1]")));
    st.edit(
        ACTIONS,
        &DocEdit::InsertRecord(
            list.clone(),
            2,
            vec![("id".into(), ScalarValue::Str("c".into()))],
        ),
    )
    .expect("add");
    let ids = |st: &Settings| {
        let d = st.file(ACTIONS).and_then(|f| f.doc()).expect("doc");
        (0..d.len(&list))
            .map(|i| d.get_str(&list.index(i).key("id")).unwrap_or("").to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&st), ["a", "b", "c"]);
    assert!(
        st.form.is_expanded(ACTIONS, &p("action[2]")),
        "new item opens"
    );
    st.edit(ACTIONS, &DocEdit::Move(list.clone(), 1, 0))
        .expect("up");
    assert_eq!(ids(&st), ["b", "a", "c"]);
    assert!(
        st.form.is_expanded(ACTIONS, &p("action[0]")),
        "b's expansion moved"
    );
    st.edit(ACTIONS, &DocEdit::Remove(p("action[1]")))
        .expect("rm");
    assert_eq!(ids(&st), ["b", "c"]);
    assert!(
        st.form.is_expanded(ACTIONS, &p("action[1]")),
        "c shifted down"
    );
    assert!(st.file(ACTIONS).is_some_and(|f| f.dirty()));
}

#[test]
fn add_step_inserts_a_tagged_record() {
    let text = "[[action]]\nid = \"a\"\n";
    let mut st = loaded(ACTIONS, text);
    st.form_msg(ACTIONS, FormMsg::Toggle(p("action[0]")));
    let rows = rows_of(&st, ACTIONS);
    let add = rows
        .iter()
        .find(|r| matches!(&r.kind, Kind::AddItem { list, union: Some(_), .. } if *list == p("action[0].steps")))
        .expect("add step row");
    let Kind::AddItem {
        list,
        len,
        union: Some(u),
    } = &add.kind
    else {
        panic!()
    };
    st.edit(
        ACTIONS,
        &DocEdit::InsertRecord(
            list.clone(),
            *len,
            vec![(u.tag.to_owned(), ScalarValue::Str("lint".into()))],
        ),
    )
    .expect("insert");
    let d = st.file(ACTIONS).and_then(|f| f.doc()).expect("doc");
    assert_eq!(d.get_str(&p("action[0].steps[0].type")), Some("lint"));
}

#[test]
fn map_key_rename_keeps_value_comment_and_expansion() {
    let text = "[kinds.book] # books\nfolder = \"books\"\n\n[kinds.film]\nfolder = \"films\"\n";
    let mut st = loaded(KINDS, text);
    st.form_msg(KINDS, FormMsg::Toggle(p("kinds.book")));
    st.form_msg(
        KINDS,
        FormMsg::RenameDraft {
            at: p("kinds.book"),
            text: "novel".into(),
        },
    );
    st.form_msg(
        KINDS,
        FormMsg::RenameCommit {
            map: p("kinds"),
            from: "book".into(),
        },
    );
    let t = doc_text(&st, KINDS);
    assert!(t.contains("[kinds.novel] # books"), "{t}");
    assert!(t.find("novel") < t.find("film"), "position kept: {t}");
    assert!(st.form.is_expanded(KINDS, &p("kinds.novel")));
    let rows = rows_of(&st, KINDS);
    assert!(
        rows.iter()
            .any(|r| matches!(&r.kind, Kind::MapKey { key, .. } if key == "novel"))
    );
}

#[test]
fn map_add_and_remove_keys() {
    let mut st = loaded(KINDS, "[kinds.book]\nfolder = \"b\"\n");
    st.form_msg(
        KINDS,
        FormMsg::NewKeyDraft {
            map: p("kinds"),
            text: "film".into(),
        },
    );
    st.form_msg(
        KINDS,
        FormMsg::NewKeyAdd {
            map: p("kinds"),
            value: &Node::FreeTable,
        },
    );
    let d = st.file(KINDS).and_then(|f| f.doc()).expect("doc");
    assert_eq!(d.keys(&p("kinds")), ["book", "film"]);
    st.edit(KINDS, &DocEdit::Remove(p("kinds.book")))
        .expect("rm");
    let d = st.file(KINDS).and_then(|f| f.doc()).expect("doc");
    assert_eq!(d.keys(&p("kinds")), ["film"]);
    // A duplicate key is refused with a notice.
    assert!(
        st.edit(
            KINDS,
            &DocEdit::InsertKey(p("kinds"), "film".into(), &Node::FreeTable)
        )
        .is_err()
    );
    assert!(st.file(KINDS).is_some_and(|f| f.notice().is_some()));
}

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

// ---- plan round-5 WP-6: network-backed pickers ---------------------------------------------

const PIPES: &str = "_custom/crucible-vault/pipelines.toml";
const PROVIDERS: &str = "_custom/crucible-vault/providers.toml";
const AGENTS: &str = "_custom/crucible-vault/agents.toml";
const SOURCES: &str = "_custom/crucible-vault/sources.toml";

/// Loads `file` from `text` into `st` (no daemon read).
fn put(st: &mut Settings, file: &str, text: &str) {
    let f = st.files.entry(file.to_owned()).or_insert_with(|| {
        ConfigFile::new(
            file,
            cv_schema::schema_for(file.rsplit('/').next().unwrap_or(file)),
        )
    });
    f.apply_read(Ok((text.as_bytes().to_vec(), ContentHash::from_hex("00"))));
}

/// The picker the form gives the field at `at` of `file`: walks the file's schema to the field
/// and asks `pickers::for_field` with its node and reference hint (WP-R20, B13).
fn picker_of(file: &str, at: &Path) -> Option<form::Picker> {
    let name = file.rsplit('/').next().unwrap_or(file);
    let mut node = cv_schema::schema_for(name).expect("schema");
    let mut hint = None;
    for seg in &at.0 {
        match (seg, node) {
            (cv_schema::Seg::Key(k), Node::Record(r)) => {
                let f = r.field(k).expect("field");
                node = &f.node;
                hint = f.hint;
            }
            (cv_schema::Seg::Key(_), Node::Map(inner))
            | (cv_schema::Seg::Index(_), Node::List(inner)) => {
                node = inner;
                hint = None;
            }
            _ => panic!("{at}: not walkable"),
        }
    }
    pickers::for_field(node, hint)
}

/// Types `text` into the picker field `(file, at)` through the app.
fn type_in(app: &mut App, file: &str, at: &str, text: &str) {
    let path = p(at);
    let picker = picker_of(file, &path);
    assert!(picker.is_some(), "{at}: no picker");
    let _ = app.send(s(SettingsMsg::Form(
        file.to_owned(),
        FormMsg::Text {
            path,
            text: text.to_owned(),
            required: false,
            picker,
        },
    )));
}

const LOCAL_PROVIDER: &str = "[[provider]]\nid = \"local\"\nkind = \"lmstudio\"\nname = \"Local\"\n\n[[provider.models]]\nid = \"gemma-4\"\n\n[[provider.models]]\nid = \"qwen-old\"\n";

#[test]
fn currency_picker_shows_code_and_name_and_stores_the_code() {
    use crate::widget::typeahead::Key;
    let (_t, d) = fake();
    let mut app = app_over(&d);
    put(
        &mut app.settings,
        PIPES,
        "[[daily_brief.fx]]\nbase = \"USD\"\nquote = \"EUR\"\n",
    );
    type_in(&mut app, PIPES, "daily_brief.fx[0].quote", "mex");
    let vis = app.settings.ta_visible(&[]);
    assert_eq!(vis[0].label, "MXN — Mexican Peso");
    assert_eq!(vis[0].value, "MXN");
    let _ = app.send(s(SettingsMsg::Typeahead(Key::Enter)));
    assert!(doc_text(&app.settings, PIPES).contains("quote = \"MXN\""));
    assert_eq!(
        pickers::label_of(&app.settings, form::Picker::Currency, "USD").as_deref(),
        Some("USD — United States Dollar")
    );
}

#[test]
fn weather_pick_writes_label_lat_lon_in_one_edit_and_short_queries_never_geocode() {
    let (_t, d) = fake();
    let mut app = app_over(&d);
    put(
        &mut app.settings,
        PIPES,
        "[[daily_brief.weather]]\nlabel = \"Home\"\nlat = 1.0\nlon = 2.0\n",
    );
    // One character: no lookup is wanted, nothing cached.
    type_in(&mut app, PIPES, "daily_brief.weather[0].label", "p");
    assert!(app.settings.lookups.geo.is_empty());
    type_in(&mut app, PIPES, "daily_brief.weather[0].label", "par");
    assert_eq!(app.settings.lookups.geo.len(), 1);
    let vis = app.settings.ta_visible(&[]);
    assert_eq!(vis.len(), 1);
    assert_eq!(
        (vis[0].label.as_str(), vis[0].secondary.as_str()),
        ("Paris, FR", "Ile-de-France")
    );
    let before = app.settings.file(PIPES).expect("f").edit_seq();
    let _ = app.send(s(SettingsMsg::Form(
        PIPES.to_owned(),
        FormMsg::Pick {
            path: p("daily_brief.weather[0].label"),
            value: vis[0].value.clone(),
        },
    )));
    let text = doc_text(&app.settings, PIPES);
    assert!(text.contains("label = \"Paris, FR\""), "{text}");
    assert!(text.contains("lat = 48.853"), "{text}");
    assert!(text.contains("lon = 2.349"), "{text}");
    assert_eq!(app.settings.file(PIPES).expect("f").edit_seq(), before + 1);
}

#[test]
fn model_picker_merges_live_and_declared_models() {
    let (_t, d) = fake();
    let mut app = app_over(&d);
    put(&mut app.settings, PROVIDERS, LOCAL_PROVIDER);
    put(
        &mut app.settings,
        PIPES,
        "[image_describe]\nprovider = \"local\"\nmodel = \"\"\n",
    );
    type_in(&mut app, PIPES, "image_describe.model", "");
    let all = pickers::merged_models(&app.settings, Some("local"));
    let got: Vec<(&str, &str)> = all
        .iter()
        .map(|c| (c.value.as_str(), c.secondary.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("gemma-4", "live"),
            ("bge-m3", "live"),
            ("qwen-old", "declared")
        ]
    );
    assert_eq!(
        app.settings.ta_status(),
        crate::widget::typeahead::Status::Ready
    );
}

#[test]
fn model_rpc_failure_falls_back_to_declared_with_a_status_line() {
    let (_t, d) = fake();
    d.set_lookups(crate::daemon::FakeLookups {
        fail: Some("upstream down".into()),
        ..Default::default()
    })
    .expect("lookups");
    let mut app = app_over(&d);
    put(&mut app.settings, PROVIDERS, LOCAL_PROVIDER);
    put(
        &mut app.settings,
        PIPES,
        "[image_describe]\nprovider = \"local\"\nmodel = \"\"\n",
    );
    type_in(&mut app, PIPES, "image_describe.model", "");
    let vis = app.settings.ta_visible(&[]);
    let vals: Vec<&str> = vis.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(vals, vec!["gemma-4", "qwen-old"]);
    assert!(vis.iter().all(|c| c.secondary == "declared"));
    assert_eq!(
        app.settings.ta_status(),
        crate::widget::typeahead::Status::Failed(lookups::MODELS_FAILED.to_owned())
    );
}

#[test]
fn provider_model_reference_splits_and_writes_back_the_prefix() {
    use crate::widget::typeahead::Key;
    let (_t, d) = fake();
    let mut app = app_over(&d);
    put(&mut app.settings, PROVIDERS, LOCAL_PROVIDER);
    put(
        &mut app.settings,
        AGENTS,
        "[writer]\nmodel = \"local:qwen-old\"\nmax_tokens = 10\n",
    );
    assert_eq!(
        picker_of(AGENTS, &p("writer.model")),
        Some(form::Picker::ModelRef)
    );
    type_in(&mut app, AGENTS, "writer.model", "local:bge");
    let vis = app.settings.ta_visible(&[]);
    assert_eq!(vis[0].label, "bge-m3");
    assert_eq!(vis[0].value, "local:bge-m3");
    let _ = app.send(s(SettingsMsg::Typeahead(Key::Enter)));
    assert!(doc_text(&app.settings, AGENTS).contains("model = \"local:bge-m3\""));
    // `{param.…}` stays free text: no candidates, valid as typed.
    type_in(&mut app, AGENTS, "writer.model", "{param.m}");
    assert!(app.settings.ta_visible(&[]).is_empty());
    let _ = app.send(s(SettingsMsg::Typeahead(Key::Enter)));
    assert!(doc_text(&app.settings, AGENTS).contains("model = \"{param.m}\""));
}

#[test]
fn unknown_channel_ids_show_fetched_titles() {
    let (_t, d) = fake();
    let mut app = app_over(&d);
    put(
        &mut app.settings,
        SOURCES,
        "[[youtube]]\nchannel = \"Chan\"\nid = \"UC123\"\n\n[[youtube]]\nchannel = \"\"\nid = \"UCfake\"\n",
    );
    put(
        &mut app.settings,
        PIPES,
        "[ignore_shorts]\nchannels = [\"UC123\", \"UCfake\", \"UCnope\"]\n",
    );
    let _ = app.send(s(SettingsMsg::Section(Section::Pipelines)));
    let st = &app.settings;
    let chans = pickers::channels(st);
    assert_eq!(pickers::chip_label(st, &chans, "UC123"), "Chan (UC123)");
    assert_eq!(
        pickers::chip_label(st, &chans, "UCfake"),
        "Fake Channel (UCfake)"
    );
    assert_eq!(pickers::chip_label(st, &chans, "UCnope"), "UCnope");
    assert_eq!(
        pickers::source_channel_placeholder(st, SOURCES, &p("youtube[1].channel")).as_deref(),
        Some("Fake Channel")
    );
    // Never auto-written.
    assert!(doc_text(st, SOURCES).contains("channel = \"\""));
}

/// Plan round-6-feedback-todos WP-8: an `fx` item with no label summarises as `BASE → QUOTE`.
#[test]
fn fx_rows_summarise_as_base_to_quote_without_a_label() {
    let st = loaded(
        PIPES,
        "[[daily_brief.fx]]\nbase = \"USD\"\nquote = \"MXN\"\n\n[[daily_brief.fx]]\nbase = \"USD\"\nquote = \"EUR\"\nlabel = \"Euro\"\n",
    );
    let sums: Vec<String> = rows_of(&st, PIPES)
        .iter()
        .filter(|r| r.path.to_string().starts_with("daily_brief.fx["))
        .filter_map(|r| match &r.kind {
            Kind::Item { summary, .. } => Some(summary.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(sums, vec!["USD \u{2192} MXN", "Euro"]);
}

/// Plan round-6-feedback-todos WP-8: a failed model lookup is re-fetched when the field is
/// re-opened (not on every keystroke while open) and on the refresh control; models show their
/// label with the id beneath, and still filter by id.
#[test]
fn failed_model_lookup_refetches_on_reopen_and_refresh() {
    use crate::widget::typeahead::Key;
    let (_t, d) = fake();
    d.set_lookups(crate::daemon::FakeLookups {
        fail: Some("upstream down".into()),
        ..Default::default()
    })
    .expect("lookups");
    let mut app = app_over(&d);
    put(
        &mut app.settings,
        PROVIDERS,
        "[[provider]]\nid = \"local\"\nkind = \"lmstudio\"\n\n[[provider.models]]\nid = \"qwen-old\"\nlabel = \"Qwen (old)\"\n",
    );
    put(
        &mut app.settings,
        PIPES,
        "[image_describe]\nprovider = \"local\"\nmodel = \"\"\n",
    );
    let failed = || Some(&lookups::Models::Failed);
    type_in(&mut app, PIPES, "image_describe.model", "");
    assert_eq!(app.settings.lookups.models.get("local"), failed());
    let vis = app.settings.ta_visible(&[]);
    assert_eq!(
        (
            vis[0].label.as_str(),
            vis[0].secondary.as_str(),
            vis[0].value.as_str()
        ),
        ("Qwen (old)", "qwen-old · declared", "qwen-old")
    );
    // The daemon recovers; keystrokes while the list is open do not refetch.
    d.set_lookups(crate::daemon::FakeLookups::default())
        .expect("lookups");
    type_in(&mut app, PIPES, "image_describe.model", "qw");
    assert_eq!(app.settings.lookups.models.get("local"), failed());
    // Filtering still matches the id behind a label.
    assert!(
        app.settings
            .ta_visible(&[])
            .iter()
            .any(|c| c.value == "qwen-old")
    );
    // Close and re-open: refetched.
    let _ = app.send(s(SettingsMsg::Typeahead(Key::Escape)));
    type_in(&mut app, PIPES, "image_describe.model", "");
    assert!(matches!(
        app.settings.lookups.models.get("local"),
        Some(lookups::Fetch::Ok(_))
    ));
    // The refresh control refetches too (here: back to failed).
    d.set_lookups(crate::daemon::FakeLookups {
        fail: Some("down again".into()),
        ..Default::default()
    })
    .expect("lookups");
    let _ = app.send(s(SettingsMsg::Form(
        PIPES.to_owned(),
        FormMsg::RefreshModels {
            path: p("image_describe.model"),
            picker: form::Picker::Model,
        },
    )));
    assert_eq!(app.settings.lookups.models.get("local"), failed());
}

// ---- WP-R20: the Editor page is the generic schema form, with steppers ----

const EDITOR: &str = super::editor_prefs::EDITOR_PATH;

fn row_at(rows: &[Row], at: &str) -> Row {
    rows.iter()
        .find(|r| r.path == p(at))
        .cloned()
        .unwrap_or_else(|| panic!("{at}: no row"))
}

fn step_of(rows: &[Row], at: &str) -> form::Step {
    match row_at(rows, at).kind {
        Kind::Number { step: Some(s), .. } => s,
        k => panic!("{at}: {k:?}"),
    }
}

/// GUI reachability (SR12): every `editor_schema()` field — nested records included — has a
/// form row on the Editor page, so `scroll_anchor` and `open_url_cmd` are editable.
#[test]
fn every_editor_schema_field_yields_a_form_row() {
    fn walk(r: &cv_schema::Record, at: &Path, out: &mut Vec<Path>) {
        for f in r.fields {
            let fp = at.key(f.key);
            if let Node::Record(sub) = f.node {
                walk(sub, &fp, out);
            }
            out.push(fp);
        }
    }
    let Node::Record(r) = cv_schema::editor_schema() else {
        panic!("record")
    };
    let mut want = Vec::new();
    walk(r, &Path::root(), &mut want);
    assert!(want.len() >= 15, "{}", want.len());
    let st = loaded(EDITOR, "");
    let rows = rows_of(&st, EDITOR);
    for w in want {
        assert!(rows.iter().any(|r| r.path == w), "{w}: no row");
    }
    assert!(matches!(
        row_at(&rows, "scroll_anchor").kind,
        Kind::Number { step: Some(_), .. }
    ));
    assert!(matches!(
        row_at(&rows, "open_url_cmd").kind,
        Kind::Chips { .. }
    ));
}

/// Decision 3: bounded numbers step from the stored value (else the default) by the schema's
/// step, as the old Editor page did (font ±1, UI scale ±0.25, limit ±10, debounce ±50).
#[test]
fn bounded_numbers_get_steppers_with_the_old_increments() {
    let st = loaded(EDITOR, "ui_scale = 2.9\n[search]\nlimit = 995\n");
    let rows = rows_of(&st, EDITOR);
    let f = step_of(&rows, "font_size");
    assert_eq!(
        (f.dec, f.inc),
        (
            Some(ScalarValue::Float(21.0)),
            Some(ScalarValue::Float(23.0))
        ),
        "absent: from the default 22"
    );
    let u = step_of(&rows, "ui_scale");
    assert_eq!(
        (u.dec, u.inc),
        (Some(ScalarValue::Float(2.65)), None),
        "3.15 is past the bound"
    );
    let l = step_of(&rows, "search.limit");
    assert_eq!(
        (l.dec, l.inc),
        (Some(ScalarValue::Int(985)), Some(ScalarValue::Int(1000))),
        "integers clamp"
    );
    let d = step_of(&rows, "search.debounce_ms");
    assert_eq!(
        (d.dec, d.inc),
        (Some(ScalarValue::Int(200)), Some(ScalarValue::Int(300)))
    );
    let a = step_of(&rows, "scroll_anchor");
    assert_eq!((a.dec, a.inc), (None, Some(ScalarValue::Int(5))));
    // Every page gains it: a bounded field elsewhere steps too; an unbounded one does not.
    let st = loaded(
        PIPES,
        "[[daily_brief.weather]]\nlabel = \"Home\"\nlat = 1.0\nlon = 2.0\n",
    );
    let mut st = st;
    st.form_msg(PIPES, FormMsg::Toggle(p("daily_brief.weather[0]")));
    let rows = rows_of(&st, PIPES);
    assert!(step_of(&rows, "daily_brief.weather[0].lat").inc.is_some());
    assert!(matches!(
        row_at(&rows, "image_describe.max_tokens").kind,
        Kind::Number { step: Some(_), .. }
    ));
    // An unbounded number (a view's `limit`) gets no stepper.
    let mut st = Settings::default();
    let mut f = ConfigFile::new(
        "_custom/crucible-vault/views/v.toml",
        Some(cv_schema::view_schema()),
    );
    f.apply_read(Ok((
        b"[[views]]\ntype = \"table\"\nname = \"T\"\nlimit = 3\n".to_vec(),
        ContentHash::from_hex("00"),
    )));
    st.files.insert(f.path().to_owned(), f);
    st.form_msg(
        "_custom/crucible-vault/views/v.toml",
        FormMsg::Toggle(p("views[0]")),
    );
    let rows = rows_of(&st, "_custom/crucible-vault/views/v.toml");
    assert!(matches!(
        row_at(&rows, "views[0].limit").kind,
        Kind::Number { step: None, .. }
    ));
}

/// The on-disk contract: generic-form edits of every `editor.toml` field write the bytes the
/// hand page wrote (same keys, value types, list shape and enum spellings; comments kept).
#[test]
fn editor_page_edits_write_the_hand_page_bytes() {
    let mut st = loaded(EDITOR, "# my prefs\nfont_size = 22 # base\n");
    let step = |st: &Settings, at: &str, inc: bool| {
        let s = step_of(&rows_of(st, EDITOR), at);
        if inc { s.inc } else { s.dec }.expect("enabled")
    };
    let v = step(&st, "font_size", true);
    st.form_msg(
        EDITOR,
        FormMsg::Step {
            path: p("font_size"),
            value: v,
        },
    );
    let v = step(&st, "ui_scale", true);
    st.form_msg(
        EDITOR,
        FormMsg::Step {
            path: p("ui_scale"),
            value: v,
        },
    );
    let v = step(&st, "search.limit", false);
    st.form_msg(
        EDITOR,
        FormMsg::Step {
            path: p("search.limit"),
            value: v,
        },
    );
    let v = step(&st, "search.debounce_ms", true);
    st.form_msg(
        EDITOR,
        FormMsg::Step {
            path: p("search.debounce_ms"),
            value: v,
        },
    );
    for (k, v) in [
        ("surround", "dark"),
        ("tab_title", "path"),
        ("quick_switch_open", "switch"),
        ("delete_mode", "permanent"),
        ("missing_link", "create"),
    ] {
        // The seg-group writes the choice string (what `Kind::Enum` offers).
        assert!(matches!(
            row_at(&rows_of(&st, EDITOR), k).kind,
            Kind::Enum { choices, .. } if choices.contains(&v)
        ));
        st.edit(EDITOR, &DocEdit::Set(p(k), ScalarValue::Str(v.into())))
            .expect("enum");
    }
    st.edit(
        EDITOR,
        &DocEdit::Set(p("focus_follows_mouse"), ScalarValue::Bool(true)),
    )
    .expect("toggle");
    for id in ["a", "b"] {
        st.form_msg(
            EDITOR,
            FormMsg::ChipAdd {
                path: p("palette.hidden"),
                value: Some(id.into()),
            },
        );
    }
    assert_eq!(
        doc_text(&st, EDITOR),
        "# my prefs\nfont_size = 23.0 # base\nui_scale = 1.25\nsurround = \"dark\"\ntab_title = \"path\"\nquick_switch_open = \"switch\"\ndelete_mode = \"permanent\"\nmissing_link = \"create\"\nfocus_follows_mouse = true\n\n[search]\nlimit = 40\ndebounce_ms = 300\n\n[palette]\nhidden = [\"a\", \"b\"]\n"
    );
    // The last chip removed leaves `hidden = []`, as the emptied text field did.
    for _ in 0..2 {
        st.form_msg(
            EDITOR,
            FormMsg::ChipRemove {
                path: p("palette.hidden"),
                index: 0,
            },
        );
    }
    assert!(doc_text(&st, EDITOR).contains("hidden = []"));
}

/// A stepper press through the app applies the pref live and drops a stale typed draft.
#[test]
fn editor_stepper_applies_live_and_clears_the_draft() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d);
    let _ = app.send(s(SettingsMsg::Form(
        EDITOR.into(),
        FormMsg::Number {
            path: p("font_size"),
            text: "3x".into(),
            float: true,
        },
    )));
    let _ = app.send(s(SettingsMsg::Form(
        EDITOR.into(),
        FormMsg::Step {
            path: p("font_size"),
            value: ScalarValue::Float(30.0),
        },
    )));
    assert!((app.prefs.font_size - 30.0).abs() < f32::EPSILON);
    let rows = rows_of(&app.settings, EDITOR);
    assert!(matches!(
        row_at(&rows, "font_size").kind,
        Kind::Number { text, .. } if text == "30.0"
    ));
    // The surround seg mirrors into the local UiState copy.
    let _ = app.send(s(SettingsMsg::Edit(
        EDITOR.into(),
        DocEdit::Set(p("surround"), ScalarValue::Str("light".into())),
    )));
    assert_eq!(app.prefs.surround, crate::theme::Surround::Light);
    assert_eq!(app.ui.surround, crate::theme::Surround::Light);
}

/// SM11: one table row per section, every file under the one config dir and named in
/// `cv_schema::FILES`.
#[test]
fn section_table_rows_match_their_sections_and_files() {
    for (i, sec) in Section::ALL.into_iter().enumerate() {
        assert_eq!(SECTIONS[i].section, sec);
        if let Some(f) = sec.file() {
            assert!(f.starts_with(CONFIG_DIR), "{f}");
            let name = sec.file_name().expect("name");
            assert!(cv_schema::FILES.contains(&name), "{name}");
            assert_eq!(Section::for_file_name(name), Some(sec));
        }
    }
    assert_eq!(crate::palette::CONFIG_PREFIX, CONFIG_DIR);
    assert!(VIEWS_DIR.starts_with(CONFIG_DIR));
}
