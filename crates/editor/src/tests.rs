//! WP-1 tests: `FakeDaemon` round trips, `SocketDaemon` against an in-process socket server,
//! and window-less `App::update` logic. Synthetic vaults only (I1).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cv_protocol::{
    ActionContext, ActionInfo, ChangeKind, HelloResult, JobState, Mode, Notification, Origin,
    Request, RpcNotification, RpcRequest, RunActionParams,
};
use cv_testkit::{SyntheticVault, VaultSpec};
use serde_json::{Value, json};

use crate::app::App;
use crate::daemon::{ALL_TOPICS, ContentHash, DaemonClient, DaemonError, FakeDaemon, SocketDaemon};
use crate::message::{Message, StatusMsg, TabsMsg};

const WAIT: Duration = Duration::from_secs(5);

fn fake() -> (tempfile::TempDir, FakeDaemon) {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    SyntheticVault::build(&vault, &VaultSpec::default()).expect("vault");
    let d = FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake");
    (tmp, d)
}

fn first_note(d: &FakeDaemon) -> String {
    d.index()
        .with_query(|q| q.folder_files("notes"))
        .expect("query")
        .into_iter()
        .find(|f| f.is_md)
        .expect("a note")
        .path
}

#[test]
fn fake_read_returns_bytes_and_daemon_hash() {
    let (_tmp, d) = fake();
    let p = first_note(&d);
    let (bytes, hash) = d.read_file(&p).expect("read");
    assert!(!bytes.is_empty());
    assert_eq!(hash, ContentHash::of(&bytes));
    assert!(matches!(
        d.read_file("../escape.md"),
        Err(DaemonError::InvalidPath)
    ));
    assert!(matches!(
        d.read_file("nope.md"),
        Err(DaemonError::NotFound(_))
    ));
}

#[test]
fn fake_write_cas_notifies_and_reindexes() {
    let (_tmp, d) = fake();
    let rx = d.subscribe(&ALL_TOPICS).expect("subscribe");
    let p = first_note(&d);
    let (_, base) = d.read_file(&p).expect("read");
    let gen0 = d.generation().expect("gen");
    let body = "# Fresh\n\nzanzibarqux #wp1tag\n";
    let new_hash = d.write_note(&p, body, &base).expect("write");
    assert_eq!(new_hash, ContentHash::of(body.as_bytes()));
    assert_eq!(d.read_file(&p).expect("reread").0, body.as_bytes());

    match rx.recv_timeout(WAIT).expect("file_changed") {
        Notification::FileChanged {
            path,
            kind,
            hash,
            origin,
        } => {
            assert_eq!(path, p);
            assert_eq!(kind, ChangeKind::Modified);
            assert_eq!(hash.as_deref(), Some(new_hash.as_str()));
            assert_eq!(origin, Origin::Daemon);
        }
        other => panic!("unexpected {other:?}"),
    }
    let n = match rx.recv_timeout(WAIT).expect("index_generation") {
        Notification::IndexGeneration { n } => n,
        other => panic!("unexpected {other:?}"),
    };
    assert!(n > gen0);

    // Stale base hash → Conflict carrying the on-disk hash.
    match d.write_note(&p, "lost", &base) {
        Err(DaemonError::Conflict { current_hash }) => assert_eq!(current_hash, new_hash),
        other => panic!("expected conflict, got {other:?}"),
    }

    // The index sees the write after reopening on the new generation.
    d.index().on_generation(n).expect("reopen");
    assert_eq!(d.index().generation(), Some(n));
    let tagged = d
        .index()
        .with_query(|q| q.tagged("wp1tag"))
        .expect("tagged");
    assert!(tagged.iter().any(|t| t.path == p));
    let hits = d
        .index()
        .with_query(|q| q.fts("zanzibarqux", 5))
        .expect("fts");
    assert_eq!(hits.len(), 1);
}

#[test]
fn fake_create_rename_delete() {
    let (_tmp, d) = fake();
    let p = d.create_note("new/one.md", "hello").expect("create");
    assert_eq!(p, "new/one.md");
    assert!(d.create_note("new/one.md", "again").is_err());
    d.rename("new/one.md", "new/two.md").expect("rename");
    assert!(d.read_file("new/two.md").is_ok());
    d.delete("new/two.md").expect("delete");
    assert!(matches!(
        d.delete("new/two.md"),
        Err(DaemonError::NotFound(_))
    ));
    assert!(
        d.state_reader()
            .expect("state")
            .counts()
            .expect("counts")
            .is_empty()
    );
}

#[test]
fn fake_scripted_action_runs() {
    let (_tmp, d) = fake();
    let rx = d.subscribe(&ALL_TOPICS).expect("subscribe");
    d.register_action(
        ActionInfo {
            id: "period.open".into(),
            title: "Open period".into(),
            group: None,
            keys: vec![],
            params: vec![],
            needs: vec![],
            durable: false,
        },
        Arc::new(|p: &RunActionParams| {
            p.params
                .get("date")
                .and_then(Value::as_str)
                .map(|d| Some(format!("daily/{d}.md")))
                .ok_or_else(|| "missing date".to_owned())
        }),
    )
    .expect("register");
    assert_eq!(d.actions_list().expect("list").len(), 1);
    let ok = d
        .run_action(RunActionParams {
            action_id: "period.open".into(),
            params: serde_json::Map::from_iter([("date".to_owned(), json!("2026-01-28"))]),
            context: ActionContext::default(),
        })
        .expect("run");
    assert_eq!(ok.open_path.as_deref(), Some("daily/2026-01-28.md"));
    assert_eq!(d.job(&ok.job_id).expect("job").status, JobState::Done);
    assert!(matches!(
        rx.recv_timeout(WAIT),
        Ok(Notification::JobUpdated(_))
    ));
    let failed = d
        .run_action(RunActionParams {
            action_id: "period.open".into(),
            params: serde_json::Map::new(),
            context: ActionContext::default(),
        })
        .expect("run");
    let job = d.job(&failed.job_id).expect("job");
    assert_eq!(job.status, JobState::Failed);
    assert_eq!(job.message.as_deref(), Some("missing date"));
    assert!(matches!(
        d.run_action(RunActionParams {
            action_id: "nope".into(),
            params: serde_json::Map::new(),
            context: ActionContext::default(),
        }),
        Err(DaemonError::NotFound(_))
    ));
}

// ---------------------------------------------------------------------------------------------
// SocketDaemon against an in-process server
// ---------------------------------------------------------------------------------------------

fn serve(sock: &Path, index_schema: u32) {
    let listener = UnixListener::bind(sock).expect("bind");
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let mut w = stream.try_clone().expect("clone");
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let Ok(line) = line else { return };
            let req: RpcRequest = serde_json::from_str(&line).expect("request");
            let id = serde_json::to_value(&req.id).expect("id");
            let result = match req.call {
                Request::Hello(_) => serde_json::to_value(HelloResult {
                    protocol_version: 1,
                    index_schema,
                    mode: Mode::Shadow,
                    vault_root: "/nonexistent-vault".into(),
                    index_db: "/nonexistent/index.db".into(),
                    state_db: "/nonexistent/state.db".into(),
                    generation: 7,
                })
                .expect("hello"),
                Request::Subscribe(s) => json!({ "topics": s.topics }),
                Request::Status => json!({ "mode": "shadow", "generation": 7 }),
                Request::WriteNote(_) => json!({ "conflict": { "current_hash": "abc" } }),
                _ => {
                    let e =
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"nope"}});
                    writeln!(w, "{e}").expect("write");
                    continue;
                }
            };
            // A notification interleaved before the response must not confuse the demux.
            if result.get("topics").is_some() {
                writeln!(w, "{}", json!({"jsonrpc":"2.0","id":id,"result":result})).expect("write");
                let n = RpcNotification::new(Notification::ModeChanged { mode: Mode::Live });
                writeln!(w, "{}", serde_json::to_string(&n).expect("n")).expect("write");
                continue;
            }
            writeln!(w, "{}", json!({"jsonrpc":"2.0","id":id,"result":result})).expect("write");
        }
    });
}

