//! WP-5 palette tests: pure prompt/chord/ranking logic, and invocation against `FakeDaemon`.
//! Synthetic vaults only (I1).

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use cv_protocol::{
    ActionInfo, JobState, JobStatus, Notification, ParamKind, ParamSpec, RunActionParams,
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
        needs: vec![cv_protocol::ContextNeed::ActivePath],
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
    let km = resolve(&std::collections::BTreeMap::new(), &acts);
    let (map, notes) = (km.actions(), km.notices);
    // Plan editor-feedback-round-2 WP-1: a chord claimed by two actions is bound to neither.
    assert_eq!(map.get(&chord::parse("Ctrl+Shift+K").expect("p")), None);
    assert_eq!(
        map.get(&chord::parse("Alt+J").expect("p")),
        Some(&"two".to_owned())
    );
    assert_eq!(map.len(), 1);
    assert_eq!(notes.len(), 4, "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("Ctrl+S is reserved")));
    assert!(
        notes
            .iter()
            .any(|n| n == "one: Ctrl+Shift+K conflicts with two (not bound)")
    );
    assert!(
        notes
            .iter()
            .any(|n| n == "two: Ctrl+Shift+K conflicts with one (not bound)")
    );
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
    app.queue_rpcs();
    app.today = Some(today());
    let _ = app.send(Message::Tabs(TabsMsg::Open("a.md".into())));
    Env {
        _tmp: tmp,
        d,
        app,
        calls,
    }
}

fn pal(app: &mut App, m: PaletteMsg) {
    let _ = app.send(Message::Palette(m));
}

fn type_z(app: &mut App) {
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Insert('Z'),
    ))));
    assert!(app.active_doc().is_some_and(|t| t.is_dirty()));
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
        e.app.active_tab().and_then(|t| t.path()),
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
    let _ = e.app.send(Message::Status(StatusMsg::Notification(
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
            .error_text()
            .is_some_and(|s| s.contains("conflict")),
        "{:?}",
        e.app.status.notice
    );
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
    app.queue_rpcs();
    app.today = Some(today());
    let _ = app.send(Message::Tabs(TabsMsg::Open("a.md".into())));
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
        let _ = app.send(Message::Status(StatusMsg::Notification(
            Notification::JobUpdated(j),
        )));
    }
}

fn all_tabs(app: &App) -> Vec<String> {
    app.panes
        .iter()
        .flat_map(|(_, ps)| ps.tabs.iter().filter_map(|t| t.path().map(str::to_owned)))
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
    let tab = app.active_doc().expect("tab");
    assert!(!tab.is_dirty() && tab.conflict().is_none());
    assert_eq!(tab.path(), "kinds/a.md");
}

/// editor-feedback-round-2 WP-8: a `Done` job whose `open_path` is the moved note retargets
/// every tab of the origin (duplicates included), not just the invoking one.
#[test]
fn durable_file_as_kind_retargets_every_tab_of_the_origin() {
    let (_tmp, d, mut app) = durable_move_env();
    let _ = app.send(Message::Panes(crate::message::PanesMsg::SplitRight));
    app.prefs.quick_switch_open = crate::header::QuickSwitchOpen::Duplicate;
    let _ = app.send(Message::Switcher(crate::message::SwitcherMsg::Pick(
        "a.md".into(),
    )));
    assert_eq!(all_tabs(&app), ["a.md", "a.md"]);
    run_file_as_kind(&mut app);
    deliver_terminal(&d, &mut app);
    assert_eq!(all_tabs(&app), ["kinds/a.md", "kinds/a.md"]);
    assert!(app.doc("a.md").is_none());
    assert_eq!(app.doc("kinds/a.md").map(|d| d.path()), Some("kinds/a.md"));
}

