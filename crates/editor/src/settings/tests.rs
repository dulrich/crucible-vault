//! plan editor-settings-tab WP-2 tests: the save loop, `editor.toml`, routing, the Settings tab
//! kind, and the editor-side schema drift test. Synthetic vaults + `FakeDaemon` only.

use std::sync::Arc;

use cv_protocol::{ActionInfo, ChangeKind, Notification, Origin};
use cv_schema::{ConfigDoc, Path};
use cv_testkit::{SyntheticVault, VaultSpec};

use super::editor_prefs::{self, EDITOR_PATH, Prefs};
use super::*;
use crate::app::{App, TabKind};
use crate::bookmarks::{BOOKMARKS_PATH, BookmarkMsg};
use crate::daemon::FakeDaemon;
use crate::daemon::{ContentHash, DaemonError};
use crate::message::Message;
use crate::message::{ExplorerMsg, PaletteMsg, PanesMsg, StatusMsg, TabsMsg};
use crate::theme::Surround;

fn fake() -> (tempfile::TempDir, Arc<FakeDaemon>) {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    SyntheticVault::build(&vault, &VaultSpec::default()).expect("vault");
    let d = FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake");
    (tmp, Arc::new(d))
}

fn app_over(d: &Arc<FakeDaemon>, state: Option<std::path::PathBuf>) -> App {
    let dc: Arc<dyn DaemonClient> = Arc::clone(d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), state);
    app.queue_rpcs();
    app
}

fn s(m: SettingsMsg) -> Message {
    Message::Settings(m)
}

/// An Editor-page edit through the generic form path.
fn pref(key: &str, v: ScalarValue) -> Message {
    s(SettingsMsg::Edit(
        EDITOR_PATH.into(),
        DocEdit::Set(Path::parse(key).expect("path"), v),
    ))
}

fn font(v: f64) -> Message {
    pref("font_size", ScalarValue::Float(v))
}

fn changed(app: &mut App, path: &str, hash: Option<String>) {
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            path: path.to_owned(),
            kind: ChangeKind::Modified,
            hash,
            origin: Origin::Daemon,
            from: None,
            replaced_by: None,
        },
    )));
}

fn settings_tabs(app: &App) -> usize {
    app.panes
        .iter()
        .flat_map(|(_, ps)| ps.tabs.iter())
        .filter(|t| t.kind == TabKind::Settings)
        .count()
}