#[test]
fn socket_handshake_status_and_notification() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sock = tmp.path().join("d.sock");
    serve(&sock, cv_index::SCHEMA_VERSION);
    let d = SocketDaemon::connect(&sock).expect("connect");
    assert_eq!(d.hello().generation, 7);
    assert_eq!(d.vault_root(), Path::new("/nonexistent-vault"));
    let rx = d.subscribe(&ALL_TOPICS).expect("subscribe");
    assert_eq!(
        rx.recv_timeout(WAIT).expect("notification"),
        Notification::ModeChanged { mode: Mode::Live }
    );
    assert_eq!(d.status().expect("status").generation, 7);
    match d.write_note("a.md", "x", &ContentHash::from_hex("h")) {
        Err(DaemonError::Conflict { current_hash }) => assert_eq!(current_hash.as_str(), "abc"),
        other => panic!("expected conflict, got {other:?}"),
    }
    assert!(matches!(d.actions_list(), Err(DaemonError::Rpc(e)) if e.code == -32601));
}

#[test]
fn socket_schema_mismatch_is_refused() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sock = tmp.path().join("d.sock");
    serve(&sock, cv_index::SCHEMA_VERSION + 1);
    match SocketDaemon::connect(&sock) {
        Err(DaemonError::SchemaMismatch { expected, found }) => {
            assert_eq!(expected, cv_index::SCHEMA_VERSION);
            assert_eq!(found, cv_index::SCHEMA_VERSION + 1);
        }
        other => panic!("expected mismatch, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// App logic without a window
// ---------------------------------------------------------------------------------------------

#[test]
fn app_open_file_adds_tab_and_status_updates() {
    let (tmp, d) = fake();
    let p = first_note(&d);
    let expected = String::from_utf8(d.read_file(&p).expect("read").0).expect("utf8");
    let state = tmp.path().join("state/crucible-vault/editor.json");
    let d: Arc<dyn DaemonClient> = Arc::new(d);
    let mut app = App::new(Some(Arc::clone(&d)), Some(state.clone()));
    assert!(app.status.connected);
    assert_eq!(app.status.mode, Mode::Live);
    assert!(app.explorer.paths.contains(&p));

    let _ = app.update(Message::Tabs(TabsMsg::Open(p.clone())));
    assert_eq!(app.open_paths().len(), 1);
    assert_eq!(app.active_tab().map(|t| t.source.text()), Some(expected));
    let _ = app.update(Message::Tabs(TabsMsg::Open(p.clone())));
    assert_eq!(app.open_paths().len(), 1, "re-open focuses the tab");
    let _ = app.update(Message::Tabs(TabsMsg::Open("missing.md".into())));
    assert_eq!(app.open_paths().len(), 1);
    assert_eq!(app.status.error.as_deref(), Some("file not found"));

    let _ = app.update(Message::Status(StatusMsg::Notification(
        Notification::ModeChanged { mode: Mode::Shadow },
    )));
    assert_eq!(app.status.mode, Mode::Shadow);
    let base = d.read_file(&p).expect("read").1;
    d.write_note(&p, "changed\n", &base).expect("write");
    let g = d.status().expect("status").generation;
    let _ = app.update(Message::Status(StatusMsg::Notification(
        Notification::IndexGeneration { n: g },
    )));
    assert_eq!(app.status.generation, g);
    assert!(app.status.line().contains(&format!("gen {g}")));
    let _ = app.update(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            path: p.clone(),
            kind: ChangeKind::Modified,
            hash: None,
            origin: Origin::External,
        },
    )));
    assert_eq!(
        app.active_tab().map(|t| t.source.text()).as_deref(),
        Some("changed\n")
    );
    let _ = app.update(Message::Status(StatusMsg::Disconnected));
    assert!(app.status.line().starts_with("disconnected · shadow"));

    // Tabs persist and restore from editor.json.
    let restored = App::new(Some(d), Some(state));
    assert_eq!(restored.open_paths().len(), 1);
    assert_eq!(restored.ui.recent.first(), Some(&p));
}

// ---------------------------------------------------------------------------------------------
// WP-4: panes, explorer, switcher, bookmarks, nav (against FakeDaemon, RPC run inline)
// ---------------------------------------------------------------------------------------------

mod wp4 {
    use std::sync::Mutex;

    use chrono::NaiveDate;
    use editor_model::period::{NavTarget, Period};

    use super::*;
    use crate::bookmarks::{BOOKMARKS_PATH, BookmarkMsg};
    use crate::message::{ExplorerMsg, NavMsg, PanesMsg, SwitcherMsg};
    use crate::panes;

    fn app_over(d: &Arc<FakeDaemon>, state: Option<std::path::PathBuf>) -> App {
        let dc: Arc<dyn DaemonClient> = Arc::clone(d) as Arc<dyn DaemonClient>;
        let mut app = App::new(Some(dc), state);
        app.rpc_inline = true;
        app
    }

    fn fake_arc() -> (tempfile::TempDir, Arc<FakeDaemon>) {
        let (tmp, d) = fake();
        (tmp, Arc::new(d))
    }

    fn changed(app: &mut App, path: &str, kind: ChangeKind) {
        let _ = app.update(Message::Status(StatusMsg::Notification(
            Notification::FileChanged {
                path: path.to_owned(),
                kind,
                hash: None,
                origin: Origin::Daemon,
            },
        )));
    }

    fn shadow(app: &mut App) {
        let _ = app.update(Message::Status(StatusMsg::Notification(
            Notification::ModeChanged { mode: Mode::Shadow },
        )));
    }

    fn row_paths(app: &App) -> Vec<String> {
        app.explorer.rows.iter().map(|r| r.path.clone()).collect()
    }

    #[test]
    fn explorer_tree_build_and_incremental() {
        let (_tmp, d) = fake_arc();
        for p in [
            "wp4tree/Zeta/b.md",
            "wp4tree/alpha/c.md",
            "wp4tree/alpha/A.md",
            "wp4tree/B.md",
            "wp4tree/a.md",
        ] {
            d.create_note(p, "x").expect("create");
        }
        let mut app = app_over(&d, None);
        assert!(app.explorer.contains("wp4tree/alpha/A.md"));
        assert!(row_paths(&app).contains(&"wp4tree".to_owned()));
        assert!(
            !row_paths(&app).contains(&"wp4tree/a.md".to_owned()),
            "collapsed"
        );
        let _ = app.update(Message::Explorer(ExplorerMsg::Toggle("wp4tree".into())));
        let _ = app.update(Message::Explorer(ExplorerMsg::Toggle(
            "wp4tree/alpha".into(),
        )));
        let rows = row_paths(&app);
        let start = rows
            .iter()
            .position(|p| p == "wp4tree")
            .expect("folder row");
        assert_eq!(
            &rows[start + 1..start + 7],
            [
                "wp4tree/alpha",
                "wp4tree/alpha/A.md",
                "wp4tree/alpha/c.md",
                "wp4tree/Zeta",
                "wp4tree/a.md",
                "wp4tree/B.md",
            ],
            "folders first, case-insensitive"
        );
        assert!(app.ui.expanded.contains("wp4tree/alpha"));

        d.create_note("wp4tree/alpha/new.md", "n").expect("create");
        changed(&mut app, "wp4tree/alpha/new.md", ChangeKind::Created);
        assert!(row_paths(&app).contains(&"wp4tree/alpha/new.md".to_owned()));
        d.delete("wp4tree/a.md").expect("delete");
        changed(&mut app, "wp4tree/a.md", ChangeKind::Deleted);
        assert!(!app.explorer.contains("wp4tree/a.md"));
        assert!(!app.explorer.stale);

        // An external rename names only its destination: stale until the next generation.
        d.rename("wp4tree/B.md", "wp4tree/C.md").expect("rename");
        changed(&mut app, "wp4tree/C.md", ChangeKind::Renamed);
        assert!(app.explorer.stale);
        assert!(app.explorer.contains("wp4tree/B.md"));
        let g = d.generation().expect("gen");
        let _ = app.update(Message::Status(StatusMsg::Notification(
            Notification::IndexGeneration { n: g },
        )));
        assert!(!app.explorer.stale);
        assert!(!app.explorer.contains("wp4tree/B.md"));
        assert!(app.explorer.contains("wp4tree/C.md"));
    }

