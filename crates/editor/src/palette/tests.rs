//! WP-5 palette tests: pure prompt/chord/ranking logic, and invocation against `FakeDaemon`.
//! Synthetic vaults only (I1).

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use cv_protocol::{
    ActionInfo, JobState, JobStatus, Mode, Notification, ParamKind, ParamSpec, RunActionParams,
};
use iced::widget::text_editor::{Action, Edit};
use serde_json::{Value, json};

use super::chord::{self, Chord};
use super::prompt::{Prompt, PromptError, Step, valid_date};
use super::*;
use crate::app::App;
use crate::daemon::{ContentHash, DaemonClient, FakeDaemon};
use crate::message::{Message, PaletteMsg, SourceMsg, StatusMsg, TabsMsg};

fn spec(name: &str, kind: ParamKind, required: bool) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        label: name.to_uppercase(),
        kind,
        required,
        choices: Vec::new(),
        default: None,
    }
}

fn action(id: &str, params: Vec<ParamSpec>, keys: &[&str]) -> ActionInfo {
    ActionInfo {
        id: id.into(),
        title: format!("Title {id}"),
        group: Some("Capture".into()),
        keys: keys.iter().map(|k| (*k).to_owned()).collect(),
        params,
        needs: vec!["active_path".into()],
        durable: false,
    }
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 13).expect("date")
}

// ---- pure: prompt ----

#[test]
fn prompt_every_kind_defaults_and_map() {
    let mut e = spec("kind", ParamKind::Enum, true);
    e.choices = vec!["x".into(), "y".into()];
    let mut t = spec("title", ParamKind::Text, false);
    t.default = Some("dflt".into());
    let mut b = spec("flag", ParamKind::Bool, false);
    b.default = Some("true".into());
    let a = action(
        "t",
        vec![
            t,
            spec("body", ParamKind::Multiline, false),
            spec("path", ParamKind::Path, false),
            e,
            spec("when", ParamKind::Date, false),
            b,
        ],
        &[],
    );
    let mut p = Prompt::new(a, today());
    assert_eq!(p.text(), "dflt", "default pre-filled");
    assert_eq!(p.advance(), Ok(Step::More));
    p.set_text("line 1\nline 2\n".into());
    assert_eq!(p.advance(), Ok(Step::More));
    p.set_text(" notes/a.md ".into());
    assert_eq!(p.advance(), Ok(Step::More));
    assert_eq!(p.text(), "x", "required enum starts at its first choice");
    p.set_text("y".into());
    assert_eq!(p.advance(), Ok(Step::More));
    assert_eq!(p.text(), "2026-10-13", "date defaults to today");
    assert_eq!(p.advance(), Ok(Step::More));
    p.set_text("ignored on a bool".into());
    let Ok(Step::Done(m)) = p.advance() else {
        panic!("done");
    };
    assert_eq!(
        Value::Object(m),
        json!({
            "title": "dflt",
            "body": "line 1\nline 2\n",
            "path": "notes/a.md",
            "kind": "y",
            "when": "2026-10-13",
            "flag": true,
        })
    );
}

#[test]
fn prompt_required_blocks_and_validation() {
    let mut e = spec("kind", ParamKind::Enum, false);
    e.choices = vec!["x".into()];
    let a = action(
        "t",
        vec![
            spec("title", ParamKind::Text, true),
            spec("when", ParamKind::Date, true),
            e,
            spec("flag", ParamKind::Bool, false),
        ],
        &[],
    );
    let mut p = Prompt::new(a, today());
    assert_eq!(p.advance(), Err(PromptError::Required("TITLE".into())));
    assert_eq!(p.step, 0);
    p.set_text("   ".into());
    assert!(p.advance().is_err(), "blank is empty");
    p.set_text("Hi".into());
    assert_eq!(p.advance(), Ok(Step::More));
    p.set_text("2026-13-01".into());
    assert_eq!(p.advance(), Err(PromptError::BadDate("WHEN".into())));
    p.set_text("2026-1-1".into());
    assert!(p.advance().is_err());
    p.set_text("2026-02-28".into());
    assert_eq!(p.advance(), Ok(Step::More));
    assert_eq!(p.text(), "", "optional enum without default starts empty");
    p.set_text("zz".into());
    assert_eq!(p.advance(), Err(PromptError::BadChoice("KIND".into())));
    p.set_text(String::new());
    assert_eq!(p.advance(), Ok(Step::More));
    p.set_bool(false);
    let Ok(Step::Done(m)) = p.advance() else {
        panic!("done");
    };
    assert_eq!(
        Value::Object(m),
        json!({"title": "Hi", "when": "2026-02-28", "flag": false}),
        "empty optional omitted; bool is JSON bool"
    );
    p.back();
    assert_eq!(p.step, 3);
    assert!(valid_date("2024-02-29") && !valid_date("2023-02-29"));
}

