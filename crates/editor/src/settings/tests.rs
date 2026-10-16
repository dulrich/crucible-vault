//! plan editor-settings-tab WP-2 tests: the save loop, `editor.toml`, routing, the Settings tab
//! kind, and the editor-side schema drift test. Synthetic vaults + `FakeDaemon` only.

use std::sync::Arc;

use cv_protocol::{ActionInfo, ChangeKind, Notification, Origin};
use cv_schema::{ConfigDoc, Path};
use cv_testkit::{SyntheticVault, VaultSpec};

use super::editor_prefs::{self, EDITOR_PATH, PrefEdit, Prefs};
use super::*;
use crate::app::{App, TabKind};
use crate::bookmarks::{BOOKMARKS_PATH, BookmarkMsg};
use crate::daemon::FakeDaemon;
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
    app.rpc_inline = true;
    app
}

fn s(m: SettingsMsg) -> Message {
    Message::Settings(m)
}

fn changed(app: &mut App, path: &str, hash: Option<String>) {
    let _ = app.update(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            path: path.to_owned(),
            kind: ChangeKind::Modified,
            hash,
            origin: Origin::Daemon,
        },
    )));
}

fn settings_tabs(app: &App) -> usize {
    app.panes
        .iter()
        .flat_map(|(_, ps)| ps.tabs.iter())
        .filter(|t| t.kind() == TabKind::Settings)
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
    assert_eq!(actions.reload_errors.len(), 2);
    assert_eq!(
        st.file(Section::Kinds.file().expect("p"))
            .expect("f")
            .reload_errors,
        ["kinds.toml: y"]
    );
    assert_eq!(st.status.unrouted, ["something odd"]);
    // The next batch replaces the previous one.
    st.route_reload_errors(&[]);
    assert!(st.files.values().all(|f| f.reload_errors.is_empty()));
    assert!(st.status.unrouted.is_empty());
}

#[test]
fn every_section_is_in_the_rail_with_its_file() {
    let labels: Vec<&str> = Section::ALL.iter().map(|s| s.label()).collect();
    assert_eq!(
        labels,
        [
            "Editor",
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
            assert!(f.schema.is_some(), "{p} has a schema");
        }
    }
    assert_eq!(st.files.len(), 10);
}

#[test]
fn malformed_file_is_not_editable_until_started_over() {
    let mut f = ConfigFile::new(EDITOR_PATH, Some(cv_schema::editor_schema()));
    let bytes = b"font_size = [unclosed".to_vec();
    let h = ContentHash::of(&bytes);
    f.apply_read(Ok((bytes, h.clone())));
    assert!(matches!(f.state, LoadState::Malformed(_)));
    assert!(f.doc.is_none() && !f.can_save());
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
    assert_eq!(f.state, LoadState::Absent);
    assert!(f.base.is_none());
    f.edit(|d| d.set(&Path::root().key("font_size"), 200.0))
        .expect("edit");
    assert!(f.dirty && !f.errors.is_empty() && !f.can_save());
    f.edit(|d| d.set(&Path::root().key("font_size"), 20.0))
        .expect("edit");
    assert!(f.errors.is_empty() && f.can_save());
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
    assert_eq!(full.delete_mode, crate::daemon::DeleteMode::Permanent);
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
    editor_prefs::apply(&mut doc, &PrefEdit::TabTitle(crate::header::TabTitle::Path)).expect("set");
    assert!(doc.to_string().contains("tab_title = \"path\""));
    assert_eq!(
        int.quick_switch_open,
        crate::header::QuickSwitchOpen::Move,
        "default"
    );
    editor_prefs::apply(
        &mut doc,
        &PrefEdit::QuickSwitchOpen(crate::header::QuickSwitchOpen::Switch),
    )
    .expect("set");
    assert!(doc.to_string().contains("quick_switch_open = \"switch\""));
}