    #[test]
    fn explorer_rename_move_new_trash_via_rpc() {
        let (_tmp, d) = fake_arc();
        d.create_note("wp4ops/one.md", "body").expect("create");
        let mut app = app_over(&d, None);
        let _ = app.update(Message::Explorer(ExplorerMsg::Open("wp4ops/one.md".into())));
        assert_eq!(app.open_paths(), ["wp4ops/one.md"]);

        let _ = app.update(Message::Explorer(ExplorerMsg::BeginRename));
        assert_eq!(
            app.explorer.edit.as_ref().map(|e| e.input.as_str()),
            Some("one.md")
        );
        let _ = app.update(Message::Explorer(ExplorerMsg::Input("two.md".into())));
        let _ = app.update(Message::Explorer(ExplorerMsg::Submit));
        assert!(
            d.read_file("wp4ops/two.md").is_ok(),
            "renamed through the daemon"
        );
        assert!(app.explorer.contains("wp4ops/two.md"));
        assert!(!app.explorer.contains("wp4ops/one.md"));
        assert_eq!(app.open_paths(), ["wp4ops/two.md"], "the tab follows");
        changed(&mut app, "wp4ops/two.md", ChangeKind::Renamed);
        assert!(!app.explorer.stale, "own rename is matched");

        let _ = app.update(Message::Explorer(ExplorerMsg::BeginMove));
        let _ = app.update(Message::Explorer(ExplorerMsg::Input("wp4moved".into())));
        let _ = app.update(Message::Explorer(ExplorerMsg::Submit));
        assert!(app.explorer.contains("wp4moved/two.md"));
        assert_eq!(app.explorer.selected.as_deref(), Some("wp4moved/two.md"));

        let _ = app.update(Message::Explorer(ExplorerMsg::BeginNew));
        let _ = app.update(Message::Explorer(ExplorerMsg::Input("fresh".into())));
        let _ = app.update(Message::Explorer(ExplorerMsg::Submit));
        assert!(app.explorer.contains("wp4moved/fresh.md"));
        assert_eq!(
            app.active_tab().map(|t| t.path.as_str()),
            Some("wp4moved/fresh.md")
        );

        let _ = app.update(Message::Explorer(ExplorerMsg::Select(
            "wp4moved/two.md".into(),
        )));
        let _ = app.update(Message::Explorer(ExplorerMsg::Trash));
        assert!(matches!(
            d.read_file("wp4moved/two.md"),
            Err(DaemonError::NotFound(_))
        ));
        assert!(!app.explorer.contains("wp4moved/two.md"));
        assert!(!app.open_paths().contains(&"wp4moved/two.md".to_owned()));
        assert!(app.closed.contains(&"wp4moved/two.md".to_owned()));
    }

    #[test]
    fn rename_and_trash_refuse_a_dirty_open_note() {
        use iced::widget::text_editor::{Action, Edit};
        let (_tmp, d) = fake_arc();
        d.create_note("wp4dirty/one.md", "body").expect("create");
        let mut app = app_over(&d, None);
        let _ = app.update(Message::Explorer(ExplorerMsg::Open(
            "wp4dirty/one.md".into(),
        )));
        let _ = app.update(Message::Source(crate::message::SourceMsg::Action(
            Action::Edit(Edit::Insert('x')),
        )));
        assert!(app.active_tab().is_some_and(|t| t.source.is_dirty()));

        let _ = app.update(Message::Explorer(ExplorerMsg::BeginRename));
        let _ = app.update(Message::Explorer(ExplorerMsg::Input("two.md".into())));
        let _ = app.update(Message::Explorer(ExplorerMsg::Submit));
        assert!(d.read_file("wp4dirty/one.md").is_ok(), "not renamed");
        assert!(d.read_file("wp4dirty/two.md").is_err());
        assert!(
            app.active_tab().is_some_and(|t| t.source.is_dirty()),
            "edits kept"
        );

        let _ = app.update(Message::Explorer(ExplorerMsg::Select(
            "wp4dirty/one.md".into(),
        )));
        let _ = app.update(Message::Explorer(ExplorerMsg::Trash));
        assert!(d.read_file("wp4dirty/one.md").is_ok(), "not trashed");
        assert!(app.status.error.is_some());
    }

