//! Plan round-5 WP-8: link context menu and selection-taking commands (against `FakeDaemon`).

use std::sync::{Arc, Mutex};

use cv_protocol::{ActionInfo, RunActionParams};
use iced::widget::text_editor::Action;

use super::*;
use crate::app::App;
use crate::context_menu::{Item, MenuMsg, Target, items};
use crate::daemon::FakeDaemon;
use crate::message::{Message, ReadingMsg, SourceMsg, TabsMsg};
use crate::widget::selectable::SelPos;

const AT: iced::Point = iced::Point::new(10.0, 10.0);

fn setup(files: &[(&str, &[u8])]) -> (tempfile::TempDir, Arc<FakeDaemon>) {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    for (p, body) in files {
        let abs = vault.join(p);
        std::fs::create_dir_all(abs.parent().expect("parent")).expect("mkdir");
        std::fs::write(abs, body).expect("write");
    }
    let d = FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake");
    (tmp, Arc::new(d))
}

fn app_on(d: &Arc<FakeDaemon>, path: &str) -> App {
    let dc: Arc<dyn DaemonClient> = Arc::clone(d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    let _ = app.send(Message::Tabs(TabsMsg::Open(path.to_owned())));
    assert_eq!(app.active_tab().and_then(|t| t.path()), Some(path));
    app
}

fn set_mode(app: &mut App, path: &str, mode: ViewMode) {
    let _ = app.send(Message::Reading(ReadingMsg::SetMode {
        path: path.to_owned(),
        mode,
    }));
}

fn link(target: &str, external: bool, selection: bool) -> Target {
    Target::Link {
        src: "A.md".into(),
        target: target.into(),
        external,
        view: 1,
        selection,
        missing: false,
    }
}

fn choose(app: &mut App, target: Target, item: Item) {
    let _ = app.send(Message::Menu(MenuMsg::Open(target, AT)));
    let _ = app.send(Message::Menu(MenuMsg::Choose(item)));
}

fn select(app: &mut App, path: &str, view: u64, b: usize) {
    let _ = app.send(Message::Reading(ReadingMsg::Select {
        path: path.into(),
        view,
        sel: Some((SelPos::new(0, 0), SelPos::new(0, b))),
    }));
}

#[test]
fn link_and_selection_menu_items() {
    let ids = |t: &Target| items(t).into_iter().map(|(i, _)| i).collect::<Vec<_>>();
    assert_eq!(
        ids(&link("B", false, false)),
        [Item::Open, Item::OpenInNewPane, Item::CopyPath]
    );
    assert_eq!(
        ids(&link("https://x.y", true, false)),
        [Item::Open, Item::CopyUrl]
    );
    assert_eq!(
        ids(&link("B", false, true)),
        [Item::Copy, Item::Open, Item::OpenInNewPane, Item::CopyPath]
    );
    let sel = Target::Selection {
        path: "A.md".into(),
        view: 1,
    };
    assert_eq!(ids(&sel), [Item::Copy]);
}

#[test]
fn copy_path_resolves_or_notices() {
    let (_tmp, d) = setup(&[("A.md", b"see [[B]]\n"), ("sub/B.md", b"b\n")]);
    let mut app = app_on(&d, "A.md");
    choose(&mut app, link("B", false, false), Item::CopyPath);
    assert_eq!(app.copied(), ["sub/B.md"]);
    assert!(app.status.notice.is_none());
    choose(&mut app, link("Nope", false, false), Item::CopyPath);
    assert_eq!(app.copied(), ["sub/B.md"], "nothing copied");
    let n = app.status.notice.clone().expect("notice");
    assert_eq!(n.severity, crate::app::Severity::Warning);
    assert_eq!(n.text, "unresolved link: Nope");
}

#[test]
fn copy_url_and_selection_copy() {
    let (_tmp, d) = setup(&[("A.md", b"hello world\n")]);
    let mut app = app_on(&d, "A.md");
    set_mode(&mut app, "A.md", ViewMode::Reading);
    let view = app.active_tab().expect("tab").id;
    choose(&mut app, link("https://x.y/z", true, false), Item::CopyUrl);
    assert_eq!(app.copied(), ["https://x.y/z"]);
    select(&mut app, "A.md", view, 5);
    let want = app.reading.selection_text("A.md", view).expect("selection");
    choose(
        &mut app,
        Target::Selection {
            path: "A.md".into(),
            view,
        },
        Item::Copy,
    );
    assert_eq!(app.copied(), ["https://x.y/z".to_owned(), want]);
}

#[test]
fn selection_action_gets_reading_selection_and_split_recency() {
    let (_tmp, d) = setup(&[("A.md", b"hello world\n")]);
    let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let rec = Arc::clone(&seen);
    d.register_action(
        ActionInfo {
            id: "sel.act".into(),
            title: "Sel".into(),
            group: None,
            keys: vec![],
            params: vec![],
            needs: vec![cv_protocol::ContextNeed::Selection],
            durable: false,
        },
        Arc::new(move |p: &RunActionParams| {
            rec.lock().expect("lock").push(p.context.selection.clone());
            Ok(None)
        }),
    )
    .expect("register");
    let mut app = app_on(&d, "A.md");
    let call = || crate::palette::Call {
        action_id: "sel.act".into(),
        title: "Sel".into(),
        params: serde_json::Map::new(),
        needs: vec![cv_protocol::ContextNeed::Selection],
    };
    set_mode(&mut app, "A.md", ViewMode::Reading);
    let view = app.active_tab().expect("tab").id;
    select(&mut app, "A.md", view, 5);
    let rsel = app.reading.selection_text("A.md", view).expect("selection");
    let _ = app.run_call(call());
    // Split: a later Source selection wins; a later Reading selection wins back.
    set_mode(&mut app, "A.md", ViewMode::Split);
    let _ = app.send(Message::Source(SourceMsg::Action(Action::SelectAll)));
    let _ = app.run_call(call());
    select(&mut app, "A.md", view, 5);
    let _ = app.run_call(call());
    let got = seen.lock().expect("lock").clone();
    assert_eq!(
        got,
        [
            Some(rsel.clone()),
            Some("hello world\n".to_owned()),
            Some(rsel)
        ]
    );
}
