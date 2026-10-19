//! plan editor-settings-tab WP-4 tests: the effective keymap (resolver precedence, audit E1 —
//! every key handler consults it), the Hotkeys page capture / Clear / Reset, and period folders
//! from `captures.toml` (audit E2). Synthetic vaults + `FakeDaemon` only.

use std::collections::BTreeMap;
use std::sync::Arc;

use cv_protocol::{ActionInfo, RunActionParams};
use cv_schema::Path;
use cv_testkit::{SyntheticVault, VaultSpec};
use iced::keyboard::{Key, Modifiers, key::Physical};
use iced::widget::text_editor::{Binding, KeyPress, Status};

use super::editor_prefs::EDITOR_PATH;
use super::pages::hotkeys::{self, ACTIONS_PATH, HotkeyMsg, Target};
use super::*;
use crate::app::{App, CAPTURES_PATH, TabKind};
use crate::daemon::FakeDaemon;
use crate::message::{ExplorerMsg, PaletteMsg, ReadingMsg};
use crate::palette::chord::{self, Chord};
use crate::palette::{Bound, ClientCmd, KeySource, Keymap, resolve};
use crate::reading::ViewMode;

fn fake() -> (tempfile::TempDir, Arc<FakeDaemon>) {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    SyntheticVault::build(&vault, &VaultSpec::default()).expect("vault");
    let d = FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake");
    (tmp, Arc::new(d))
}

fn put(d: &FakeDaemon, path: &str, body: &str) {
    let p = d.vault_root().join(path);
    std::fs::create_dir_all(p.parent().expect("parent")).expect("dir");
    std::fs::write(p, body).expect("write");
}

fn app_over(d: &Arc<FakeDaemon>) -> App {
    let dc: Arc<dyn DaemonClient> = Arc::clone(d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    app
}

fn c(s: &str) -> Chord {
    chord::parse(s).expect("chord")
}

fn key(app: &mut App, k: &str) {
    let _ = app.send(Message::Palette(PaletteMsg::Key(c(k))));
}

fn hk(app: &mut App, m: HotkeyMsg) {
    let _ = app.send(Message::Settings(SettingsMsg::Hotkey(m)));
}

fn open(app: &mut App, p: &str) {
    let _ = app.send(Message::Explorer(ExplorerMsg::Open(p.into())));
}

fn set_mode(app: &mut App, p: &str, mode: ViewMode) {
    let _ = app.send(Message::Reading(ReadingMsg::SetMode {
        path: p.into(),
        mode,
    }));
}

fn ov(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.iter().map(|s| (*s).to_owned()).collect()))
        .collect()
}

fn info(id: &str, keys: &[&str]) -> ActionInfo {
    ActionInfo {
        id: id.into(),
        title: id.into(),
        group: None,
        keys: keys.iter().map(|k| (*k).to_owned()).collect(),
        params: vec![],
        needs: vec![],
        durable: false,
    }
}

/// Registers `id` (with `keys`) on the fake daemon, counting its runs.
fn register(d: &FakeDaemon, id: &str, keys: &[&str]) -> Arc<std::sync::Mutex<usize>> {
    let n = Arc::new(std::sync::Mutex::new(0));
    let seen = Arc::clone(&n);
    d.register_action(
        info(id, keys),
        Arc::new(move |_: &RunActionParams| {
            if let Ok(mut g) = seen.lock() {
                *g += 1;
            }
            Ok(None)
        }),
    )
    .expect("register");
    n
}

fn kp(ch: &str, m: Modifiers) -> KeyPress {
    KeyPress {
        key: Key::Character(ch.into()),
        modified_key: Key::Character(ch.into()),
        physical_key: Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified),
        modifiers: m,
        text: Some(ch.into()),
        status: Status::Focused { is_hovered: false },
    }
}

/// The chord a focused source editor hands to the app, if any.
fn routed(b: Option<Binding<Message>>) -> Option<String> {
    match b {
        Some(Binding::Custom(Message::Palette(PaletteMsg::Key(c)))) => Some(c.to_string()),
        _ => None,
    }
}

fn keys_of(app: &App, id: &str) -> Option<Vec<String>> {
    app.settings
        .file(EDITOR_PATH)?
        .doc
        .as_ref()?
        .get_str_list(&Path::root().key("keys").key(id))
}