#[test]
fn pref_edits_keep_comments() {
    let mut doc = ConfigDoc::parse("# my prefs\nfont_size = 22 # base\n").expect("doc");
    editor_prefs::apply(&mut doc, &PrefEdit::FontSize(24.0)).expect("set");
    editor_prefs::apply(&mut doc, &PrefEdit::PaletteHidden("a, b ,".into())).expect("set");
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

[search]
limit = 80
debounce_ms = 120

[palette]
hidden = ["quit", "my.action"]

[keys]
quick_switcher = ["Ctrl+P", "Ctrl+Alt+P"]
quit = []
"#;

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
        crate::palette::routed(&comma),
        "passes through the source editor"
    );
    let _ = app.update(Message::Palette(PaletteMsg::Key(comma.clone())));
    assert_eq!(settings_tabs(&app), 1);
    assert!(app.active_tab().is_some_and(|t| t.is_settings()));
    assert!(app.active_note().is_none());
    assert!(app.status.error.is_none(), "no daemon read of the sentinel");
    // Open a note, then Ctrl+, again: the existing tab is focused, never a second one.
    let note = d
        .index()
        .with_query(|q| q.folder_files("notes"))
        .expect("q")
        .into_iter()
        .find(|f| f.is_md)
        .expect("note")
        .path;
    let _ = app.update(Message::Tabs(TabsMsg::Open(note.clone())));
    let _ = app.update(Message::Palette(PaletteMsg::Key(comma)));
    assert_eq!(settings_tabs(&app), 1);
    assert!(app.active_tab().is_some_and(|t| t.is_settings()));
    // Never in history or recent; files loaded for the pages.
    let ps = app.panes.get(app.focus).expect("pane");
    assert!(!ps.history.can_forward());
    assert!(!app.ui.recent.iter().any(|p| p == SETTINGS_TAB));
    assert!(
        app.settings
            .files
            .values()
            .all(|f| f.state != LoadState::NotLoaded)
    );
    // Ctrl+Shift+, stays reserved and inert.
    let shift = crate::palette::chord::parse("Ctrl+Shift+,").expect("chord");
    assert!(crate::palette::chord::reserved(&shift));
    assert_eq!(crate::palette::client_chord(&shift), None);
}

#[test]
fn settings_tab_persists_and_old_ui_state_loads() {
    let (tmp, d) = fake();
    let state = tmp.path().join("state/editor.json");
    {
        let mut app = app_over(&d, Some(state.clone()));
        let _ = app.update(s(SettingsMsg::Open));
        assert_eq!(app.ui.tabs, [SETTINGS_TAB]);
        assert!(!app.ui.modes.contains_key(SETTINGS_TAB), "no mode entry");
    }
    let app = app_over(&d, Some(state.clone()));
    assert_eq!(settings_tabs(&app), 1, "restored without a daemon read");
    assert!(app.status.error.is_none());
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
    let _ = app.update(s(SettingsMsg::Open));
    let _ = app.update(Message::Source(crate::message::SourceMsg::Save));
    let _ = app.update(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::AddActive)));
    let _ = app.update(Message::Find(crate::message::FindMsg::Open));
    assert!(
        app.bookmarks.items.is_empty(),
        "the sentinel is never bookmarked"
    );
    assert!(app.panes.get(app.focus).is_some_and(|ps| ps.find.is_none()));
    assert!(d.read_file(BOOKMARKS_PATH).is_err());
    // Closing it is a plain removal.
    let _ = app.update(Message::Tabs(TabsMsg::CloseActive));
    assert_eq!(settings_tabs(&app), 0);
}

// ---- app: editor.toml ----

#[test]
fn absent_editor_toml_defaults_and_save_creates_exact_name() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    assert_eq!(app.prefs, Prefs::default());
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert_eq!(f.state, LoadState::Absent);
    assert!(!f.dirty, "the surround seed alone is not an edit");
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::FontSize(30.0))));
    assert!(d.read_file(EDITOR_PATH).is_err(), "no write before Save");
    assert!(
        (app.prefs.font_size - 30.0).abs() < f32::EPSILON,
        "applied live"
    );
    let _ = app.update(s(SettingsMsg::Save(EDITOR_PATH.into())));
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
    assert_eq!(f.base.as_ref(), Some(&h));
    assert!(!f.dirty && f.state == LoadState::Loaded);
}