    #[test]
    fn shadow_mode_blocks_file_ops_but_not_navigation() {
        let (_tmp, d) = fake_arc();
        d.create_note("wp4sh/keep.md", "k").expect("create");
        let mut app = app_over(&d, None);
        shadow(&mut app);
        let _ = app.update(Message::Explorer(ExplorerMsg::Open("wp4sh/keep.md".into())));
        assert_eq!(app.open_paths(), ["wp4sh/keep.md"], "navigation works");
        let _ = app.update(Message::Explorer(ExplorerMsg::BeginNew));
        assert!(app.explorer.edit.is_none());
        assert_eq!(
            app.status.error.as_deref(),
            Some(crate::explorer::SHADOW_REASON)
        );
        let _ = app.update(Message::Explorer(ExplorerMsg::Trash));
        assert!(d.read_file("wp4sh/keep.md").is_ok());
        let _ = app.update(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::AddActive)));
        assert!(app.bookmarks.items.is_empty());
        assert!(d.read_file(BOOKMARKS_PATH).is_err());
        let _ = app.update(Message::Switcher(SwitcherMsg::Open));
        let _ = app.update(Message::Switcher(SwitcherMsg::Query(
            "nothing like it".into(),
        )));
        let _ = app.update(Message::Switcher(SwitcherMsg::Create));
        assert!(d.read_file("nothing like it.md").is_err());
    }

    fn register_create(d: &Arc<FakeDaemon>) {
        let inner = Arc::clone(d);
        d.register_action(
            ActionInfo {
                id: "note.create".into(),
                title: "Create note".into(),
                group: None,
                keys: vec![],
                params: vec![],
                needs: vec![],
                durable: false,
            },
            Arc::new(move |p: &RunActionParams| {
                let path = p
                    .params
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or("missing path")?;
                inner.create_note(path, "").map_err(|e| e.to_string())?;
                Ok(Some(path.to_owned()))
            }),
        )
        .expect("register");
    }

    #[test]
    fn switcher_ranks_opens_and_creates_on_miss() {
        let (_tmp, d) = fake_arc();
        register_create(&d);
        d.create_note("wp4sw/Works.md", "w").expect("create");
        let mut app = app_over(&d, None);
        let _ = app.update(Message::Switcher(SwitcherMsg::Open));
        assert!(app.switcher.open);
        let _ = app.update(Message::Switcher(SwitcherMsg::Query("works".into())));
        assert_eq!(app.switcher.current(), Some("wp4sw/Works.md"));
        let _ = app.update(Message::Switcher(SwitcherMsg::Submit));
        assert!(!app.switcher.open);
        assert_eq!(
            app.active_tab().map(|t| t.path.as_str()),
            Some("wp4sw/Works.md")
        );

        let _ = app.update(Message::Switcher(SwitcherMsg::Open));
        let _ = app.update(Message::Switcher(SwitcherMsg::Query(
            "qqzz brand idea".into(),
        )));
        assert!(app.switcher.results.is_empty());
        let _ = app.update(Message::Switcher(SwitcherMsg::Submit));
        assert!(d.read_file("qqzz brand idea.md").is_ok(), "note.create ran");
        assert_eq!(
            app.active_tab().map(|t| t.path.as_str()),
            Some("qqzz brand idea.md")
        );

        // Shift+Enter creates even when something matches.
        let _ = app.update(Message::Switcher(SwitcherMsg::Open));
        let _ = app.update(Message::Switcher(SwitcherMsg::Query("wp4sw/Works2".into())));
        let _ = app.update(Message::Switcher(SwitcherMsg::Modifiers(
            iced::keyboard::Modifiers::SHIFT,
        )));
        let _ = app.update(Message::Switcher(SwitcherMsg::Submit));
        assert!(d.read_file("wp4sw/Works2.md").is_ok());
    }

    #[test]
    fn bookmarks_load_add_and_conflict() {
        let (_tmp, d) = fake_arc();
        d.create_note("wp4bm/a.md", "a").expect("create");
        let mut app = app_over(&d, None);
        assert!(app.bookmarks.items.is_empty() && app.bookmarks.hash.is_none());
        let _ = app.update(Message::Tabs(TabsMsg::Open("wp4bm/a.md".into())));
        let _ = app.update(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::AddActive)));
        let (bytes, h1) = d.read_file(BOOKMARKS_PATH).expect("created");
        assert_eq!(
            crate::bookmarks::parse(&bytes).expect("parse"),
            [crate::bookmarks::Bookmark {
                name: "a".into(),
                path: "wp4bm/a.md".into()
            }]
        );
        assert_eq!(app.bookmarks.hash.as_ref(), Some(&h1));

        // Loading from an existing file.
        let fresh = app_over(&d, None);
        assert_eq!(fresh.bookmarks.items.len(), 1);

        // Someone else rewrites the file: the next edit hits a CAS conflict and reloads.
        let other = "[[bookmark]]\nname = \"x\"\npath = \"x.md\"\n\n[[bookmark]]\nname = \"y\"\npath = \"y.md\"\n";
        d.write_note(BOOKMARKS_PATH, other, &h1).expect("external");
        let _ = app.update(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::Remove(0))));
        assert!(
            app.status
                .error
                .as_deref()
                .is_some_and(|e| e.contains("changed on disk"))
        );
        let names: Vec<&str> = app
            .bookmarks
            .items
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(names, ["x", "y"]);
        assert!(!app.bookmarks.writing);
        let _ = app.update(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::Down(0))));
        let names: Vec<String> =
            crate::bookmarks::parse(&d.read_file(BOOKMARKS_PATH).expect("r").0)
                .expect("parse")
                .into_iter()
                .map(|b| b.name)
                .collect();
        assert_eq!(names, ["y", "x"]);
    }

    fn register_period(
        d: &Arc<FakeDaemon>,
        materialize: bool,
    ) -> Arc<Mutex<Vec<(String, String)>>> {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&calls);
        let inner = Arc::clone(d);
        d.register_action(
            ActionInfo {
                id: "period.open".into(),
                title: "Open period".into(),
                group: None,
                keys: vec![],
                params: vec![],
                needs: vec![],
                durable: false,
            },
            Arc::new(move |p: &RunActionParams| {
                let get = |k: &str| {
                    p.params
                        .get(k)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                };
                let (period, date) = (get("period"), get("date"));
                if let Ok(mut g) = seen.lock() {
                    g.push((period.clone(), date.clone()));
                }
                let path = if period == "month" {
                    format!("daily/month/{}.md", &date[..7])
                } else {
                    format!("daily/day/{date}.md")
                };
                if materialize && inner.read_file(&path).is_err() {
                    inner.create_note(&path, "").map_err(|e| e.to_string())?;
                }
                Ok(Some(path))
            }),
        )
        .expect("register");
        calls
    }

    #[test]
    fn nav_runs_period_open_and_opens_in_the_same_pane() {
        let (_tmp, d) = fake_arc();
        let calls = register_period(&d, true);
        d.create_note("daily/day/2026-12-31.md", "d")
            .expect("create");
        d.create_note("wp4nav/other.md", "o").expect("create");
        let mut app = app_over(&d, None);
        app.today = NaiveDate::from_ymd_opt(2026, 10, 13);
        let _ = app.update(Message::Tabs(TabsMsg::Open(
            "daily/day/2026-12-31.md".into(),
        )));
        let day_pane = app.focus;
        let _ = app.update(Message::Panes(PanesMsg::SplitRight));
        assert_ne!(app.focus, day_pane);
        let _ = app.update(Message::Tabs(TabsMsg::Open("wp4nav/other.md".into())));
        let other_pane = app.focus;

        let tomorrow = NaiveDate::from_ymd_opt(2027, 1, 1).expect("date");
        let _ = app.update(Message::Nav(NavMsg::Go {
            pane: day_pane,
            target: NavTarget::Period(Period::Day, tomorrow),
        }));
        assert_eq!(
            calls.lock().expect("calls").last(),
            Some(&("day".to_owned(), "2027-01-01".to_owned()))
        );
        let tabs_of = |app: &App, p| -> Vec<String> {
            app.panes
                .get(p)
                .map(|s| s.tabs.iter().map(|t| t.path.clone()).collect())
                .unwrap_or_default()
        };
        assert_eq!(
            tabs_of(&app, day_pane),
            ["daily/day/2027-01-01.md"],
            "replaced in its own pane"
        );
        assert_eq!(tabs_of(&app, other_pane), ["wp4nav/other.md"]);
        assert!(app.closed.contains(&"daily/day/2026-12-31.md".to_owned()));

        let _ = app.update(Message::Nav(NavMsg::Go {
            pane: day_pane,
            target: NavTarget::Period(
                Period::Month,
                NaiveDate::from_ymd_opt(2027, 1, 1).expect("d"),
            ),
        }));
        assert_eq!(
            calls.lock().expect("calls").last(),
            Some(&("month".to_owned(), "2027-01-01".to_owned()))
        );
        assert_eq!(tabs_of(&app, day_pane), ["daily/month/2027-01.md"]);
    }

    #[test]
    fn nav_in_shadow_reports_not_materialized() {
        let (_tmp, d) = fake_arc();
        let _calls = register_period(&d, false);
        d.create_note("daily/day/2026-10-13.md", "d")
            .expect("create");
        let mut app = app_over(&d, None);
        let _ = app.update(Message::Tabs(TabsMsg::Open(
            "daily/day/2026-10-13.md".into(),
        )));
        shadow(&mut app);
        let pane = app.focus;
        let _ = app.update(Message::Nav(NavMsg::Go {
            pane,
            target: NavTarget::Period(
                Period::Day,
                NaiveDate::from_ymd_opt(2026, 10, 14).expect("d"),
            ),
        }));
        assert_eq!(
            app.status.error.as_deref(),
            Some("not materialized (shadow)")
        );
        assert_eq!(app.open_paths(), ["daily/day/2026-10-13.md"]);
    }

    #[test]
    fn panes_split_open_close_and_persist() {
        let (tmp, d) = fake_arc();
        for p in ["wp4p/a.md", "wp4p/b.md", "wp4p/c.md"] {
            d.create_note(p, "x").expect("create");
        }
        let state = tmp.path().join("state/crucible-vault/editor.json");
        let mut app = app_over(&d, Some(state.clone()));
        let _ = app.update(Message::Tabs(TabsMsg::Open("wp4p/a.md".into())));
        let first = app.focus;
        let _ = app.update(Message::Panes(PanesMsg::SplitDown));
        let second = app.focus;
        assert_eq!(app.panes.len(), 2);
        let _ = app.update(Message::Tabs(TabsMsg::Open("wp4p/b.md".into())));
        let _ = app.update(Message::Tabs(TabsMsg::Open("wp4p/c.md".into())));
        assert_eq!(
            app.panes.get(second).map(|s| s.tabs.len()),
            Some(2),
            "opens into focus"
        );
        assert_eq!(app.panes.get(first).map(|s| s.tabs.len()), Some(1));
        // Re-opening a tab open in another pane focuses it.
        let _ = app.update(Message::Tabs(TabsMsg::Open("wp4p/a.md".into())));
        assert_eq!(app.focus, first);
        let _ = app.update(Message::Panes(PanesMsg::Focus(second)));

        // Persist and reload the layout.
        let restored = app_over(&d, Some(state.clone()));
        assert_eq!(restored.panes.len(), 2);
        assert_eq!(
            restored.open_paths(),
            ["wp4p/a.md", "wp4p/b.md", "wp4p/c.md"]
        );
        assert_eq!(
            restored.active_tab().map(|t| t.path.as_str()),
            Some("wp4p/c.md")
        );
        assert!(matches!(
            restored.ui.panes.as_ref().map(|l| &l.root),
            Some(crate::ui_state::PaneNode::Split {
                dir: crate::ui_state::SplitDir::Down,
                ..
            })
        ));

        // Closing a pane reports each of its tabs.
        app.closed.clear();
        let _ = app.update(Message::Panes(PanesMsg::Close(second)));
        assert_eq!(app.panes.len(), 1);
        assert_eq!(app.closed, ["wp4p/b.md", "wp4p/c.md"]);
        assert_eq!(app.focus, first);
        let _ = app.update(Message::Tabs(TabsMsg::Close(first, 0)));
        assert_eq!(app.closed.last().map(String::as_str), Some("wp4p/a.md"));
        assert!(app.open_paths().is_empty());
    }

    #[test]
    fn wp1_shaped_editor_json_loads_into_one_pane() {
        let (tmp, d) = fake_arc();
        d.create_note("wp4old/a.md", "x").expect("create");
        d.create_note("wp4old/b.md", "y").expect("create");
        let state = tmp.path().join("state/crucible-vault/editor.json");
        std::fs::create_dir_all(state.parent().expect("dir")).expect("mkdir");
        std::fs::write(
            &state,
            br#"{"tabs":["wp4old/a.md","wp4old/b.md"],"active_tab":0,"layout":"source","surround":"dark","recent":[]}"#,
        )
        .expect("write");
        let app = app_over(&d, Some(state));
        assert_eq!(app.panes.len(), 1);
        assert_eq!(app.open_paths(), ["wp4old/a.md", "wp4old/b.md"]);
        assert_eq!(
            app.active_tab().map(|t| t.path.as_str()),
            Some("wp4old/a.md")
        );
        let _ = panes::order(&app.panes);
    }

    // ---- editor-usability-round WP-2: Reading-view filename rename + task toggle ----

    use crate::message::ReadingMsg;

    fn title(app: &mut App, path: &str, value: &str) {
        let _ = app.update(Message::Reading(ReadingMsg::TitleInput {
            path: path.into(),
            value: value.into(),
        }));
    }

    #[test]
    fn filename_rename_dispatches_explorer_flow_and_retargets() {
        let (_tmp, d) = fake_arc();
        d.create_note("wp2ren/one.md", "body").expect("create");
        let mut app = app_over(&d, None);
        let _ = app.update(Message::Explorer(ExplorerMsg::Open("wp2ren/one.md".into())));
        assert_eq!(app.reading.title("wp2ren/one.md"), "one");
        title(&mut app, "wp2ren/one.md", "two");
        assert_eq!(app.reading.title("wp2ren/one.md"), "two");
        let _ = app.update(Message::Reading(ReadingMsg::TitleSubmit {
            path: "wp2ren/one.md".into(),
        }));
        assert!(
            d.read_file("wp2ren/two.md").is_ok(),
            "renamed via the daemon"
        );
        assert!(app.explorer.contains("wp2ren/two.md"));
        assert_eq!(app.open_paths(), ["wp2ren/two.md"], "the tab follows");
        assert_eq!(app.reading.title("wp2ren/two.md"), "two");
        changed(&mut app, "wp2ren/two.md", ChangeKind::Renamed);
        assert!(!app.explorer.stale, "matched via expect_rename");

        // Blur (focus-loss poll) submits; Escape reverts.
        title(&mut app, "wp2ren/two.md", "three");
        let _ = app.update(Message::Reading(ReadingMsg::TitleFocus {
            path: "wp2ren/two.md".into(),
            focused: false,
        }));
        assert_eq!(app.open_paths(), ["wp2ren/three.md"]);
        title(&mut app, "wp2ren/three.md", "four");
        let _ = app.update(Message::Reading(ReadingMsg::TitleRevert));
        assert_eq!(app.reading.title("wp2ren/three.md"), "three");
        let _ = app.update(Message::Reading(ReadingMsg::TitleSubmit {
            path: "wp2ren/three.md".into(),
        }));
        assert_eq!(app.open_paths(), ["wp2ren/three.md"], "reverted: no rename");
    }

    #[test]
    fn filename_rename_daemon_error_surfaces_and_keeps_tab() {
        let (_tmp, d) = fake_arc();
        d.create_note("wp2err/one.md", "body").expect("create");
        let mut app = app_over(&d, None);
        let _ = app.update(Message::Explorer(ExplorerMsg::Open("wp2err/one.md".into())));
        // The file vanished behind the editor's back: the daemon's rename fails (NotFound).
        std::fs::remove_file(d.vault_root().join("wp2err/one.md")).expect("rm");
        title(&mut app, "wp2err/one.md", "two");
        let _ = app.update(Message::Reading(ReadingMsg::TitleSubmit {
            path: "wp2err/one.md".into(),
        }));
        assert!(app.status.error.is_some(), "daemon error surfaced");
        assert_eq!(app.open_paths(), ["wp2err/one.md"], "tab unchanged");
        assert_eq!(app.reading.title("wp2err/one.md"), "one");
        // A slash is refused before any RPC.
        app.status.error = None;
        title(&mut app, "wp2err/one.md", "a/b");
        let _ = app.update(Message::Reading(ReadingMsg::TitleSubmit {
            path: "wp2err/one.md".into(),
        }));
        assert_eq!(app.status.error.as_deref(), Some("invalid name"));
    }

    #[test]
    fn filename_rename_flushes_a_dirty_buffer_first() {
        use iced::widget::text_editor::{Action, Edit};
        let (_tmp, d) = fake_arc();
        d.create_note("wp2dirty/one.md", "body").expect("create");
        let mut app = app_over(&d, None);
        let _ = app.update(Message::Explorer(ExplorerMsg::Open(
            "wp2dirty/one.md".into(),
        )));
        let _ = app.update(Message::Source(crate::message::SourceMsg::Action(
            Action::Edit(Edit::Insert('x')),
        )));
        assert!(app.active_tab().is_some_and(|t| t.source.is_dirty()));
        title(&mut app, "wp2dirty/one.md", "two");
        let _ = app.update(Message::Reading(ReadingMsg::TitleSubmit {
            path: "wp2dirty/one.md".into(),
        }));
        assert_eq!(app.open_paths(), ["wp2dirty/two.md"]);
        assert_eq!(d.read_file("wp2dirty/two.md").expect("renamed").0, b"xbody");
        assert!(app.active_tab().is_some_and(|t| !t.source.is_dirty()));
    }

    #[test]
    fn reading_task_toggle_edits_the_buffer_and_rerenders() {
        let (_tmp, d) = fake_arc();
        d.create_note("wp2task/t.md", "- [ ] a\n- [x] b\n")
            .expect("create");
        let mut app = app_over(&d, None);
        let _ = app.update(Message::Explorer(ExplorerMsg::Open("wp2task/t.md".into())));
        let _ = app.update(Message::Reading(ReadingMsg::ToggleTask {
            path: "wp2task/t.md".into(),
            line: 0,
        }));
        let tab = app.active_tab().expect("tab");
        assert_eq!(tab.source.text(), "- [x] a\n- [x] b\n");
        assert!(tab.source.is_dirty(), "saved by the normal autosave path");
        assert_eq!(
            d.read_file("wp2task/t.md").expect("file").0,
            b"- [ ] a\n- [x] b\n"
        );
        let doc = app.reading.doc("wp2task/t.md").expect("rendered");
        assert_eq!(
            doc.seq,
            tab.source.edit_seq(),
            "re-rendered from the buffer"
        );
        assert_eq!(
            editor_model::render::task_lines(&doc.model),
            vec![(0, 'x'), (1, 'x')]
        );
    }
}