const NOTE: &str = "hk/a.md";

fn app_with_note(d: &Arc<FakeDaemon>) -> App {
    d.create_note(NOTE, "body").expect("note");
    let mut app = app_over(d);
    open(&mut app, NOTE);
    app
}

// ---- pure: the resolver ----

#[test]
fn no_overrides_is_todays_keymap() {
    let km = Keymap::default();
    for (k, _) in crate::palette::keymap() {
        let ch = c(&k);
        let fixed = matches!(k.as_str(), "Ctrl+S" | "Ctrl+Z" | "Ctrl+Shift+Z")
            || k.starts_with("Ctrl+Alt+");
        if fixed {
            assert!(km.get(&ch).is_none(), "{k} stays with the text editor");
            continue;
        }
        assert!(
            matches!(km.get(&ch), Some(Bound::Client(_) | Bound::Preset(_))),
            "{k}"
        );
        assert!(km.routed(&ch), "{k}");
    }
    let client = |k: &str| km.get(&c(k)).cloned();
    assert_eq!(
        client("Ctrl+E"),
        Some(Bound::Client(ClientCmd::CycleReading))
    );
    assert_eq!(
        client("Ctrl+P"),
        Some(Bound::Client(ClientCmd::QuickSwitcher))
    );
    assert_eq!(
        client("Ctrl+,"),
        Some(Bound::Client(ClientCmd::OpenSettings))
    );
    assert_eq!(client("Alt+3"), Some(Bound::Client(ClientCmd::GoToTab(3))));
    assert_eq!(client("Alt+9"), None);
    assert!(matches!(client("Ctrl+O"), Some(Bound::Preset(p)) if p.action == "period.open"));
    assert!(matches!(client("Ctrl+R"), Some(Bound::Preset(p)) if p.action == "lint-note"));
    assert!(km.notices.is_empty(), "{:?}", km.notices);
}

#[test]
fn override_moves_a_chord_and_empty_unbinds() {
    let km = resolve(&ov(&[("close_tab", &["Ctrl+Shift+W"]), ("quit", &[])]), &[]);
    assert_eq!(
        km.get(&c("Ctrl+Shift+W")),
        Some(&Bound::Client(ClientCmd::CloseTab))
    );
    assert_eq!(km.get(&c("Ctrl+W")), None, "the default no longer fires");
    assert_eq!(km.get(&c("Ctrl+Q")), None, "[] unbinds");
    assert_eq!(km.chords["quit"], Vec::<String>::new());
    assert!(km.overridden.contains("quit"));
    // A RESERVED editor default (Ctrl+E) is rebindable too.
    let km = resolve(&ov(&[("cycle_reading", &["Alt+E"])]), &[]);
    assert_eq!(
        km.get(&c("Alt+E")),
        Some(&Bound::Client(ClientCmd::CycleReading))
    );
    assert!(km.get(&c("Ctrl+E")).is_none());
    assert!(km.routed(&c("Alt+E")));
    // Presets take overrides by their stable ids.
    let km = resolve(
        &ov(&[
            ("open_period.day", &["Alt+D"]),
            ("preset.lint-note", &["Alt+L"]),
        ]),
        &[],
    );
    assert!(matches!(km.get(&c("Alt+D")), Some(Bound::Preset(p)) if p.period.is_some()));
    assert!(matches!(km.get(&c("Alt+L")), Some(Bound::Preset(p)) if p.action == "lint-note"));
    assert!(km.get(&c("Ctrl+O")).is_none() && km.get(&c("Ctrl+R")).is_none());
}

#[test]
fn conflicting_claims_register_none_and_mark_every_claimant() {
    // Plan editor-feedback-round-2 WP-1: quit's override, close_tab's default and action b
    // all claim Ctrl+W → nobody gets it; the override still releases quit's Ctrl+Q default,
    // which action a then holds alone.
    let acts = [info("a", &["Ctrl+Q"]), info("b", &["Ctrl+W"])];
    let km = resolve(&ov(&[("quit", &["Ctrl+W"])]), &acts);
    assert_eq!(km.get(&c("Ctrl+W")), None);
    for id in ["quit", "close_tab", "b"] {
        assert!(km.conflicted.contains(id), "{id} marked");
        assert!(
            km.row_notices[id]
                .iter()
                .any(|n| n.contains("Ctrl+W conflicts with")),
            "{id}: {:?}",
            km.row_notices.get(id)
        );
        assert_eq!(km.chords[id], Vec::<String>::new(), "{id} holds nothing");
    }
    assert_eq!(km.get(&c("Ctrl+Q")), Some(&Bound::Action("a".into())));
    assert!(!km.conflicted.contains("a"));
    assert!(
        !km.routed(&c("Ctrl+Q")),
        "actions are not routed out of the editor"
    );
}