#[test]
fn editor_toml_appeared_since_load_is_a_conflict() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::FontSize(30.0))));
    let theirs = "font_size = 12\n";
    d.write_note(EDITOR_PATH, theirs, &ContentHash::from_hex(""))
        .expect("external create");
    let _ = app.update(s(SettingsMsg::Save(EDITOR_PATH.into())));
    assert_eq!(d.read_file(EDITOR_PATH).expect("r").0, theirs.as_bytes());
    assert_eq!(
        config_dir_entries(&d),
        ["editor.toml"],
        "never a suffixed copy"
    );
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert_eq!(f.notice.as_deref(), Some(CHANGED_ON_DISK));
    assert!(!f.dirty, "reloaded");
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
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::DeleteMode(
        crate::daemon::DeleteMode::Permanent,
    ))));
    d.write_note(EDITOR_PATH, "font_size = 26\n", &h0)
        .expect("external");
    let _ = app.update(s(SettingsMsg::Save(EDITOR_PATH.into())));
    assert_eq!(d.read_file(EDITOR_PATH).expect("r").0, b"font_size = 26\n");
    assert_eq!(
        app.settings
            .file(EDITOR_PATH)
            .and_then(|f| f.notice.as_deref()),
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
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::UiScale(1.25))));
    changed(&mut app, EDITOR_PATH, Some(h1.as_str().to_owned()));
    assert!(app.settings.file(EDITOR_PATH).is_some_and(|f| f.dirty));
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
    assert!(matches!(f.state, LoadState::Malformed(_)));
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::FontSize(30.0))));
    let _ = app.update(s(SettingsMsg::Save(EDITOR_PATH.into())));
    assert_eq!(d.read_file(EDITOR_PATH).expect("r").0, bad.as_bytes());
    // Start over + Save replaces it through CAS on the malformed content.
    let _ = app.update(s(SettingsMsg::StartOver(EDITOR_PATH.into())));
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::FontSize(30.0))));
    let _ = app.update(s(SettingsMsg::Save(EDITOR_PATH.into())));
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
            .and_then(|f| f.doc.as_ref())
            .and_then(|doc| doc.get_str(&Path::root().key("surround"))),
        Some("light")
    );
    // After a save, editor.toml is the source (the local copy no longer wins).
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::Surround(Surround::Dark))));
    let _ = app.update(s(SettingsMsg::Save(EDITOR_PATH.into())));
    std::fs::write(&state, br#"{"surround":"med"}"#).expect("state");
    let app = app_over(&d, Some(state));
    assert_eq!(app.prefs.surround, Surround::Dark);
}

#[test]
fn theme_cycle_edits_editor_toml_in_memory_only() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let _ = app.update(Message::Theme(crate::message::ThemeMsg::Cycle));
    assert_eq!(app.prefs.surround, Surround::Light);
    assert!(app.settings.file(EDITOR_PATH).is_some_and(|f| f.dirty));
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
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::FontSize(44.0))));
    assert!(
        (app.prefs.font_size - 44.0).abs() < f32::EPSILON,
        "source size"
    );
    let after = crate::reading::view::scaled(
        crate::reading::view::block_size(None),
        app.prefs.text_scale(),
    );
    assert!((after - 2.0 * before).abs() < 1e-4, "{before} → {after}");
    // Revert drops the unsaved size.
    let _ = app.update(s(SettingsMsg::Revert(EDITOR_PATH.into())));
    assert_eq!(app.prefs, Prefs::default());
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
    let _ = app.update(Message::Explorer(ExplorerMsg::Refresh));
    let _ = app.update(Message::Explorer(ExplorerMsg::Open("wp3del/one.md".into())));
    let _ = app.update(Message::Explorer(ExplorerMsg::Select(
        "wp3del/one.md".into(),
    )));
    let _ = app.update(Message::Explorer(ExplorerMsg::Trash));
    let c = app
        .dialog
        .as_ref()
        .and_then(|d| d.delete_confirm())
        .expect("delete dialog (legacy confirm_trash = false is ignored)");
    assert_eq!(c.ticked(), ["wp3del/own.png"]);
    assert_eq!(c.items.len(), 2);
    // Escape = Cancel: nothing deleted.
    let _ = app.update(Message::Dialog(DialogMsg::Cancel));
    assert!(app.dialog.is_none());
    assert!(d.read_file("wp3del/one.md").is_ok(), "not yet trashed");
    // Again, and the referenced item cannot be ticked.
    let _ = app.update(Message::Explorer(ExplorerMsg::Trash));
    let shared_ix = app
        .dialog
        .as_ref()
        .and_then(|d| d.delete_confirm())
        .and_then(|c| c.items.iter().position(|(i, _)| i.referenced_elsewhere))
        .expect("shared listed");
    let _ = app.update(Message::Dialog(DialogMsg::Toggle(shared_ix)));
    assert_eq!(
        app.dialog
            .as_ref()
            .and_then(|d| d.delete_confirm())
            .map(crate::dialog::DeleteConfirm::ticked),
        Some(vec!["wp3del/own.png".to_owned()])
    );
    // Enter is Cancel on this destructive dialog.
    let _ = app.update(Message::Dialog(DialogMsg::Default));
    assert!(d.read_file("wp3del/one.md").is_ok());
    let _ = app.update(Message::Explorer(ExplorerMsg::Trash));
    let _ = app.update(Message::Dialog(DialogMsg::Ok));
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
    let _ = app.update(Message::Explorer(ExplorerMsg::Open("wp3perm/a.md".into())));
    let _ = app.update(Message::Explorer(ExplorerMsg::DeleteActive));
    let _ = app.update(Message::Dialog(crate::dialog::DialogMsg::Ok));
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
    let _ = app.update(s(SettingsMsg::Open));
    let _ = app.update(s(SettingsMsg::Section(Section::Status)));
    assert!(app.settings.status.result.is_some(), "status on open");
    app.settings.status.result = None;
    let _ = app.update(Message::Status(StatusMsg::Notification(
        Notification::ConfigReloaded {
            errors: vec!["providers.toml: bad (line 2)".into(), "mystery".into()],
        },
    )));
    assert_eq!(
        app.settings
            .file(Section::Providers.file().expect("p"))
            .expect("f")
            .reload_errors,
        ["providers.toml: bad (line 2)"]
    );
    assert_eq!(app.settings.status.unrouted, ["mystery"]);
    assert!(app.settings.status.result.is_some(), "refreshed on reload");
    assert_eq!(
        app.settings.status.result.as_ref().map(|r| r.mode),
        Some(app.status.mode)
    );
}