// ---------------------------------------------------------------------------------------------
// editor-usability-round WP-3: overlay, side collapse, borders, history, view mode, dirty slot
// ---------------------------------------------------------------------------------------------

mod usability_wp3 {
    use super::*;
    use crate::message::{ExplorerMsg, PaletteMsg, PanesMsg, ReadingMsg, SwitcherMsg};
    use crate::reading::ViewMode;

    type Fixture = (tempfile::TempDir, Arc<FakeDaemon>, App);

    fn app_with(notes: &[&str], state: Option<std::path::PathBuf>) -> Fixture {
        let (tmp, d) = fake();
        let d = Arc::new(d);
        for n in notes {
            d.create_note(n, "x").expect("create");
        }
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let mut app = App::new(Some(dc), state);
        app.rpc_inline = true;
        (tmp, d, app)
    }

    fn open(app: &mut App, p: &str) {
        let _ = app.update(Message::Explorer(ExplorerMsg::Open(p.into())));
    }

    fn active(app: &App) -> Option<String> {
        app.active_tab().map(|t| t.path.clone())
    }

    fn history(app: &App) -> Vec<String> {
        app.panes
            .get(app.focus)
            .map(|ps| ps.history.entries().to_vec())
            .unwrap_or_default()
    }

    #[test]
    fn back_forward_walks_the_pane_history_without_pushing() {
        let (_tmp, _d, mut app) = app_with(&["h3/a.md", "h3/b.md", "h3/c.md"], None);
        for p in ["h3/a.md", "h3/b.md", "h3/c.md"] {
            open(&mut app, p);
        }
        assert_eq!(history(&app), ["h3/a.md", "h3/b.md", "h3/c.md"]);
        let pane = app.focus;
        let _ = app.update(Message::Panes(PanesMsg::Back(pane)));
        assert_eq!(active(&app).as_deref(), Some("h3/b.md"));
        let _ = app.update(Message::Panes(PanesMsg::BackFocused));
        assert_eq!(active(&app).as_deref(), Some("h3/a.md"));
        let _ = app.update(Message::Panes(PanesMsg::BackFocused));
        assert_eq!(active(&app).as_deref(), Some("h3/a.md"), "no-op at start");
        let _ = app.update(Message::Panes(PanesMsg::ForwardFocused));
        assert_eq!(active(&app).as_deref(), Some("h3/b.md"));
        assert_eq!(history(&app).len(), 3, "back/forward never push");
        // Re-opening the current entry does not duplicate it; a new open drops forward.
        open(&mut app, "h3/b.md");
        open(&mut app, "h3/a.md");
        assert_eq!(history(&app), ["h3/a.md", "h3/b.md", "h3/a.md"]);
        let _ = app.update(Message::Panes(PanesMsg::Forward(pane)));
        assert_eq!(active(&app).as_deref(), Some("h3/a.md"));
    }