#[test]
fn same_chord_twice_in_one_action_is_collapsed() {
    let km = resolve(
        &BTreeMap::new(),
        &[info("a", &["Ctrl+Shift+K", "ctrl+shift+k"])],
    );
    assert_eq!(km.get(&c("Ctrl+Shift+K")), Some(&Bound::Action("a".into())));
    assert_eq!(km.chords["a"], ["Ctrl+Shift+K"]);
    assert!(
        km.conflicted.is_empty() && km.notices.is_empty(),
        "{:?}",
        km.notices
    );
}

#[test]
fn empty_unbind_of_a_default_releases_its_chord() {
    // Without the unbind, action a's Ctrl+W collides with close_tab's default.
    let acts = [info("a", &["Ctrl+W"])];
    assert_eq!(resolve(&BTreeMap::new(), &acts).get(&c("Ctrl+W")), None);
    let km = resolve(&ov(&[("close_tab", &[])]), &acts);
    assert_eq!(km.get(&c("Ctrl+W")), Some(&Bound::Action("a".into())));
    assert!(km.conflicted.is_empty(), "{:?}", km.notices);
}

#[test]
fn bad_override_chords_become_notices() {
    let km = resolve(
        &ov(&[
            ("quit", &["Ctrl+S", "Nope+Q", "K", "Ctrl+K"]),
            ("close_tab", &["Ctrl+K"]),
            ("save", &["Ctrl+J"]),
            ("no_such", &["Ctrl+J"]),
        ]),
        &[],
    );
    let has = |s: &str| km.notices.iter().any(|n| n.contains(s));
    assert!(has("quit: Ctrl+S is reserved"), "{:?}", km.notices);
    assert!(has("quit: unreadable key \"Nope+Q\""));
    assert!(has("quit: K needs Ctrl or Alt"));
    assert!(has("quit: Ctrl+K conflicts with close_tab"));
    assert!(has("close_tab: Ctrl+K conflicts with quit"));
    assert!(has("save: a text-editing command"));
    assert!(has("no_such: unknown command id"));
    assert!(km.get(&c("Ctrl+J")).is_none());
    assert!(km.get(&c("Ctrl+S")).is_none());
    assert!(
        km.get(&c("Ctrl+K")).is_none(),
        "contested: bound to neither"
    );
}

#[test]
fn ctrl_comma_opens_settings_and_ctrl_shift_comma_cannot_be_bound() {
    let km = Keymap::default();
    assert_eq!(
        km.get(&c("Ctrl+,")),
        Some(&Bound::Client(ClientCmd::OpenSettings))
    );
    let km = resolve(
        &ov(&[("quit", &["Ctrl+Shift+,"])]),
        &[info("a", &["Ctrl+Shift+,"])],
    );
    assert!(km.get(&c("Ctrl+Shift+,")).is_none());
    assert_eq!(km.notices.len(), 2, "{:?}", km.notices);
    assert!(
        km.routed(&c("Ctrl+Shift+,")),
        "still kept away from the text editor"
    );

    let (_tmp, d) = fake();
    let mut app = app_over(&d);
    key(&mut app, "Ctrl+,");
    let n = app
        .panes
        .iter()
        .flat_map(|(_, ps)| ps.tabs.iter())
        .filter(|t| t.kind() == TabKind::Settings)
        .count();
    assert_eq!(n, 1);
}

// ---- app: every handler consults the effective keymap (audit E1) ----