#[test]
fn durable_file_as_kind_keeps_edits_made_during_the_run_as_a_conflict() {
    let (_tmp, d, mut app) = durable_move_env();
    run_file_as_kind(&mut app);
    type_z(&mut app);
    let text = app.active_doc().expect("tab").text();
    deliver_terminal(&d, &mut app);
    assert_eq!(all_tabs(&app), ["kinds/a.md"]);
    let tab = app.active_doc().expect("tab");
    assert_eq!(tab.text(), text, "edits are not replaced");
    assert!(tab.is_dirty() && tab.conflict().is_some());
    assert_eq!(tab.path(), "kinds/a.md", "Keep mine saves at the new path");
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
    let _ = app.send(Message::Status(StatusMsg::Notification(
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

// --- WP-8: palette hints -----------------------------------------------------------------

fn titled(id: &str, title: &str, group: &str) -> ActionInfo {
    let mut a = action(id, Vec::new(), &[]);
    a.title = title.into();
    a.group = Some(group.into());
    a
}

/// Whether `q` ranks `i` strictly first (rank_entries' order, plus no equal-score runner-up).
fn uniquely_first(q: &str, i: usize, entries: &[Entry]) -> bool {
    let hays = haystacks(entries);
    let r = editor_model::rank::rank(
        q,
        &hays,
        editor_model::rank::RankMode::Plain,
        &[],
        |_| false,
        2,
    );
    rank_entries(q, entries).first() == Some(&i) && (r.len() < 2 || r[0].score > r[1].score)
}

fn real_plus_synthetic() -> Vec<Entry> {
    let words = [
        "Capture",
        "Ingest",
        "Summarize",
        "Translate",
        "Archive",
        "Tag",
        "Link",
        "Export",
        "Review",
        "Draft",
    ];
    let objs = ["note", "clipping", "fanfic", "daily", "inbox"];
    let mut actions = vec![titled(PERIOD_OPEN, "Open period note", "Periodic")];
    for (n, (w, o)) in words
        .iter()
        .flat_map(|w| objs.iter().map(move |o| (w, o)))
        .enumerate()
    {
        actions.push(titled(
            &format!("syn.{n}"),
            &format!("{w} {o}"),
            "Pipelines",
        ));
    }
    build_entries(&actions)
}

#[test]
fn hints_rank_their_entry_uniquely_first() {
    let entries = real_plus_synthetic();
    let hints = hints::compute(&entries);
    assert_eq!(hints.len(), entries.len());
    let mut with = 0;
    for (i, h) in hints.iter().enumerate() {
        if let Some(h) = h {
            with += 1;
            assert!(h.chars().count() <= hints::MAX_LEN);
            assert!(
                uniquely_first(h, i, &entries),
                "{h:?} for {:?}",
                entries[i].title
            );
        }
    }
    assert!(
        with * 2 > entries.len(),
        "only {with}/{} have hints",
        entries.len()
    );
}

#[test]
fn hints_are_shortest_first() {
    let entries = real_plus_synthetic();
    let hints = hints::compute(&entries);
    for (i, h) in hints.iter().enumerate() {
        let Some(h) = h else { continue };
        let t: Vec<char> = entries[i].title.to_lowercase().chars().collect();
        let n = h.chars().count();
        if n == 1 {
            continue;
        }
        // No shorter alphanumeric subsequence of the title qualifies.
        let allowed: Vec<char> = t.iter().copied().filter(|c| c.is_alphanumeric()).collect();
        let mut shorter = std::collections::HashSet::new();
        let mut stack: Vec<(usize, String)> = vec![(0, String::new())];
        while let Some((start, s)) = stack.pop() {
            if !s.is_empty() {
                shorter.insert(s.clone());
            }
            if s.chars().count() == n - 1 || s.chars().count() >= 2 {
                continue; // bounded check: lengths 1..=min(n-1, 2)
            }
            for (k, c) in allowed.iter().enumerate().skip(start) {
                stack.push((k + 1, format!("{s}{c}")));
            }
        }
        for s in shorter {
            assert!(!uniquely_first(&s, i, &entries), "{s:?} beats {h:?}");
        }
    }
}

#[test]
fn colliding_entries_get_no_hint() {
    // Two identical titles in the same group: neither can be strictly first.
    let actions = vec![
        titled("a", "Same title", "G"),
        titled("b", "Same title", "G"),
        titled("c", "Other thing", "G"),
    ];
    let entries = build_entries(&actions);
    let hints = hints::compute(&entries);
    let n = entries.len();
    assert_eq!(hints[n - 3], None);
    assert_eq!(hints[n - 2], None);
    assert!(hints[n - 1].is_some());
}

#[test]
fn hints_recompute_on_entry_set_change() {
    let mut p = Palette::default();
    assert_eq!(p.hints.len(), p.entries.len());
    assert!(p.hints.iter().any(Option::is_some));
    p.set_actions(vec![titled("zz", "Zebra quokka", "Animals")]);
    assert_eq!(p.hints.len(), p.entries.len());
    let i = p.entries.len() - 1;
    let h = p.hints[i].clone().expect("hint for the new action");
    assert!(uniquely_first(&h, i, &p.entries));
    let first = p.entries[0].id.clone();
    let id = match first {
        EntryId::Client(c) => c.id(),
        EntryId::Action(a) => a,
        EntryId::Bookmark(p) => bookmark_id(&p),
    };
    p.set_hidden(vec![id]);
    assert_eq!(p.hints.len(), p.entries.len());
    assert_eq!(p.hints, hints::compute(&p.entries));
}

#[test]
fn hints_compute_time_is_bounded() {
    let entries = real_plus_synthetic();
    let t0 = Instant::now();
    let hints = hints::compute(&entries);
    let el = t0.elapsed();
    eprintln!(
        "WP-8 hints: {} entries, {} with hints, {:?} ({} build)",
        entries.len(),
        hints.iter().filter(|h| h.is_some()).count(),
        el,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    assert!(el < Duration::from_secs(5), "{el:?}");
}

// ---- usability-sprint-captures-links WP-6: "File as kind" repro through the real flow ----

/// The live `file-as-kind`: durable, one required enum param, `upsert_tags` (writes `from`) →
/// `move` → `lint`. Handler effects run on `complete_deferred`.
fn file_as_kind_live_env() -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    write(&vault, "a.md", "# A\n\nfirst line\n");
    let d = Arc::new(FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake"));
    let mut kind = spec("kind", ParamKind::Enum, true);
    kind.choices = vec!["transcript".into(), "blog".into()];
    let mut info = action("file-as-kind", vec![kind], &[]);
    info.durable = true;
    let d2 = Arc::clone(&d);
    d.register_action(
        info,
        Arc::new(move |p: &RunActionParams| {
            let from = p.context.active_path.clone().ok_or("no path")?;
            let kind = p.params.get("kind").and_then(Value::as_str).ok_or("kind")?;
            let to = format!("{kind}/a.md");
            let (bytes, h) = d2.read_file(&from).map_err(|e| e.to_string())?;
            let tagged = format!(
                "---\ntags: [{kind}]\n---\n{}",
                String::from_utf8_lossy(&bytes)
            );
            d2.write_note(&from, &tagged, &h)
                .map_err(|e| e.to_string())?;
            d2.rename(&from, &to).map_err(|e| e.to_string())?;
            Ok(Some(to))
        }),
    )
    .expect("register");
    d.defer_action("file-as-kind").expect("defer");
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    app.today = Some(today());
    let _ = app.send(Message::Tabs(TabsMsg::Open("a.md".into())));
    (tmp, d, app)
}

fn notify(app: &mut App, n: Notification) {
    let _ = app.send(Message::Status(StatusMsg::Notification(n)));
}

fn job_status(id: &str, status: JobState, open: Option<&str>) -> JobStatus {
    JobStatus {
        job_id: id.into(),
        status,
        message: None,
        output_paths: Vec::new(),
        open_path: open.map(str::to_owned),
    }
}

/// Picks the action, chooses `blog` in the enum prompt, submits; then replays the daemon's
/// notifications in its emit order: `Running` (published when the runner starts the job,
/// `cv/src/runner/mod.rs` `publish_status(.., Running ..)`), the tags write on `from`, the
/// rename, `Done` with `open_path`.
fn run_live_file_as_kind(d: &FakeDaemon, app: &mut App) {
    pal(app, PaletteMsg::Open);
    let i = entry_of(app, "file-as-kind");
    pal(app, PaletteMsg::Pick(i));
    assert!(app.palette.prompt.is_some(), "enum param prompted");
    pal(app, PaletteMsg::Prompt(PromptMsg::Choice("blog".into())));
    pal(app, PaletteMsg::Submit);
    let id = app
        .palette
        .job_titles
        .keys()
        .next()
        .cloned()
        .expect("reply handled");
    notify(
        app,
        Notification::JobUpdated(job_status(&id, JobState::Running, None)),
    );
    assert_eq!(
        d.complete_deferred().expect("complete"),
        std::slice::from_ref(&id)
    );
    let tagged = std::fs::read(d.vault_root().join("blog/a.md")).expect("moved");
    notify(
        app,
        Notification::FileChanged {
            path: "a.md".into(),
            kind: cv_protocol::ChangeKind::Modified,
            hash: Some(ContentHash::of(&tagged).as_str().to_owned()),
            origin: cv_protocol::Origin::Daemon,
        },
    );
    for (path, kind) in [
        ("blog/a.md", cv_protocol::ChangeKind::Renamed),
        ("a.md", cv_protocol::ChangeKind::Deleted),
    ] {
        notify(
            app,
            Notification::FileChanged {
                path: path.into(),
                kind,
                hash: None,
                origin: cv_protocol::Origin::Daemon,
            },
        );
    }
    notify(
        app,
        Notification::JobUpdated(job_status(&id, JobState::Done, Some("blog/a.md"))),
    );
}

#[test]
fn live_file_as_kind_moves_the_tab_clean_buffer() {
    let (_tmp, d, mut app) = file_as_kind_live_env();
    run_live_file_as_kind(&d, &mut app);
    assert_eq!(all_tabs(&app), ["blog/a.md"], "one tab, at the moved path");
    assert!(app.doc("a.md").is_none(), "no stale doc for from");
    assert!(!d.vault_root().join("a.md").exists(), "from not recreated");
    let tab = app.active_doc().expect("tab");
    assert!(!tab.is_dirty() && tab.conflict().is_none());
    assert!(tab.text().contains("tags: [blog]"));
}

#[test]
fn live_file_as_kind_moves_the_tab_dirty_buffer_saved_first() {
    let (_tmp, d, mut app) = file_as_kind_live_env();
    type_z(&mut app);
    run_live_file_as_kind(&d, &mut app);
    assert_eq!(all_tabs(&app), ["blog/a.md"], "one tab, at the moved path");
    assert!(!d.vault_root().join("a.md").exists(), "from not recreated");
    let tab = app.active_doc().expect("tab");
    assert!(!tab.is_dirty() && tab.conflict().is_none());
    assert!(tab.text().contains('Z'), "saved-first edit kept");
}

// ---- WP-6 Part B: "Move note…" ----

#[test]
fn move_folder_candidates_dedup_root_and_exclude_current() {
    use crate::switcher::{folder_candidates, move_target};
    let paths: Vec<String> = ["a.md", "x/b.md", "x/c.md", "x/y/z/d.md", "q/e.md"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(
        folder_candidates(&paths, "x/b.md"),
        ["/", "q", "x/y", "x/y/z"],
        "root shown, dedup, ancestors kept, current folder excluded"
    );
    assert_eq!(
        folder_candidates(&paths, "a.md"),
        ["q", "x", "x/y", "x/y/z"],
        "a root note has no root candidate"
    );
    assert_eq!(move_target("x/b.md", "/"), "b.md");
    assert_eq!(move_target("a.md", "x/y"), "x/y/a.md");
}

fn move_env() -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    write(&vault, "a.md", "# A\n");
    write(&vault, "notes/b.md", "# B\n");
    let d = Arc::new(FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake"));
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    app.today = Some(today());
    let _ = app.send(Message::Tabs(TabsMsg::Open("a.md".into())));
    (tmp, d, app)
}

fn move_to(app: &mut App, folder: &str) {
    let _ = app.send(ClientCmd::MoveNote.message(app.focus));
    assert_eq!(app.switcher.move_from.as_deref(), Some("a.md"));
    assert_eq!(app.switcher.folders, ["notes"]);
    let _ = app.send(Message::Switcher(crate::message::SwitcherMsg::Pick(
        folder.into(),
    )));
    assert!(!app.switcher.open && app.switcher.move_from.is_none());
}

#[test]
fn move_note_renames_and_the_tab_follows() {
    let (_tmp, d, mut app) = move_env();
    move_to(&mut app, "notes");
    assert_eq!(all_tabs(&app), ["notes/a.md"]);
    assert!(app.doc("a.md").is_none());
    assert_eq!(app.doc("notes/a.md").map(|d| d.path()), Some("notes/a.md"));
    assert!(d.vault_root().join("notes/a.md").exists());
    assert!(!d.vault_root().join("a.md").exists());
}

#[test]
fn move_note_collision_is_a_status_and_no_rename() {
    let (_tmp, d, mut app) = move_env();
    write(d.vault_root(), "notes/a.md", "# other\n");
    move_to(&mut app, "notes");
    assert_eq!(all_tabs(&app), ["a.md"]);
    assert!(d.vault_root().join("a.md").exists());
    assert!(
        app.status
            .error_text()
            .is_some_and(|e| e.contains("already exists"))
    );
}

#[test]
fn move_note_refuses_a_dirty_buffer() {
    let (_tmp, d, mut app) = move_env();
    type_z(&mut app);
    move_to(&mut app, "notes");
    assert_eq!(all_tabs(&app), ["a.md"]);
    assert!(d.vault_root().join("a.md").exists());
    assert!(
        app.status
            .error_text()
            .is_some_and(|e| e.contains("save the open note first"))
    );
}

// ---- editor-round-4 WP-1: modal keys ----

fn key_press(named: Named, modifiers: iced::keyboard::Modifiers) -> iced::keyboard::Event {
    iced::keyboard::Event::KeyPressed {
        key: iced::keyboard::Key::Named(named),
        modified_key: iced::keyboard::Key::Named(named),
        physical_key: iced::keyboard::key::Physical::Unidentified(
            iced::keyboard::key::NativeCode::Unidentified,
        ),
        location: iced::keyboard::Location::Standard,
        modifiers,
        text: None,
        repeat: false,
    }
}

fn esc_event() -> iced::Event {
    iced::Event::Keyboard(key_press(Named::Escape, iced::keyboard::Modifiers::empty()))
}

/// A captured Escape (the focused text input took it) routes to the modal; an ignored one
/// does not (it already reaches `key_event`), so one press is one stage.
#[test]
fn captured_escape_routes_to_palette_and_switcher_once() {
    use iced::event::Status;
    let w = iced::window::Id::unique();
    assert!(matches!(
        captured_escape(esc_event(), Status::Captured, w),
        Some(Message::Palette(PaletteMsg::Escape))
    ));
    assert!(captured_escape(esc_event(), Status::Ignored, w).is_none());
    assert!(matches!(
        crate::switcher::captured_escape(esc_event(), Status::Captured, w),
        Some(Message::Switcher(crate::message::SwitcherMsg::Escape))
    ));
    assert!(crate::switcher::captured_escape(esc_event(), Status::Ignored, w).is_none());
    let enter = iced::Event::Keyboard(key_press(Named::Enter, iced::keyboard::Modifiers::empty()));
    assert!(captured_escape(enter, Status::Captured, w).is_none());
}

/// Plain Enter maps to `Enter`; Ctrl+Enter stays `CtrlEnter`.
#[test]
fn plain_enter_key_maps_to_enter() {
    assert!(matches!(
        key_event(key_press(Named::Enter, iced::keyboard::Modifiers::empty())),
        Some(Message::Palette(PaletteMsg::Enter))
    ));
    assert!(matches!(
        key_event(key_press(Named::Enter, iced::keyboard::Modifiers::CTRL)),
        Some(Message::Palette(PaletteMsg::CtrlEnter))
    ));
}

/// Enter on the File-as-kind enum step submits (runs the action once).
#[test]
fn enter_submits_enum_step() {
    let (_tmp, _d, mut app) = file_as_kind_live_env();
    pal(&mut app, PaletteMsg::Open);
    let i = entry_of(&app, "file-as-kind");
    pal(&mut app, PaletteMsg::Pick(i));
    assert!(app.palette.prompt.is_some());
    pal(
        &mut app,
        PaletteMsg::Prompt(PromptMsg::Choice("blog".into())),
    );
    pal(&mut app, PaletteMsg::Enter);
    assert!(app.palette.prompt.is_none(), "enum step submitted");
    assert_eq!(app.palette.job_titles.len(), 1, "action ran once");
}

/// Enter on a text step is a no-op (its input's `on_submit` owns Enter): no double submit.
#[test]
fn enter_on_text_step_does_not_submit() {
    let (_tmp, _d, mut app) = file_as_kind_live_env();
    let mut p = Prompt::new(
        action("t", vec![spec("name", ParamKind::Text, true)], &[]),
        today(),
    );
    p.set_text("x".into());
    app.palette.open = true;
    app.palette.prompt = Some(p);
    pal(&mut app, PaletteMsg::Enter);
    let pr = app.palette.prompt.as_ref().expect("still prompting");
    assert_eq!(pr.step, 0);
    assert!(app.palette.prompt_error.is_none());
}

/// Plan round-5 WP-3: a Failed job shows its step error in the toast and raises an Error notice
/// naming the action (the File as kind collision reaches the bar this way).
#[test]
fn failed_job_message_reaches_toast_and_notice() {
    let mut app = App::new(None, None);
    app.palette
        .job_titles
        .insert("j1".into(), "File as kind".into());
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::JobUpdated(JobStatus {
            job_id: "j1".into(),
            status: JobState::Failed,
            message: Some("already exists: stack/clip.md".into()),
            output_paths: Vec::new(),
            open_path: None,
        }),
    )));
    let toast = app.palette.toast.clone().expect("toast");
    assert_eq!(
        toast.line(),
        "File as kind: failed — already exists: stack/clip.md"
    );
    assert_eq!(
        app.status.error_text(),
        Some("File as kind failed: already exists: stack/clip.md")
    );
}

/// Plan round-5 WP-3: notices expire by severity (info 5 s < error 30 s); a lower-severity notice
/// does not displace a live error; the inline text is one truncated line.
#[test]
fn notice_expiry_and_severity_ordering() {
    use crate::app::{NOTICE_INLINE_CHARS, Severity};
    let mut app = App::new(None, None);
    let t0 = Instant::now();
    app.status.notify_at(Severity::Info, "hello", t0);
    app.status.expire_notice(t0 + Duration::from_secs(4));
    assert!(app.status.notice.is_some());
    app.status.expire_notice(t0 + Duration::from_secs(5));
    assert!(app.status.notice.is_none());
    app.status.notify_at(Severity::Error, "bad\nthing", t0);
    app.status
        .notify_at(Severity::Warning, "meh", t0 + Duration::from_secs(1));
    assert_eq!(app.status.error_text(), Some("bad\nthing"));
    assert_eq!(
        app.status.notice.as_ref().map(|n| n.inline()),
        Some("bad thing".into())
    );
    app.status.expire_notice(t0 + Duration::from_secs(29));
    assert!(app.status.notice.is_some());
    app.status.notify_at(
        Severity::Warning,
        "x".repeat(200),
        t0 + Duration::from_secs(31),
    );
    let n = app.status.notice.clone().expect("replaced after expiry");
    assert_eq!(n.severity, Severity::Warning);
    assert_eq!(n.inline().chars().count(), NOTICE_INLINE_CHARS);
    assert!(n.inline().ends_with('…'));
}