    #[test]
    fn rename_retargets_history_entries() {
        let (_tmp, _d, mut app) = app_with(&["h3r/one.md", "h3r/other.md"], None);
        open(&mut app, "h3r/one.md");
        open(&mut app, "h3r/other.md");
        let _ = app.update(Message::Reading(ReadingMsg::TitleInput {
            path: "h3r/other.md".into(),
            value: "two".into(),
        }));
        let _ = app.update(Message::Reading(ReadingMsg::TitleSubmit {
            path: "h3r/other.md".into(),
        }));
        assert_eq!(history(&app), ["h3r/one.md", "h3r/two.md"]);
    }

    #[test]
    fn ctrl_e_toggles_source_and_reading_only() {
        assert_eq!(ViewMode::Source.toggle(), ViewMode::Reading);
        assert_eq!(ViewMode::Reading.toggle(), ViewMode::Source);
        assert_eq!(ViewMode::Split.toggle(), ViewMode::Source);
        let (_tmp, _d, mut app) = app_with(&["m3/a.md"], None);
        open(&mut app, "m3/a.md");
        let _ = app.update(Message::Reading(ReadingMsg::SetMode {
            path: "m3/a.md".into(),
            mode: ViewMode::Split,
        }));
        let _ = app.update(Message::Reading(ReadingMsg::CycleMode));
        assert_eq!(app.reading.mode("m3/a.md"), ViewMode::Source);
        let _ = app.update(Message::Reading(ReadingMsg::CycleMode));
        assert_eq!(app.reading.mode("m3/a.md"), ViewMode::Reading);
    }

    #[test]
    fn mode_menu_opens_and_closes_on_select() {
        let (_tmp, _d, mut app) = app_with(&["m3m/a.md"], None);
        open(&mut app, "m3m/a.md");
        let pane = app.focus;
        let _ = app.update(Message::Panes(PanesMsg::ModeMenu(Some(pane))));
        assert_eq!(app.mode_menu, Some(pane));
        drop(app.view());
        let _ = app.update(Message::Reading(ReadingMsg::SetMode {
            path: "m3m/a.md".into(),
            mode: ViewMode::Split,
        }));
        assert_eq!(app.mode_menu, None);
        assert_eq!(app.reading.mode("m3m/a.md"), ViewMode::Split);
    }

    #[test]
    fn overlay_close_message_closes_switcher_and_palette() {
        let (_tmp, _d, mut app) = app_with(&[], None);
        assert!(app.overlay_close().is_none());
        let _ = app.update(Message::Switcher(SwitcherMsg::Open));
        drop(app.view());
        let close = app.overlay_close().expect("switcher open");
        assert!(matches!(close, Message::Switcher(SwitcherMsg::Close)));
        let _ = app.update(close);
        assert!(!app.switcher.open);
        let _ = app.update(Message::Palette(PaletteMsg::Open));
        drop(app.view());
        let close = app.overlay_close().expect("palette open");
        assert!(matches!(close, Message::Palette(PaletteMsg::Close)));
        let _ = app.update(close);
        assert!(!app.palette.open);
        assert!(app.overlay_close().is_none());
    }

    #[test]
    fn side_collapse_and_borders_persist() {
        let tmp = tempfile::tempdir().expect("tmp");
        let state = tmp.path().join("state/editor.json");
        let (_tmp, d, mut app) = app_with(&[], Some(state.clone()));
        assert!(!app.ui.side_collapsed && !app.ui.pane_borders);
        let _ = app.update(Message::Panes(PanesMsg::ToggleSide));
        let _ = app.update(Message::Panes(PanesMsg::ToggleBorders));
        drop(app.view());
        let dc: Arc<dyn DaemonClient> = d as Arc<dyn DaemonClient>;
        let again = App::new(Some(dc), Some(state));
        assert!(again.ui.side_collapsed && again.ui.pane_borders);
    }

    #[test]
    fn palette_lists_the_pane_border_toggle() {
        let cmd = crate::palette::ClientCmd::TogglePaneBorders;
        assert!(crate::palette::ClientCmd::all().contains(&cmd));
        assert_eq!(cmd.title().0, "Toggle pane borders (debug)");
        assert!(matches!(
            cmd.message(pane_grid_pane()),
            Message::Panes(PanesMsg::ToggleBorders)
        ));
    }