// ---- pure: chords ----

#[test]
fn chord_grammar() {
    let c = chord::parse("Ctrl+Shift+K").expect("parse");
    assert_eq!(
        c,
        Chord {
            command: true,
            shift: true,
            alt: false,
            key: "k".into()
        }
    );
    assert_eq!(chord::parse("mod+shift+k"), Some(c.clone()));
    assert_eq!(chord::parse("CMD + SHIFT + K"), Some(c.clone()));
    assert_eq!(c.to_string(), "Ctrl+Shift+K");
    assert_eq!(chord::parse("Alt+F5").map(|c| c.key), Some("f5".into()));
    assert_eq!(
        chord::parse("ctrl+esc").map(|c| c.key),
        Some("escape".into())
    );
    assert_eq!(chord::parse("Hyper+K"), None);
    assert_eq!(chord::parse("Ctrl+"), None);
    assert_eq!(chord::parse("Ctrl+Blah"), None);
    assert!(chord::reserved(&chord::parse("ctrl+s").expect("p")));
    assert!(!chord::reserved(&c));
    let ev = chord::from_event(
        &iced::keyboard::Key::Character("K".into()),
        iced::keyboard::Modifiers::CTRL | iced::keyboard::Modifiers::SHIFT,
    );
    assert_eq!(ev, Some(c));
}

#[test]
fn bindings_report_conflicts() {
    let acts = vec![
        action("one", vec![], &["Ctrl+Shift+K", "Ctrl+S", "Nope+Q"]),
        action("two", vec![], &["ctrl+shift+k", "Alt+J"]),
    ];
    let (map, notes) = bindings(&acts);
    assert_eq!(
        map.get(&chord::parse("Ctrl+Shift+K").expect("p")),
        Some(&"one".to_owned())
    );
    assert_eq!(
        map.get(&chord::parse("Alt+J").expect("p")),
        Some(&"two".to_owned())
    );
    assert_eq!(map.len(), 2);
    assert_eq!(notes.len(), 3, "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("editor default")));
    assert!(notes.iter().any(|n| n.contains("already bound to one")));
}

// ---- pure: ranking / merge ----

#[test]
fn palette_merges_and_ranks_with_keys() {
    let acts = vec![action("cap.inbox", vec![], &["Ctrl+Shift+I"])];
    let entries = build_entries(&acts);
    let top = |q: &str| entries[rank_entries(q, &entries)[0]].clone();
    assert_eq!(top("reading").id, EntryId::Client(ClientCmd::CycleReading));
    assert_eq!(top("reading").keys, vec!["Ctrl+E".to_owned()]);
    let a = top("title cap");
    assert_eq!(a.id, EntryId::Action("cap.inbox".into()));
    assert_eq!(a.keys, vec!["Ctrl+Shift+I".to_owned()]);
    assert_eq!(a.group, "Capture");
    let r = rank_entries("capture", &entries);
    assert_eq!(
        entries[r[0]].id,
        EntryId::Action("cap.inbox".into()),
        "group matches"
    );
    assert_eq!(rank_entries("", &entries).len(), entries.len());
    assert!(rank_entries("qqqzzz", &entries).is_empty());
}

// ---- FakeDaemon ----

type Calls = Arc<Mutex<Vec<(RunActionParams, bool)>>>;

struct Env {
    _tmp: tempfile::TempDir,
    d: Arc<FakeDaemon>,
    app: App,
    calls: Calls,
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(p, body).expect("write");
}

fn env() -> Env {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    write(&vault, "a.md", "# A\n\nfirst line\n");
    write(&vault, "b.md", "# B\n");
    let d = Arc::new(FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake"));
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let mut e = spec("kind", ParamKind::Enum, true);
    e.choices = vec!["x".into(), "y".into()];
    let prompted = action(
        "test.prompted",
        vec![
            spec("title", ParamKind::Text, true),
            spec("flag", ParamKind::Bool, false),
            spec("when", ParamKind::Date, false),
            e,
        ],
        &["Ctrl+Shift+K", "Ctrl+S"],
    );
    let plain = action("test.plain", vec![], &["Alt+J"]);
    for info in [prompted, plain] {
        let c = Arc::clone(&calls);
        let root = vault.clone();
        d.register_action(
            info,
            Arc::new(move |p: &RunActionParams| {
                // Does the context hash match the file on disk right now?
                let fresh = p.context.active_path.as_ref().is_some_and(|ap| {
                    std::fs::read(root.join(ap)).is_ok_and(|b| {
                        p.context.buffer_hash.as_deref() == Some(ContentHash::of(&b).as_str())
                    })
                });
                if let Ok(mut g) = c.lock() {
                    g.push((p.clone(), fresh));
                }
                Ok(Some("b.md".into()))
            }),
        )
        .expect("register");
    }
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.rpc_inline = true;
    app.today = Some(today());
    let _ = app.update(Message::Tabs(TabsMsg::Open("a.md".into())));
    Env {
        _tmp: tmp,
        d,
        app,
        calls,
    }
}

fn pal(app: &mut App, m: PaletteMsg) {
    let _ = app.update(Message::Palette(m));
}

fn type_z(app: &mut App) {
    let _ = app.update(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Insert('Z'),
    ))));
    assert!(app.active_tab().is_some_and(|t| t.source.is_dirty()));
}