#[test]
fn rebound_client_command_fires_in_a_focused_source_editor() {
    let (_tmp, d) = fake();
    put(&d, EDITOR_PATH, "[keys]\nclose_tab = [\"Ctrl+Shift+W\"]\n");
    let mut app = app_with_note(&d);
    set_mode(&mut app, NOTE, ViewMode::Source);
    let km = Arc::clone(&app.palette.keymap);
    let cs = Modifiers::CTRL | Modifiers::SHIFT;
    assert_eq!(
        routed(crate::source::test_key_binding_with(kp("W", cs), &km)).as_deref(),
        Some("Ctrl+Shift+W")
    );
    assert_eq!(
        routed(crate::source::test_key_binding_with(
            kp("w", Modifiers::CTRL),
            &km
        )),
        None,
        "the moved-away default no longer routes"
    );
    key(&mut app, "Ctrl+W");
    assert_eq!(app.open_paths(), [NOTE], "old chord does nothing");
    key(&mut app, "Ctrl+Shift+W");
    assert!(app.open_paths().is_empty(), "new chord closes the tab");
}

#[test]
fn rebound_client_command_fires_in_reading_mode() {
    let (_tmp, d) = fake();
    put(&d, EDITOR_PATH, "[keys]\nclose_tab = [\"Alt+W\"]\n");
    let mut app = app_with_note(&d);
    set_mode(&mut app, NOTE, ViewMode::Reading);
    key(&mut app, "Ctrl+W");
    assert_eq!(app.open_paths(), [NOTE]);
    key(&mut app, "Alt+W");
    assert!(app.open_paths().is_empty());
}

#[test]
fn ctrl_e_is_an_ordinary_rebindable_command() {
    let (_tmp, d) = fake();
    let mut app = app_with_note(&d);
    set_mode(&mut app, NOTE, ViewMode::Reading);
    // Default: Ctrl+E through the keymap (no Reading listener anymore).
    key(&mut app, "Ctrl+E");
    assert_eq!(app.reading.mode(NOTE), ViewMode::Source);
    key(&mut app, "Ctrl+E");
    assert_eq!(app.reading.mode(NOTE), ViewMode::Reading);
    // Rebound live (no restart) through the Settings edit path.
    let _ = app.send(Message::Settings(SettingsMsg::Edit(
        EDITOR_PATH.into(),
        DocEdit::Set(
            Path::root().key("keys").key("cycle_reading"),
            ScalarValue::StrList(vec!["Ctrl+Shift+E".into()]),
        ),
    )));
    key(&mut app, "Ctrl+E");
    assert_eq!(app.reading.mode(NOTE), ViewMode::Reading, "old chord inert");
    key(&mut app, "Ctrl+Shift+E");
    assert_eq!(app.reading.mode(NOTE), ViewMode::Source);
    // The focused source editor routes the new chord, not Ctrl+E.
    let km = Arc::clone(&app.palette.keymap);
    let cs = Modifiers::CTRL | Modifiers::SHIFT;
    assert!(routed(crate::source::test_key_binding_with(kp("E", cs), &km)).is_some());
    assert!(
        routed(crate::source::test_key_binding_with(
            kp("e", Modifiers::CTRL),
            &km
        ))
        .is_none()
    );
}

#[test]
fn unbound_command_chord_is_disabled() {
    let (_tmp, d) = fake();
    put(&d, EDITOR_PATH, "[keys]\nquit = []\n");
    let mut app = app_over(&d);
    key(&mut app, "Ctrl+Q");
    assert!(!app.exiting, "Ctrl+Q unbound");
}

#[test]
fn config_action_takes_a_chord_an_override_freed() {
    let (_tmp, d) = fake();
    put(&d, EDITOR_PATH, "[keys]\nquit = [\"Ctrl+Shift+Q\"]\n");
    let runs = register(&d, "a", &["Ctrl+Q"]);
    let mut app = app_over(&d);
    key(&mut app, "Ctrl+Q");
    assert_eq!(*runs.lock().expect("lock"), 1, "the action runs");
    assert!(!app.exiting);
    key(&mut app, "Ctrl+Shift+Q");
    assert!(app.exiting, "quit moved");
}

#[test]
fn palette_entries_show_effective_chords() {
    let (_tmp, d) = fake();
    put(&d, EDITOR_PATH, "[keys]\nquick_switcher = [\"Alt+P\"]\n");
    let app = app_over(&d);
    let e = app
        .palette
        .entries
        .iter()
        .find(|e| e.id == crate::palette::EntryId::Client(ClientCmd::QuickSwitcher))
        .expect("entry");
    assert_eq!(e.keys, ["Alt+P"]);
}