fn config_dir_entries(d: &FakeDaemon) -> Vec<String> {
    let dir = d.vault_root().join(CONFIG_DIR);
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|r| {
            r.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

// ---- pure: routing, save loop, prefs ----

#[test]
fn reload_errors_attribute_by_file_name() {
    assert_eq!(
        attribute("actions.toml: action #2: unknown field `x`"),
        Some(Section::Actions)
    );
    assert_eq!(
        attribute("action \"daily\" disarmed: step 1 (move): bad"),
        Some(Section::Actions)
    );
    assert_eq!(
        attribute("schedules.toml: schedule #1 names unknown action \"x\""),
        Some(Section::Schedules)
    );
    assert_eq!(
        attribute("providers.toml: expected a table (line 3)"),
        Some(Section::Providers)
    );
    assert_eq!(attribute("pipelines.toml: bad"), Some(Section::Pipelines));
    assert_eq!(attribute("sources.toml: bad"), Some(Section::Sources));
    assert_eq!(attribute("captures.toml: bad"), Some(Section::Captures));
    assert_eq!(attribute("agents.toml: bad"), Some(Section::Agents));
    assert_eq!(attribute("kinds.toml: bad"), Some(Section::Kinds));
    assert_eq!(attribute("views/a.toml: nope"), None);
    assert_eq!(attribute("watcher: lagging"), None);
    assert_eq!(attribute("no colon at all"), None);
}

#[test]
fn route_reload_errors_fills_pages_and_status() {
    let mut st = Settings::default();
    st.route_reload_errors(&[
        "actions.toml: broken".into(),
        "action \"a\" disarmed: x".into(),
        "kinds.toml: y".into(),
        "something odd".into(),
    ]);
    let actions = st.file(Section::Actions.file().expect("p")).expect("f");
    assert_eq!(actions.reload_errors().len(), 2);
    assert_eq!(
        st.file(Section::Kinds.file().expect("p"))
            .expect("f")
            .reload_errors(),
        ["kinds.toml: y"]
    );
    assert_eq!(st.status.unrouted, ["something odd"]);
    // The next batch replaces the previous one.
    st.route_reload_errors(&[]);
    assert!(st.files.values().all(|f| f.reload_errors().is_empty()));
    assert!(st.status.unrouted.is_empty());
}

#[test]
fn every_section_is_in_the_rail_with_its_file() {
    let labels: Vec<&str> = Section::ALL.iter().map(|s| s.label()).collect();
    assert_eq!(
        labels,
        [
            "Editor",
            "Vault",
            "Hotkeys",
            "Actions",
            "Schedules",
            "Pipelines",
            "Sources",
            "Captures & periods",
            "Kinds",
            "Agents",
            "Providers",
            "Views",
            "Bookmarks",
            "Status"
        ]
    );
    let st = Settings::default();
    for sec in Section::ALL {
        if let Some(p) = sec.file() {
            assert!(p.starts_with(CONFIG_DIR), "{p}");
            let f = st.file(p).expect("loop for every file page");
            assert!(f.schema().is_some(), "{p} has a schema");
        }
    }
    assert_eq!(st.files.len(), 11);
}

#[test]
fn malformed_file_is_not_editable_until_started_over() {
    let mut f = ConfigFile::new(EDITOR_PATH, Some(cv_schema::editor_schema()));
    let bytes = b"font_size = [unclosed".to_vec();
    let h = ContentHash::of(&bytes);
    f.apply_read(Ok((bytes, h.clone())));
    assert!(matches!(f.state(), LoadState::Malformed(_)));
    assert!(f.doc().is_none() && !f.can_save());
    assert!(
        f.edit(|d| d.set(&Path::root().key("font_size"), 20.0))
            .is_err()
    );
    assert!(f.begin_save().is_none(), "never overwritten without a Save");
    assert_eq!(
        editor_prefs::effective(&f, Surround::Med),
        Prefs::default(),
        "malformed → defaults"
    );
    f.start_over();
    assert!(f.can_save());
    let job = f.begin_save().expect("job");
    assert_eq!(job.base, Some(h), "CAS on the malformed content");
}

#[test]
fn form_errors_block_save() {
    let mut f = ConfigFile::new(EDITOR_PATH, Some(cv_schema::editor_schema()));
    f.apply_read(Err(DaemonError::NotFound(EDITOR_PATH.into())));
    assert_eq!(*f.state(), LoadState::Absent);
    assert!(f.base().is_none());
    f.edit(|d| d.set(&Path::root().key("font_size"), 200.0))
        .expect("edit");
    assert!(f.dirty() && !f.errors().is_empty() && !f.can_save());
    f.edit(|d| d.set(&Path::root().key("font_size"), 20.0))
        .expect("edit");
    assert!(f.errors().is_empty() && f.can_save());
}

#[test]
fn prefs_defaults_fields_and_fallbacks() {
    let empty = ConfigDoc::new();
    assert_eq!(
        editor_prefs::from_doc(&empty, Surround::Med),
        Prefs::default()
    );
    assert_eq!(
        editor_prefs::from_doc(&empty, Surround::Dark).surround,
        Surround::Dark,
        "no surround → the UiState fallback"
    );
    let full = editor_prefs::parse(EDITOR_SAMPLE, Surround::Med).expect("parse");
    assert!((full.font_size - 18.0).abs() < f32::EPSILON);
    assert!((full.ui_scale - 1.5).abs() < f32::EPSILON);
    assert_eq!(full.surround, Surround::Light);
    assert_eq!(full.tab_title, crate::header::TabTitle::Unambiguous);
    assert_eq!(
        full.quick_switch_open,
        crate::header::QuickSwitchOpen::Duplicate
    );
    assert_eq!(full.search_limit, 80);
    assert_eq!(full.search_debounce_ms, 120);
    assert_eq!(full.scroll_anchor, 40);
    assert_eq!(full.nav_links.len(), 1);
    assert_eq!(full.delete_mode, crate::daemon::DeleteMode::Permanent);
    assert_eq!(
        full.missing_link,
        crate::settings::editor_prefs::MissingLink::Create
    );
    assert_eq!(legacy_missing(), editor_prefs::MissingLink::Warn);
    // plan editor-ui-round-3 WP-3: a legacy `confirm_trash` key is tolerated and ignored.
    let legacy = editor_prefs::parse("confirm_trash = true\n", Surround::Med).expect("parse");
    assert_eq!(legacy.delete_mode, crate::daemon::DeleteMode::Trash);
    assert_eq!(full.palette_hidden, ["quit", "my.action"]);
    assert_eq!(full.keys.get("quick_switcher").map(Vec::len), Some(2));
    assert_eq!(full.keys.get("quit").map(Vec::len), Some(0), "[] unbinds");
    // Out of range / mistyped → that field's default.
    let bad = editor_prefs::parse(
        "font_size = 2\nui_scale = 9.0\nsurround = \"neon\"\ntab_title = \"stem\"\nquick_switch_open = \"focus\"\n[search]\nlimit = \"x\"\n",
        Surround::Med,
    )
    .expect("parse");
    assert_eq!(bad, Prefs::default());
    // An integer font size is accepted (Float accepts int input).
    let int = editor_prefs::parse("font_size = 30\n", Surround::Med).expect("parse");
    assert!((int.font_size - 30.0).abs() < f32::EPSILON);
    assert_eq!(int.tab_title, crate::header::TabTitle::Title, "default");
    let mut doc = ConfigDoc::parse("").expect("doc");
    DocEdit::Set(
        Path::root().key("tab_title"),
        ScalarValue::Str("path".into()),
    )
    .apply(&mut doc)
    .expect("set");
    assert!(doc.to_string().contains("tab_title = \"path\""));
    assert_eq!(
        int.quick_switch_open,
        crate::header::QuickSwitchOpen::Move,
        "default"
    );
    DocEdit::Set(
        Path::root().key("quick_switch_open"),
        ScalarValue::Str("switch".into()),
    )
    .apply(&mut doc)
    .expect("set");
    assert!(doc.to_string().contains("quick_switch_open = \"switch\""));
}

#[test]
fn pref_edits_keep_comments() {
    let mut doc = ConfigDoc::parse("# my prefs\nfont_size = 22 # base\n").expect("doc");
    DocEdit::Set(Path::root().key("font_size"), ScalarValue::Float(24.0))
        .apply(&mut doc)
        .expect("set");
    DocEdit::Set(
        Path::parse("palette.hidden").expect("p"),
        ScalarValue::StrList(vec!["a".into(), "b".into()]),
    )
    .apply(&mut doc)
    .expect("set");
    let out = doc.to_string();
    assert!(
        out.contains("# my prefs") && out.contains("# base"),
        "{out}"
    );
    assert_eq!(
        doc.get_str_list(&Path::parse("palette.hidden").expect("p")),
        Some(vec!["a".to_owned(), "b".to_owned()])
    );
    assert_eq!(doc.get_float(&Path::root().key("font_size")), Some(24.0));
}

#[test]
fn reading_sizes_scale_with_font_size() {
    use crate::reading::view::{block_size, scaled};
    // plan editor-ui-round-3 WP-6: a paragraph renders at exactly `font_size`, for two sizes,
    // and the H1 : paragraph ratio is the design table's (28 : 16) at both.
    for font_size in [22.0_f32, 33.0] {
        let p = Prefs {
            font_size,
            ..Prefs::default()
        };
        let para = scaled(block_size(None), p.text_scale());
        let h1 = scaled(block_size(Some(1)), p.text_scale());
        assert!((para - font_size).abs() < 1e-4, "{font_size}: {para}");
        assert!((h1 / para - 28.0 / 16.0).abs() < 1e-4);
    }
    let big = Prefs {
        font_size: 33.0,
        ..Prefs::default()
    };
    assert!((scaled(block_size(Some(2)), big.text_scale()) - 49.5).abs() < 1e-4);
}

#[test]
fn hidden_ids_leave_the_palette_list() {
    use crate::palette::{ClientCmd, EntryId, build_entries, visible_entries};
    let action = ActionInfo {
        id: "my.action".into(),
        title: "Mine".into(),
        group: None,
        keys: vec![],
        params: vec![],
        needs: vec![],
        durable: false,
    };
    let all = build_entries(std::slice::from_ref(&action));
    let shown = visible_entries(all.clone(), &["quit".into(), "my.action".into()]);
    assert_eq!(shown.len() + 2, all.len());
    assert!(
        !shown
            .iter()
            .any(|e| e.id == EntryId::Client(ClientCmd::Quit))
    );
    assert!(
        !shown
            .iter()
            .any(|e| e.id == EntryId::Action("my.action".into()))
    );
    assert!(
        all.iter()
            .any(|e| e.id == EntryId::Client(ClientCmd::OpenSettings) && e.keys == ["Ctrl+,"]),
        "Open settings is a palette entry with its chord"
    );
}

// ---- editor-side schema drift (WP-1 gap): schema ⇔ the editor's real parsers ----

const EDITOR_SAMPLE: &str = r#"# editor prefs
font_size = 18.0
ui_scale = 1.5
surround = "light"
tab_title = "unambiguous"
quick_switch_open = "duplicate"
confirm_trash = true
delete_mode = "permanent"
missing_link = "create"
focus_follows_mouse = true
menu_bar = "hidden"
auto_lint = false
scroll_anchor = 40
open_url_cmd = ["firefox", "--new-tab"]
nav_open = "replace"

[[nav_links]]
label = "Inbox"
path = "core/Inbox.md"

[search]
limit = 80
debounce_ms = 120
location = "modal"

[palette]
hidden = ["quit", "my.action"]

[keys]
quick_switcher = ["Ctrl+P", "Ctrl+Alt+P"]
quit = []
"#;

/// Plan round-11 WP-7: `nav_links` absent → the defaults, `[]` → none, a malformed entry is
/// skipped.
#[test]
fn nav_links_pref_default_empty_and_malformed() {
    let defaults = editor_model::period::default_nav_links();
    assert_eq!(editor_prefs::Prefs::default().nav_links, defaults);
    let p = editor_prefs::parse("", Surround::Med).expect("parse");
    assert_eq!(p.nav_links, defaults);
    let p = editor_prefs::parse("nav_links = []\n", Surround::Med).expect("parse");
    assert!(p.nav_links.is_empty());
    let p = editor_prefs::parse(
        "[[nav_links]]\nlabel = \"Inbox\"\npath = \"core/Inbox.md\"\n\n[[nav_links]]\nlabel = \"No path\"\n\n[[nav_links]]\npath = \"x.md\"\n",
        Surround::Med,
    )
    .expect("parse");
    assert_eq!(p.nav_links.len(), 1);
    assert_eq!(p.nav_links[0].label, "Inbox");
    assert_eq!(p.nav_links[0].path, "core/Inbox.md");
}

/// Plan round-6 WP-4: `missing_link` absent → `warn`.
fn legacy_missing() -> editor_prefs::MissingLink {
    editor_prefs::parse("", Surround::Med)
        .expect("parse")
        .missing_link
}

const BOOKMARKS_SAMPLE: &str = r#"[[bookmark]]
name = "Home"
path = "Home.md"

[[bookmark]]
name = "Inbox"
path = "inbox/Inbox.md"
keys = ["Alt+I"]
"#;

#[test]
fn editor_and_bookmarks_schemas_match_the_editor_parsers() {
    for (name, sample) in [
        ("editor.toml", EDITOR_SAMPLE),
        ("bookmarks.toml", BOOKMARKS_SAMPLE),
    ] {
        let schema = cv_schema::schema_for(name).expect("schema");
        let doc = ConfigDoc::parse(sample).expect("toml");
        assert!(
            cv_schema::validate(&doc, schema).is_empty(),
            "{name}: {:?}",
            cv_schema::validate(&doc, schema)
        );
        let need = cv_schema::schema_paths(schema);
        let have = cv_schema::covered_paths(&doc, schema);
        let missing: Vec<_> = need.difference(&have).collect();
        assert!(missing.is_empty(), "{name} sample misses {missing:?}");
    }
    // The real parsers take every field.
    let p = editor_prefs::parse(EDITOR_SAMPLE, Surround::Med).expect("editor parse");
    assert_ne!(p, Prefs::default(), "every field read");
    assert!(!p.auto_lint, "auto_lint is read (default true)");
    let b = crate::bookmarks::parse(BOOKMARKS_SAMPLE.as_bytes()).expect("bookmarks parse");
    assert_eq!(b.len(), 2);
    // Round trip: the bookmarks renderer's output validates against the schema.
    let rendered = crate::bookmarks::render(&b).expect("render");
    let doc = ConfigDoc::parse(&rendered).expect("toml");
    assert!(
        cv_schema::validate(&doc, cv_schema::schema_for("bookmarks.toml").expect("s")).is_empty()
    );
    // Negative self-check: a field the schema rejects.
    let bad = ConfigDoc::parse("font_size = \"big\"\n").expect("toml");
    assert!(!cv_schema::validate(&bad, cv_schema::editor_schema()).is_empty());
}

// ---- app: the Settings tab kind ----

#[test]
fn ctrl_comma_opens_settings_once_and_focuses_it() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let comma = crate::palette::chord::parse("Ctrl+,").expect("chord");
    assert!(
        crate::palette::Keymap::default().routed(&comma),
        "passes through the source editor"
    );
    let _ = app.send(Message::Palette(PaletteMsg::Key(comma.clone())));
    assert_eq!(settings_tabs(&app), 1);
    assert!(
        app.active_tab()
            .is_some_and(|t| t.kind == crate::app::TabKind::Settings)
    );
    assert!(app.active_note().is_none());
    assert!(
        app.status.error_text().is_none(),
        "no daemon read for the Settings tab"
    );
    // Open a note, then Ctrl+, again: the existing tab is focused, never a second one.
    let note = d
        .index()
        .with_query(|q| q.folder_files("notes"))
        .expect("q")
        .into_iter()
        .find(|f| f.is_md)
        .expect("note")
        .path;
    let _ = app.send(Message::Tabs(TabsMsg::Open(note.clone())));
    let _ = app.send(Message::Palette(PaletteMsg::Key(comma)));
    assert_eq!(settings_tabs(&app), 1);
    assert!(
        app.active_tab()
            .is_some_and(|t| t.kind == crate::app::TabKind::Settings)
    );
    // Never in history or recent; files loaded for the pages.
    let ps = app.panes.get(app.focus).expect("pane");
    assert!(!ps.history.can_forward());
    assert!(app.ui.recent.iter().all(|p| *p == note), "only the note");
    assert!(
        app.settings
            .files
            .values()
            .all(|f| *f.state() != LoadState::NotLoaded)
    );
    // Ctrl+Shift+, stays reserved and inert.
    let shift = crate::palette::chord::parse("Ctrl+Shift+,").expect("chord");
    assert!(crate::palette::chord::reserved(&shift));
    assert_eq!(crate::palette::Keymap::default().get(&shift), None);
}

#[test]
fn settings_tab_persists_and_old_ui_state_loads() {
    let (tmp, d) = fake();
    let state = tmp.path().join("state/editor.json");
    {
        let mut app = app_over(&d, Some(state.clone()));
        let _ = app.send(s(SettingsMsg::Open));
        let _ = app.send(Message::PersistDue);
        let leaves = app.ui.panes.as_ref().map(|l| l.root.leaves()[0].0.to_vec());
        assert_eq!(leaves, Some(vec![crate::app::TabKind::Settings]));
        assert!(app.ui.modes.is_empty(), "no mode entry");
    }
    let app = app_over(&d, Some(state.clone()));
    assert_eq!(settings_tabs(&app), 1, "restored without a daemon read");
    assert!(app.status.error_text().is_none());
    // A pre-settings file (plain vault paths) still loads.
    std::fs::write(
        &state,
        br#"{"tabs":["missing.md"],"active_tab":0,"surround":"dark","recent":[],"expanded":[]}"#,
    )
    .expect("old");
    let app = app_over(&d, Some(state));
    assert_eq!(settings_tabs(&app), 0);
    assert_eq!(app.prefs.surround, Surround::Dark);
}

#[test]
fn settings_tab_takes_no_note_commands() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let _ = app.send(s(SettingsMsg::Open));
    let _ = app.send(Message::Source(crate::message::SourceMsg::Save));
    let _ = app.send(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::AddActive)));
    let _ = app.send(Message::Find(crate::message::FindMsg::Open));
    assert!(
        app.bookmarks.items.is_empty(),
        "the Settings tab is never bookmarked"
    );
    assert!(app.panes.get(app.focus).is_some_and(|ps| ps.find.is_none()));
    assert!(d.read_file(BOOKMARKS_PATH).is_err());
    // Closing it is a plain removal.
    let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
    assert_eq!(settings_tabs(&app), 0);
}

#[path = "tests_autosave.rs"]
mod autosave;

#[path = "tests_app_editor_toml.rs"]
mod app_editor_toml;