fn entry_of(app: &App, id: &str) -> usize {
    app.palette
        .entries
        .iter()
        .position(|e| e.id == EntryId::Action(id.into()))
        .expect("entry")
}

fn calls(e: &Env) -> Vec<(RunActionParams, bool)> {
    e.calls.lock().expect("lock").clone()
}

#[test]
fn palette_prompts_saves_first_and_runs_with_context() {
    let mut e = env();
    type_z(&mut e.app);
    pal(&mut e.app, PaletteMsg::Open);
    assert!(e.app.palette.open && e.app.palette.actions.is_some());
    assert!(
        e.app.palette.notice.iter().any(|n| n.contains("Ctrl+S")),
        "conflict with an editor default is reported"
    );
    let i = entry_of(&e.app, "test.prompted");
    pal(&mut e.app, PaletteMsg::Pick(i));
    assert!(e.app.palette.prompt.is_some());
    pal(&mut e.app, PaletteMsg::Submit);
    assert!(e.app.palette.prompt_error.is_some(), "required blocks Next");
    pal(
        &mut e.app,
        PaletteMsg::Prompt(PromptMsg::Text("Hello".into())),
    );
    pal(&mut e.app, PaletteMsg::Submit);
    pal(&mut e.app, PaletteMsg::Prompt(PromptMsg::Bool(true)));
    pal(&mut e.app, PaletteMsg::Submit);
    pal(&mut e.app, PaletteMsg::Submit); // date: today
    pal(
        &mut e.app,
        PaletteMsg::Prompt(PromptMsg::Choice("y".into())),
    );
    assert!(calls(&e).is_empty());
    pal(&mut e.app, PaletteMsg::Submit);

    let got = calls(&e);
    assert_eq!(got.len(), 1);
    let (p, fresh) = &got[0];
    assert_eq!(
        Value::Object(p.params.clone()),
        json!({"title": "Hello", "flag": true, "when": "2026-10-13", "kind": "y"})
    );
    assert_eq!(p.context.active_path.as_deref(), Some("a.md"));
    assert_eq!(p.context.line, Some(1));
    assert!(*fresh, "the action saw the post-save hash");
    let disk = std::fs::read_to_string(e.d.vault_root().join("a.md")).expect("read");
    assert!(disk.starts_with('Z'), "dirty buffer saved first");
    assert!(!e.app.palette.open && e.app.palette.prompt.is_none());
    assert_eq!(
        e.app.active_tab().map(|t| t.path.as_str()),
        Some("b.md"),
        "open_path opened in the focused pane"
    );

    // The toast picks up the action's title and expires.
    let job = e
        .app
        .palette
        .job_titles
        .keys()
        .next()
        .cloned()
        .expect("job");
    let _ = e.app.update(Message::Status(StatusMsg::Notification(
        Notification::JobUpdated(JobStatus {
            job_id: job,
            status: JobState::Done,
            message: None,
            output_paths: Vec::new(),
            open_path: None,
        }),
    )));
    let toast = e.app.palette.toast.clone().expect("toast");
    assert_eq!(toast.line(), "Title test.prompted: done");
    e.app.palette.expire_toast(Instant::now());
    assert!(e.app.palette.toast.is_some());
    e.app
        .palette
        .expire_toast(Instant::now() + Duration::from_secs(6));
    assert!(e.app.palette.toast.is_none());
}