// ---- the Hotkeys page ----

fn hotkeys_app(d: &Arc<FakeDaemon>) -> App {
    let mut app = app_over(d);
    let _ = app.send(Message::Settings(SettingsMsg::Open));
    let _ = app.send(Message::Settings(SettingsMsg::Section(Section::Hotkeys)));
    app
}

#[test]
fn record_captures_the_next_chord_into_editor_toml_without_writing() {
    let (_tmp, d) = fake();
    let mut app = hotkeys_app(&d);
    hk(&mut app, HotkeyMsg::Record(Target::Command("quit".into())));
    key(&mut app, "Alt+K");
    assert!(!app.exiting);
    assert_eq!(keys_of(&app, "quit"), Some(vec!["Alt+K".into()]));
    assert!(app.settings.hotkeys.recording.is_none());
    assert!(app.settings.file(EDITOR_PATH).is_some_and(|f| f.dirty));
    assert!(
        !d.vault_root().join(EDITOR_PATH).exists(),
        "nothing written without Save"
    );
    // Live: the keymap and the page snapshot already use it.
    assert_eq!(
        app.palette.keymap.get(&c("Alt+K")),
        Some(&Bound::Client(ClientCmd::Quit))
    );
    let rows = hotkeys::rows(&app.settings.hotkeys, None);
    let q = rows
        .iter()
        .find(|r| r.target == Target::Command("quit".into()))
        .expect("row");
    assert_eq!(
        (q.chords.clone(), q.source),
        (vec!["Alt+K".into()], KeySource::EditorToml)
    );
    // Save writes it through the exact-name path.
    let _ = app.send(Message::Settings(app.settings.due_now(EDITOR_PATH)));
    let body = std::fs::read_to_string(d.vault_root().join(EDITOR_PATH)).expect("saved");
    assert!(body.contains("quit = [\"Alt+K\"]"), "{body}");
}

const ACTION_STEP: &str =
    "\n[[action.steps]]\ntype = \"move\"\npath = \"{file}\"\nto = \"archive\"\n";

#[test]
fn assigning_a_chord_held_elsewhere_is_refused() {
    let (_tmp, d) = fake();
    put(
        &d,
        ACTIONS_PATH,
        &format!("[[action]]\nid = \"a\"\nkeys = [\"Ctrl+Shift+K\"]\n{ACTION_STEP}"),
    );
    register(&d, "a", &["Ctrl+Shift+K"]);
    let mut app = hotkeys_app(&d);
    // An editor default (Close tab's Ctrl+W).
    hk(&mut app, HotkeyMsg::Record(Target::Command("quit".into())));
    key(&mut app, "Ctrl+W");
    assert_eq!(keys_of(&app, "quit"), None, "nothing edited");
    assert!(!app.exiting);
    let note = app.settings.hotkeys.note.clone().unwrap_or_default();
    assert!(note.contains("Ctrl+W is already assigned to"), "{note}");
    // A config action's key.
    hk(&mut app, HotkeyMsg::Record(Target::Command("quit".into())));
    key(&mut app, "Ctrl+Shift+K");
    assert_eq!(keys_of(&app, "quit"), None);
    let note = app.settings.hotkeys.note.clone().unwrap_or_default();
    assert!(note.contains("already assigned to a"), "{note}");
    // An action taking an editor default.
    hk(&mut app, HotkeyMsg::Record(Target::Action("a".into())));
    key(&mut app, "Ctrl+Q");
    let keys = Path::root().key("action").index(0).key("keys");
    let doc = app
        .settings
        .file(ACTIONS_PATH)
        .and_then(|f| f.doc.as_ref())
        .and_then(|d| d.get_str_list(&keys));
    assert_eq!(doc, Some(vec!["Ctrl+Shift+K".into()]), "unchanged");
    assert!(!app.settings.file(ACTIONS_PATH).is_some_and(|f| f.dirty));
    // Re-recording its own chord is not a conflict.
    hk(&mut app, HotkeyMsg::Record(Target::Action("a".into())));
    key(&mut app, "Ctrl+Shift+K");
    let note = app.settings.hotkeys.note.clone().unwrap_or_default();
    assert!(!note.contains("Ctrl+Shift+K is already"), "{note}");
}