    fn pane_grid_pane() -> iced::widget::pane_grid::Pane {
        let (_tmp, _d, app) = app_with(&[], None);
        app.focus
    }

    #[test]
    fn dirty_marker_lives_on_the_tab_and_source_slots_are_stable() {
        assert_eq!(
            crate::source::SLOTS.iter().position(|s| *s == "editor"),
            Some(2)
        );
        assert_eq!(crate::source::status_notice(Mode::Live), None);
        assert!(crate::source::status_notice(Mode::Shadow).is_some());
        let (_tmp, _d, mut app) = app_with(&["d3/t.md"], None);
        let _ = app.update(Message::Explorer(ExplorerMsg::Open("d3/t.md".into())));
        let tab = app.active_tab().expect("tab");
        assert_eq!(crate::source::tab_title(&tab.source, &tab.path), "d3/t.md");
        let _ = app.update(Message::Reading(ReadingMsg::SetMode {
            path: "d3/t.md".into(),
            mode: ViewMode::Source,
        }));
        drop(app.view());
        let _ = app.update(Message::Source(crate::message::SourceMsg::Action(
            iced::widget::text_editor::Action::Edit(iced::widget::text_editor::Edit::Insert('y')),
        )));
        let tab = app.active_tab().expect("tab");
        assert!(tab.source.is_dirty());
        assert_eq!(
            crate::source::tab_title(&tab.source, &tab.path),
            "d3/t.md •"
        );
        drop(app.view());
    }

    #[test]
    fn ctrl_alt_arrows_step_the_focused_pane() {
        use iced::keyboard::{Event, Key, Location, Modifiers, key::Named, key::Physical};
        let ev = |n: Named, m: Modifiers| Event::KeyPressed {
            key: Key::Named(n),
            modified_key: Key::Named(n),
            physical_key: Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified),
            location: Location::Standard,
            modifiers: m,
            text: None,
            repeat: false,
        };
        let ca = Modifiers::CTRL | Modifiers::ALT;
        assert!(matches!(
            crate::palette::key_event(ev(Named::ArrowLeft, ca)),
            Some(Message::Panes(PanesMsg::BackFocused))
        ));
        assert!(matches!(
            crate::palette::key_event(ev(Named::ArrowRight, ca)),
            Some(Message::Panes(PanesMsg::ForwardFocused))
        ));
        assert!(!matches!(
            crate::palette::key_event(ev(Named::ArrowLeft, Modifiers::CTRL)),
            Some(Message::Panes(_))
        ));
    }
}

// ---------------------------------------------------------------------------------------------
// editor-usability-round WP-4: keymap, client commands, close/quit flush, find in note
// ---------------------------------------------------------------------------------------------

mod usability_wp4 {
    use iced::widget::text_editor::{Action, Edit};

    use super::*;
    use crate::message::{ExplorerMsg, FindMsg, PaletteMsg, SourceMsg, TabsMsg};
    use crate::palette::chord::{self, Chord};
    use crate::reading::ViewMode;

    type Fixture = (tempfile::TempDir, Arc<FakeDaemon>, App);

    fn app_with(notes: &[(&str, &str)]) -> Fixture {
        let (tmp, d) = fake();
        let d = Arc::new(d);
        for (n, body) in notes {
            d.create_note(n, body).expect("create");
        }
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let mut app = App::new(Some(dc), None);
        app.rpc_inline = true;
        (tmp, d, app)
    }

    fn open(app: &mut App, p: &str) {
        let _ = app.update(Message::Explorer(ExplorerMsg::Open(p.into())));
    }

    fn type_char(app: &mut App, c: char) {
        let _ = app.update(Message::Source(SourceMsg::Action(Action::Edit(
            Edit::Insert(c),
        ))));
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

    fn c(s: &str) -> Chord {
        chord::parse(s).expect("chord")
    }

    /// Registers `id` on the fake daemon, counting its runs.
    fn register_counting(d: &Arc<FakeDaemon>, id: &str) -> Arc<std::sync::Mutex<Vec<String>>> {
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&calls);
        d.register_action(
            info(id, &[]),
            Arc::new(move |p: &RunActionParams| {
                if let Ok(mut g) = seen.lock() {
                    g.push(
                        p.params
                            .get("period")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                    );
                }
                Ok(None)
            }),
        )
        .expect("register");
        calls
    }

    #[test]
    fn keymap_has_no_duplicate_chord_and_reserves_settings_chords() {
        let map = crate::palette::keymap();
        let mut seen = std::collections::HashSet::new();
        for (k, what) in &map {
            let parsed = chord::parse(k).unwrap_or_else(|| panic!("unparseable {k}"));
            assert!(seen.insert(parsed), "duplicate chord {k} ({what})");
        }
        for k in [
            "Ctrl+Shift+P",
            "Ctrl+P",
            "Ctrl+O",
            "Ctrl+R",
            "Ctrl+F",
            "Ctrl+Shift+F",
            "Ctrl+W",
            "Ctrl+Q",
            "Alt+1",
            "Alt+8",
            "Ctrl+Shift+,",
        ] {
            assert!(chord::reserved(&c(k)), "{k} reserved");
        }
        // plan editor-settings-tab WP-2: Ctrl+, left RESERVED for the Open settings command.
        assert!(!chord::reserved(&c("Ctrl+,")));
        assert_eq!(
            crate::palette::client_chord(&c("Ctrl+,")),
            Some(crate::palette::ClientCmd::OpenSettings)
        );
        assert_eq!(
            crate::palette::client_chord(&c("Ctrl+P")),
            Some(crate::palette::ClientCmd::QuickSwitcher)
        );
        assert_eq!(
            crate::palette::client_chord(&c("Ctrl+Shift+P")),
            Some(crate::palette::ClientCmd::CommandPaletteKey)
        );
        assert_eq!(crate::palette::client_chord(&c("Alt+9")), None);
    }

    #[test]
    fn actions_toml_key_colliding_with_a_client_default_is_refused() {
        let (map, notes) =
            crate::palette::bindings(&[info("a", &["ctrl+w"]), info("b", &["ctrl+alt+l"])]);
        assert!(!map.contains_key(&c("Ctrl+W")));
        assert_eq!(map.get(&c("Ctrl+Alt+L")).map(String::as_str), Some("b"));
        assert_eq!(notes, ["a: Ctrl+W is an editor default (not bound)"]);
    }

    #[test]
    fn default_action_chords_run_presets_and_are_noops_when_absent() {
        let (_tmp, d, mut app) = app_with(&[("wp4k/a.md", "x")]);
        // No actions registered: Ctrl+O / Ctrl+R do nothing, and no "Open today" entry exists.
        let _ = app.update(Message::Palette(PaletteMsg::Key(c("Ctrl+O"))));
        let _ = app.update(Message::Palette(PaletteMsg::Key(c("Ctrl+R"))));
        assert!(app.status.error.is_none(), "{:?}", app.status.error);
        assert!(!app.palette.entries.iter().any(|e| e.title == "Open today"));
        let periods = register_counting(&d, "period.open");
        let lints = register_counting(&d, "lint-note");
        app.palette.invalidate();
        let _ = app.update(Message::Palette(PaletteMsg::Key(c("Ctrl+O"))));
        open(&mut app, "wp4k/a.md");
        let _ = app.update(Message::Palette(PaletteMsg::Key(c("Ctrl+R"))));
        assert_eq!(*periods.lock().expect("lock"), ["day"]);
        assert_eq!(lints.lock().expect("lock").len(), 1);
        let today = app
            .palette
            .entries
            .iter()
            .find(|e| e.title == "Open today")
            .expect("listed once period.open is loaded");
        assert_eq!(today.keys, ["Ctrl+O"]);
    }