#[test]
fn save_conflict_aborts_the_run() {
    let mut e = env();
    type_z(&mut e.app);
    let (_, h) = e.d.read_file("a.md").expect("read");
    e.d.write_note("a.md", "changed elsewhere\n", &h)
        .expect("external write");
    pal(&mut e.app, PaletteMsg::Open);
    let i = entry_of(&e.app, "test.plain");
    pal(&mut e.app, PaletteMsg::Pick(i));
    assert!(calls(&e).is_empty(), "never dispatched with a stale hash");
    assert!(
        e.app
            .status
            .error
            .as_deref()
            .is_some_and(|s| s.contains("conflict")),
        "{:?}",
        e.app.status.error
    );
}

#[test]
fn shadow_skips_save_first() {
    let mut e = env();
    type_z(&mut e.app);
    let _ = e.app.update(Message::Status(StatusMsg::Notification(
        Notification::ModeChanged { mode: Mode::Shadow },
    )));
    pal(
        &mut e.app,
        PaletteMsg::Key(chord::parse("Alt+J").expect("p")),
    );
    let got = calls(&e);
    assert_eq!(
        got.len(),
        1,
        "chord invoked the action (actions fetched on demand)"
    );
    assert!(
        got[0].1,
        "context carries the base hash (still the on-disk one)"
    );
    assert!(
        e.app
            .panes
            .iter()
            .flat_map(|(_, ps)| ps.tabs.iter())
            .any(|t| t.path == "a.md" && t.source.is_dirty()),
        "buffer left dirty"
    );
    let disk = std::fs::read_to_string(e.d.vault_root().join("a.md")).expect("read");
    assert_eq!(disk, "# A\n\nfirst line\n", "no save in shadow");
}

#[test]
fn chord_opens_the_prompt_and_esc_cancels() {
    let mut e = env();
    pal(
        &mut e.app,
        PaletteMsg::Key(chord::parse("ctrl+shift+k").expect("p")),
    );
    assert!(e.app.palette.prompt.is_some(), "params prompt included");
    pal(&mut e.app, PaletteMsg::Close);
    assert!(e.app.palette.prompt.is_none() && !e.app.palette.open);
    // A reserved chord never reaches an action.
    pal(
        &mut e.app,
        PaletteMsg::Key(chord::parse("Ctrl+S").expect("p")),
    );
    assert!(e.app.palette.prompt.is_none());
    assert!(calls(&e).is_empty());
}

#[test]
fn client_command_runs_from_palette() {
    let mut e = env();
    pal(&mut e.app, PaletteMsg::Open);
    let i = e
        .app
        .palette
        .entries
        .iter()
        .position(|x| x.id == EntryId::Client(ClientCmd::SplitRight))
        .expect("entry");
    let panes = e.app.panes.len();
    pal(&mut e.app, PaletteMsg::Pick(i));
    assert_eq!(e.app.panes.len(), panes + 1);
    assert!(!e.app.palette.open);
}

// ---- editor-usability-round WP-5: durable "File as kind" follows the note (audit B1, M2) ----