#[test]
fn bookmarks_absent_then_appeared_is_a_conflict_not_a_copy() {
    let (_tmp, d) = fake();
    d.create_note("wp2bm/a.md", "a").expect("create");
    let mut app = app_over(&d, None);
    assert!(
        app.settings
            .file(BOOKMARKS_PATH)
            .is_some_and(|f| f.base.is_none())
    );
    let theirs = "[[bookmark]]\nname = \"t\"\npath = \"t.md\"\n";
    d.write_note(BOOKMARKS_PATH, theirs, &ContentHash::from_hex(""))
        .expect("external");
    let _ = app.update(Message::Tabs(TabsMsg::Open("wp2bm/a.md".into())));
    let _ = app.update(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::AddActive)));
    assert_eq!(d.read_file(BOOKMARKS_PATH).expect("r").0, theirs.as_bytes());
    assert_eq!(
        config_dir_entries(&d),
        ["bookmarks.toml"],
        "no suffixed copy"
    );
    assert!(
        app.status
            .error
            .as_deref()
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
    let _ = app.update(s(SettingsMsg::Open));
    let _ = app.update(s(SettingsMsg::Edit(
        kinds.into(),
        DocEdit::Set(
            Path::parse("kinds.book.folder").expect("p"),
            ScalarValue::Str("library".into()),
        ),
    )));
    assert!(app.settings.file(kinds).is_some_and(|f| f.dirty));
    let _ = app.update(s(SettingsMsg::Save(kinds.into())));
    let text = String::from_utf8(d.read_file(kinds).expect("r").0).expect("utf8");
    assert!(
        text.contains("# my kinds") && text.contains("# keep"),
        "{text}"
    );
    assert!(text.contains("folder = \"library\""), "{text}");
}

// ---- plan editor-feedback-round-2 WP-2: auto-save ----

fn seq(app: &App, path: &str) -> u64 {
    app.settings.file(path).map_or(0, |f| f.edit_seq)
}

fn due(app: &mut App, path: &str) {
    let n = seq(app, path);
    let _ = app.update(s(SettingsMsg::AutoSaveDue(path.into(), n)));
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
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::FontSize(30.0))));
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert_eq!(f.scheduled_seq, f.edit_seq, "a debounce timer was armed");
    assert!(disk(&d, EDITOR_PATH).is_none(), "nothing before the timer");
    due(&mut app, EDITOR_PATH);
    let (_, h) = d.read_file(EDITOR_PATH).expect("written");
    assert!(disk(&d, EDITOR_PATH).is_some_and(|t| t.contains("font_size = 30")));
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert!(!f.dirty && f.base.as_ref() == Some(&h));
    assert_eq!(f.save_status().map(|s| s.0).as_deref(), Some("saved"));
    // The same timer again writes nothing more.
    due(&mut app, EDITOR_PATH);
    assert_eq!(d.read_file(EDITOR_PATH).expect("r").1, h);
}