#[test]
fn binding_an_action_key_keeps_every_action_row() {
    // The "actions vanish after binding" repro: bind + Save → the daemon reloads actions.toml
    // (ConfigReloaded / FileChanged invalidate the palette's action cache) → the next Hotkeys
    // edit used to sync an EMPTY action list onto the page.
    let (_tmp, d) = fake();
    put(
        &d,
        ACTIONS_PATH,
        &format!("[[action]]\nid = \"a\"\n{ACTION_STEP}\n[[action]]\nid = \"b\"\n{ACTION_STEP}"),
    );
    register(&d, "a", &[]);
    register(&d, "b", &[]);
    let mut app = hotkeys_app(&d);
    let action_rows = |app: &App| {
        hotkeys::rows(
            &app.settings.hotkeys,
            app.settings.file(ACTIONS_PATH).and_then(|f| f.doc.as_ref()),
        )
        .into_iter()
        .filter(|r| matches!(r.target, Target::Action(_)))
        .count()
    };
    assert_eq!(action_rows(&app), 2);
    hk(&mut app, HotkeyMsg::Record(Target::Action("a".into())));
    key(&mut app, "Alt+J");
    let _ = app.send(Message::Settings(app.settings.due_now(ACTIONS_PATH)));
    let _ = app.send(Message::Status(crate::message::StatusMsg::Notification(
        cv_protocol::Notification::ConfigReloaded { errors: vec![] },
    )));
    assert_eq!(action_rows(&app), 2, "after the reload");
    hk(&mut app, HotkeyMsg::Record(Target::Action("b".into())));
    key(&mut app, "Alt+M");
    assert_eq!(action_rows(&app), 2, "after the next binding");
    // A bare invalidation (FileChanged on a config path) keeps the snapshot too.
    app.palette.invalidate();
    hk(&mut app, HotkeyMsg::Clear(Target::Command("quit".into())));
    assert_eq!(action_rows(&app), 2, "after an invalidation");
}

#[test]
fn escape_cancels_and_reserved_chords_are_refused() {
    let (_tmp, d) = fake();
    let mut app = hotkeys_app(&d);
    hk(&mut app, HotkeyMsg::Record(Target::Command("quit".into())));
    let _ = app.send(Message::Palette(PaletteMsg::Close));
    assert!(app.settings.hotkeys.recording.is_none());
    assert_eq!(keys_of(&app, "quit"), None);
    hk(&mut app, HotkeyMsg::Record(Target::Command("quit".into())));
    key(&mut app, "Ctrl+C");
    assert_eq!(keys_of(&app, "quit"), None);
    assert!(
        app.settings
            .hotkeys
            .note
            .as_deref()
            .is_some_and(|n| n.contains("reserved"))
    );
}

#[test]
fn clear_and_reset_a_command() {
    let (_tmp, d) = fake();
    let mut app = hotkeys_app(&d);
    hk(&mut app, HotkeyMsg::Clear(Target::Command("quit".into())));
    assert_eq!(keys_of(&app, "quit"), Some(vec![]));
    assert!(app.palette.keymap.get(&c("Ctrl+Q")).is_none());
    hk(&mut app, HotkeyMsg::Reset("quit".into()));
    assert_eq!(keys_of(&app, "quit"), None);
    assert_eq!(
        app.palette.keymap.get(&c("Ctrl+Q")),
        Some(&Bound::Client(ClientCmd::Quit))
    );
}