fn durable_move_env() -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    write(&vault, "a.md", "# A\n\nfirst line\n");
    let d = Arc::new(FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake"));
    let mut info = action("file-as-kind", vec![], &[]);
    info.durable = true;
    let d2 = Arc::clone(&d);
    d.register_action(
        info,
        Arc::new(move |p: &RunActionParams| {
            let from = p.context.active_path.clone().ok_or("no path")?;
            d2.rename(&from, "kinds/a.md").map_err(|e| e.to_string())?;
            Ok(Some("kinds/a.md".into()))
        }),
    )
    .expect("register");
    d.defer_action("file-as-kind").expect("defer");
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.rpc_inline = true;
    app.today = Some(today());
    let _ = app.update(Message::Tabs(TabsMsg::Open("a.md".into())));
    (tmp, d, app)
}

fn run_file_as_kind(app: &mut App) {
    pal(app, PaletteMsg::Open);
    let i = entry_of(app, "file-as-kind");
    pal(app, PaletteMsg::Pick(i));
}

fn deliver_terminal(d: &FakeDaemon, app: &mut App) {
    for id in d.complete_deferred().expect("complete") {
        let j = d.job(&id).expect("job");
        assert_eq!(j.status, JobState::Done);
        let _ = app.update(Message::Status(StatusMsg::Notification(
            Notification::JobUpdated(j),
        )));
    }
}

fn all_tabs(app: &App) -> Vec<String> {
    app.panes
        .iter()
        .flat_map(|(_, ps)| ps.tabs.iter().map(|t| t.path.clone()))
        .collect()
}

#[test]
fn durable_file_as_kind_retargets_the_tab_on_terminal_status() {
    let (_tmp, d, mut app) = durable_move_env();
    run_file_as_kind(&mut app);
    assert_eq!(
        all_tabs(&app),
        ["a.md"],
        "nothing opens before the job ends"
    );
    deliver_terminal(&d, &mut app);
    assert_eq!(
        all_tabs(&app),
        ["kinds/a.md"],
        "exactly one tab, at the new path"
    );
    let tab = app.active_tab().expect("tab");
    assert!(!tab.source.is_dirty() && tab.source.conflict().is_none());
    assert_eq!(tab.source.path(), "kinds/a.md");
}

#[test]
fn durable_file_as_kind_keeps_edits_made_during_the_run_as_a_conflict() {
    let (_tmp, d, mut app) = durable_move_env();
    run_file_as_kind(&mut app);
    type_z(&mut app);
    let text = app.active_tab().expect("tab").source.text();
    deliver_terminal(&d, &mut app);
    assert_eq!(all_tabs(&app), ["kinds/a.md"]);
    let tab = app.active_tab().expect("tab");
    assert_eq!(tab.source.text(), text, "edits are not replaced");
    assert!(tab.source.is_dirty() && tab.source.conflict().is_some());
    assert_eq!(
        tab.source.path(),
        "kinds/a.md",
        "Keep mine saves at the new path"
    );
    let disk = std::fs::read_to_string(d.vault_root().join("kinds/a.md")).expect("read");
    assert_eq!(disk, "# A\n\nfirst line\n", "nothing written yet");
}

#[test]
fn terminal_status_before_the_reply_still_follows_and_original_kept_opens_new_tab() {
    // The original path still exists (a copy-style action): open_path opens as a new tab.
    let (_tmp, d, mut app) = durable_move_env();
    let vault = d.vault_root().to_path_buf();
    write(&vault, "kinds/b.md", "# B\n");
    let j = JobStatus {
        job_id: "early".into(),
        status: JobState::Done,
        message: None,
        output_paths: Vec::new(),
        open_path: Some("kinds/b.md".into()),
    };
    let _ = app.update(Message::Status(StatusMsg::Notification(
        Notification::JobUpdated(j),
    )));
    assert_eq!(
        all_tabs(&app),
        ["a.md"],
        "unknown job: remembered, not opened"
    );
    pal(
        &mut app,
        PaletteMsg::Ran {
            title: "t".into(),
            origin: Some(("a.md".into(), 0)),
            result: Ok(cv_protocol::RunActionResult {
                job_id: "early".into(),
                open_path: None,
            }),
        },
    );
    let mut tabs = all_tabs(&app);
    tabs.sort();
    assert_eq!(tabs, ["a.md", "kinds/b.md"]);
}