#[test]
fn autosave_rapid_edits_coalesce_into_one_write() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::FontSize(20.0))));
    let first = seq(&app, EDITOR_PATH);
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::FontSize(22.0))));
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::FontSize(24.0))));
    // The superseded timers fire: no write.
    let _ = app.update(s(SettingsMsg::AutoSaveDue(EDITOR_PATH.into(), first)));
    let _ = app.update(s(SettingsMsg::AutoSaveDue(EDITOR_PATH.into(), first + 1)));
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
    let _ = app.update(s(SettingsMsg::Open));
    let _ = app.update(s(SettingsMsg::Edit(
        kinds.into(),
        DocEdit::Set(
            Path::parse("kinds.book.folder").expect("p"),
            ScalarValue::Int(1),
        ),
    )));
    let f = app.settings.file(kinds).expect("f");
    assert!(f.dirty && !f.errors.is_empty(), "{:?}", f.errors);
    assert_ne!(f.scheduled_seq, f.edit_seq, "no timer for an invalid state");
    assert!(
        f.save_status()
            .is_some_and(|(l, warn)| warn && l.starts_with("not saved: ")),
        "{:?}",
        f.save_status()
    );
    due(&mut app, kinds);
    let _ = app.update(s(SettingsMsg::Save(kinds.into())));
    let _ = app.update(Message::Tabs(TabsMsg::CloseActive));
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
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::FontSize(30.0))));
    // The first write is in flight (not run inline).
    app.rpc_inline = false;
    due(&mut app, EDITOR_PATH);
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert!(f.saving);
    let first = f.doc.as_ref().expect("doc").to_string();
    let base = f.base.clone();
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::FontSize(32.0))));
    due(&mut app, EDITOR_PATH);
    assert!(app.settings.file(EDITOR_PATH).is_some_and(|f| f.queued));
    // The first write lands; the queued one runs right after.
    app.rpc_inline = true;
    let r = write_config(d.as_ref(), EDITOR_PATH, &first, base.as_ref());
    let _ = app.update(s(SettingsMsg::Saved(EDITOR_PATH.into(), r)));
    let text = disk(&d, EDITOR_PATH).expect("written");
    assert!(text.contains("font_size = 32"), "{text}");
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert!(!f.dirty && !f.saving && !f.queued);
}

#[test]
fn autosave_cas_conflict_reloads_without_overwrite() {
    let (_tmp, d) = fake();
    let h0 = d
        .write_note(EDITOR_PATH, "font_size = 20\n", &ContentHash::from_hex(""))
        .expect("seed");
    let mut app = app_over(&d, None);
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::FontSize(30.0))));
    d.write_note(EDITOR_PATH, "font_size = 26\n", &h0)
        .expect("external");
    due(&mut app, EDITOR_PATH);
    assert_eq!(disk(&d, EDITOR_PATH).as_deref(), Some("font_size = 26\n"));
    let f = app.settings.file(EDITOR_PATH).expect("f");
    assert_eq!(f.notice.as_deref(), Some(CHANGED_ON_DISK));
    assert!(!f.dirty);
    assert!((app.prefs.font_size - 26.0).abs() < f32::EPSILON);
}

#[test]
fn autosave_views_parse_error_blocks_compile_warning_does_not() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let path = app.settings.new_view_file("mine").expect("new");
    let _ = app.update(s(SettingsMsg::Edit(
        path.clone(),
        DocEdit::Set(Path::root().key("invented"), ScalarValue::Int(1)),
    )));
    assert!(
        app.settings
            .file(&path)
            .is_some_and(|f| !f.errors.is_empty())
    );
    due(&mut app, &path);
    assert!(disk(&d, &path).is_none(), "a parse error blocks the write");
    let bad = "[[views]]\ntype = \"table\"\nname = \"T\"\nfilters = \"((\"\n";
    if let Some(f) = app.settings.file_mut(&path) {
        f.doc = Some(ConfigDoc::parse(bad).expect("doc"));
    }
    let _ = app.update(s(SettingsMsg::Edit(
        path.clone(),
        DocEdit::Set(
            Path::parse("views[0].name").expect("p"),
            ScalarValue::Str("U".into()),
        ),
    )));
    let f = app.settings.file(&path).expect("f");
    assert!(f.errors.is_empty() && !f.warnings.is_empty());
    due(&mut app, &path);
    let text = disk(&d, &path).expect("a compile warning does not block");
    assert!(text.contains("name = \"U\""), "{text}");
}

#[test]
fn autosave_flushes_on_settings_tab_close_and_quit() {
    let (_tmp, d) = fake();
    let mut app = app_over(&d, None);
    let _ = app.update(s(SettingsMsg::Open));
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::FontSize(30.0))));
    assert!(disk(&d, EDITOR_PATH).is_none());
    let _ = app.update(Message::Tabs(TabsMsg::CloseActive));
    assert_eq!(settings_tabs(&app), 0);
    assert!(disk(&d, EDITOR_PATH).is_some_and(|t| t.contains("font_size = 30")));
    // Quit flushes a pending write before exiting.
    let _ = app.update(s(SettingsMsg::Pref(PrefEdit::FontSize(34.0))));
    let _ = app.update(Message::Tabs(TabsMsg::Quit));
    assert!(disk(&d, EDITOR_PATH).is_some_and(|t| t.contains("font_size = 34")));
    assert!(app.exiting);
}