    #[test]
    fn close_tab_saves_a_dirty_buffer_first_and_cancels_on_conflict() {
        let (_tmp, d, mut app) = app_with(&[("wp4c/a.md", "a"), ("wp4c/b.md", "b")]);
        open(&mut app, "wp4c/a.md");
        let _ = app.update(Message::Reading(crate::message::ReadingMsg::SetMode {
            path: "wp4c/a.md".into(),
            mode: ViewMode::Source,
        }));
        type_char(&mut app, 'x');
        let _ = app.update(Message::Tabs(TabsMsg::CloseActive));
        assert!(app.open_paths().is_empty(), "closed after the save");
        assert_eq!(d.read_file("wp4c/a.md").expect("file").0, b"xa");

        // Conflict: the file changes on disk under a dirty buffer → the close is cancelled.
        open(&mut app, "wp4c/b.md");
        let _ = app.update(Message::Reading(crate::message::ReadingMsg::SetMode {
            path: "wp4c/b.md".into(),
            mode: ViewMode::Source,
        }));
        type_char(&mut app, 'y');
        let (_, h) = d.read_file("wp4c/b.md").expect("file");
        d.write_note("wp4c/b.md", "theirs", &h)
            .expect("external write");
        let _ = app.update(Message::Tabs(TabsMsg::Close(app.focus, 0)));
        assert_eq!(app.open_paths(), ["wp4c/b.md"], "close cancelled");
        assert!(
            app.status
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("close cancelled")),
            "{:?}",
            app.status.error
        );
        assert!(!app.exiting);
    }

    #[test]
    fn quit_flushes_dirty_buffers_then_exits_and_cancels_on_failure() {
        let (_tmp, d, mut app) = app_with(&[("wp4q/a.md", "a")]);
        open(&mut app, "wp4q/a.md");
        let _ = app.update(Message::Reading(crate::message::ReadingMsg::SetMode {
            path: "wp4q/a.md".into(),
            mode: ViewMode::Source,
        }));
        type_char(&mut app, 'x');
        let (_, h) = d.read_file("wp4q/a.md").expect("file");
        d.write_note("wp4q/a.md", "theirs", &h)
            .expect("external write");
        let _ = app.update(Message::Tabs(TabsMsg::Quit));
        assert!(!app.exiting, "a failed save cancels the quit");
        assert!(app.status.error.is_some());

        let (_tmp2, d2, mut app2) = app_with(&[("wp4q/b.md", "b")]);
        open(&mut app2, "wp4q/b.md");
        let _ = app2.update(Message::Reading(crate::message::ReadingMsg::SetMode {
            path: "wp4q/b.md".into(),
            mode: ViewMode::Source,
        }));
        type_char(&mut app2, 'y');
        let _ = app2.update(Message::Tabs(TabsMsg::Quit));
        assert!(app2.exiting);
        assert_eq!(d2.read_file("wp4q/b.md").expect("file").0, b"yb");
    }

    #[test]
    fn go_to_tab_selects_in_the_focused_pane_and_ignores_out_of_range() {
        let (_tmp, _d, mut app) = app_with(&[("wp4g/a.md", "a"), ("wp4g/b.md", "b")]);
        open(&mut app, "wp4g/a.md");
        open(&mut app, "wp4g/b.md");
        let _ = app.update(Message::Palette(PaletteMsg::Key(c("Alt+1"))));
        assert_eq!(app.active_tab().map(|t| t.path.as_str()), Some("wp4g/a.md"));
        let _ = app.update(Message::Tabs(TabsMsg::GoTo(2)));
        assert_eq!(app.active_tab().map(|t| t.path.as_str()), Some("wp4g/b.md"));
        let _ = app.update(Message::Tabs(TabsMsg::GoTo(5)));
        assert_eq!(app.active_tab().map(|t| t.path.as_str()), Some("wp4g/b.md"));
    }

    #[test]
    fn find_selects_matches_in_source_with_wrap() {
        let (_tmp, _d, mut app) = app_with(&[("wp4f/a.md", "one Foo\ntwo foo\nfoo")]);
        open(&mut app, "wp4f/a.md");
        let _ = app.update(Message::Reading(crate::message::ReadingMsg::SetMode {
            path: "wp4f/a.md".into(),
            mode: ViewMode::Source,
        }));
        let _ = app.update(Message::Palette(PaletteMsg::Key(c("Ctrl+F"))));
        assert!(app.panes.get(app.focus).is_some_and(|ps| ps.find.is_some()));
        let _ = app.update(Message::Find(FindMsg::Query("FOO".into())));
        let sel = |app: &App| app.active_tab().and_then(|t| t.source.selection());
        let line = |app: &App| app.active_tab().map(|t| t.source.cursor().position.line);
        assert_eq!(sel(&app).as_deref(), Some("Foo"));
        assert_eq!(line(&app), Some(0));
        let _ = app.update(Message::Find(FindMsg::Submit));
        assert_eq!(line(&app), Some(1));
        let _ = app.update(Message::Find(FindMsg::Next));
        assert_eq!(line(&app), Some(2));
        let _ = app.update(Message::Find(FindMsg::Next));
        assert_eq!(line(&app), Some(0), "wraps forward");
        let _ = app.update(Message::Find(FindMsg::Shift(true)));
        let _ = app.update(Message::Find(FindMsg::Submit));
        assert_eq!(line(&app), Some(2), "Shift+Enter wraps back");
        let bar = app
            .panes
            .get(app.focus)
            .and_then(|ps| ps.find.clone())
            .expect("bar");
        assert_eq!(bar.count(), "3/3");
        drop(app.view());
        let _ = app.update(Message::Find(FindMsg::Close));
        assert!(app.panes.get(app.focus).is_some_and(|ps| ps.find.is_none()));
    }

    #[test]
    fn source_key_binding_passes_client_chords_through() {
        use iced::keyboard::{Key, Modifiers, key::Physical};
        use iced::widget::text_editor::{Binding, KeyPress, Status};
        let kp = |ch: &str, m: Modifiers, text: Option<&str>| KeyPress {
            key: Key::Character(ch.into()),
            modified_key: Key::Character(ch.into()),
            physical_key: Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified),
            modifiers: m,
            text: text.map(Into::into),
            status: Status::Focused { is_hovered: false },
        };
        let routed = |b: Option<Binding<Message>>| match b {
            Some(Binding::Custom(Message::Palette(PaletteMsg::Key(c)))) => Some(c.to_string()),
            _ => None,
        };
        let cs = Modifiers::CTRL | Modifiers::SHIFT;
        for (ch, m, want) in [
            ("w", Modifiers::CTRL, "Ctrl+W"),
            ("q", Modifiers::CTRL, "Ctrl+Q"),
            ("f", Modifiers::CTRL, "Ctrl+F"),
            ("r", Modifiers::CTRL, "Ctrl+R"),
            ("o", Modifiers::CTRL, "Ctrl+O"),
            ("p", Modifiers::CTRL, "Ctrl+P"),
            ("P", cs, "Ctrl+Shift+P"),
            ("F", cs, "Ctrl+Shift+F"),
            ("1", Modifiers::ALT, "Alt+1"),
            ("8", Modifiers::ALT, "Alt+8"),
            (",", Modifiers::CTRL, "Ctrl+,"),
        ] {
            assert_eq!(
                routed(crate::source::test_key_binding(kp(ch, m, Some(ch)))).as_deref(),
                Some(want),
                "{want}"
            );
        }
        // Copy and plain typing are untouched.
        assert!(matches!(
            crate::source::test_key_binding(kp("c", Modifiers::CTRL, None)),
            Some(Binding::Copy)
        ));
        assert!(matches!(
            crate::source::test_key_binding(kp("9", Modifiers::ALT, Some("9"))),
            Some(Binding::Insert('9'))
        ));
    }
}