#[test]
fn config_action_chords_edit_actions_toml() {
    let (_tmp, d) = fake();
    put(
        &d,
        ACTIONS_PATH,
        "# my actions\n[[action]]\nid = \"a\"\nkeys = [\"Ctrl+Shift+K\"]\n\n[[action.steps]]\ntype = \"move\"\npath = \"{file}\"\nto = \"archive\"\n",
    );
    register(&d, "a", &["Ctrl+Shift+K"]);
    let mut app = hotkeys_app(&d);
    assert!(
        app.settings.hotkeys.actions.iter().any(|a| a.id == "a"),
        "Hotkeys fetched the action list"
    );
    hk(&mut app, HotkeyMsg::Record(Target::Action("a".into())));
    key(&mut app, "Ctrl+J");
    let keys = Path::root().key("action").index(0).key("keys");
    let doc = |app: &App| {
        app.settings
            .file(ACTIONS_PATH)
            .and_then(|f| f.doc.as_ref())
            .and_then(|d| d.get_str_list(&keys))
    };
    assert_eq!(doc(&app), Some(vec!["Ctrl+J".into()]));
    let rows = hotkeys::rows(
        &app.settings.hotkeys,
        app.settings.file(ACTIONS_PATH).and_then(|f| f.doc.as_ref()),
    );
    let r = rows
        .iter()
        .find(|r| r.target == Target::Action("a".into()))
        .expect("row");
    assert_eq!(
        (r.chords.clone(), r.source),
        (vec!["Ctrl+J".into()], KeySource::ActionsToml)
    );
    let _ = app.send(Message::Settings(app.settings.due_now(ACTIONS_PATH)));
    let body = std::fs::read_to_string(d.vault_root().join(ACTIONS_PATH)).expect("file");
    assert!(
        body.contains("# my actions") && body.contains("\"Ctrl+J\""),
        "{body}"
    );
    hk(&mut app, HotkeyMsg::Clear(Target::Action("a".into())));
    assert_eq!(doc(&app), None);
    // An action that is not in actions.toml is refused with a note.
    hk(&mut app, HotkeyMsg::Record(Target::Action("zz".into())));
    key(&mut app, "Ctrl+J");
    assert!(app.settings.hotkeys.note.is_some());
}

#[test]
fn record_edit_is_pure_and_refuses_plain_keys() {
    assert!(hotkeys::record_edit(&Target::Command("quit".into()), &c("K"), None).is_err());
    let (file, _) =
        hotkeys::record_edit(&Target::Command("quit".into()), &c("Alt+K"), None).expect("edit");
    assert_eq!(file, EDITOR_PATH);
    assert!(hotkeys::reset_edit("quit", None).is_none());
}

// ---- period folders from captures.toml (audit E2) ----

#[test]
fn period_folders_come_from_captures_with_const_fallback() {
    use editor_model::period::PeriodFolders;
    let (_tmp, d) = fake();
    let app = app_over(&d);
    assert_eq!(
        app.period_folders,
        PeriodFolders::default(),
        "absent → consts"
    );

    put(
        &d,
        CAPTURES_PATH,
        "[periods.day]\nfolder = \"journal/d\"\n[periods.week]\nfolder = \"journal/w\"\n",
    );
    let mut app = app_over(&d);
    assert_eq!(app.period_folders.day, "journal/d", "loaded at boot");
    assert_eq!(app.period_folders.month, editor_model::period::MONTH_FOLDER);
    // Live edit re-derives.
    let _ = app.send(Message::Settings(SettingsMsg::Edit(
        CAPTURES_PATH.into(),
        DocEdit::Set(
            Path::root().key("periods").key("month").key("folder"),
            ScalarValue::Str("journal/m".into()),
        ),
    )));
    assert_eq!(app.period_folders.month, "journal/m");
    assert!(
        editor_model::period::bar_for(
            "journal/d/2026-10-13.md",
            chrono::NaiveDate::from_ymd_opt(2026, 10, 14).expect("d"),
            &app.period_folders
        )
        .is_some()
    );

    put(&d, CAPTURES_PATH, "[periods.day\nfolder = ");
    let app = app_over(&d);
    assert_eq!(
        app.period_folders,
        PeriodFolders::default(),
        "malformed → consts"
    );
}

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
    // Reorder: the chord follows the bookmark.
    bm_msg(&mut app, crate::bookmarks::BookmarkMsg::Down(0));
    let on_disk = bm_on_disk(&d);
    assert_eq!(on_disk[1].path, "wp9/a.md");
    assert_eq!(on_disk[1].keys, ["Alt+9"]);
    assert_eq!(
        app.palette.keymap.get(&c("Alt+9")),
        Some(&Bound::Bookmark("wp9/a.md".into()))
    );
    // The chord opens it.
    key(&mut app, "Alt+9");
    assert_eq!(app.active_tab().map(|t| t.path.as_str()), Some("wp9/a.md"));
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
