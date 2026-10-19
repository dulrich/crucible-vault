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

    let _ = app.send(Message::Tabs(TabsMsg::Open(p.clone())));
    assert_eq!(app.open_paths().len(), 1);
    assert_eq!(app.active_doc().map(|t| t.text()), Some(expected));
    let _ = app.send(Message::Tabs(TabsMsg::Open(p.clone())));
    assert_eq!(app.open_paths().len(), 1, "re-open focuses the tab");
    let _ = app.send(Message::Tabs(TabsMsg::Open("missing.md".into())));
    assert_eq!(app.open_paths().len(), 1);
    assert_eq!(app.status.error_text(), Some("file not found"));

    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::ModeChanged { mode: Mode::Shadow },
    )));
    assert_eq!(app.status.mode, Mode::Shadow);
    let base = d.read_file(&p).expect("read").1;
    d.write_note(&p, "changed\n", &base).expect("write");
    let g = d.status().expect("status").generation;
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::IndexGeneration { n: g },
    )));
    assert_eq!(app.status.generation, g);
    assert!(app.status.line().contains(&format!("gen {g}")));
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            path: p.clone(),
            kind: ChangeKind::Modified,
            hash: None,
            origin: Origin::External,
        },
    )));
    assert_eq!(
        app.active_doc().map(|t| t.text()).as_deref(),
        Some("changed\n")
    );
    let _ = app.send(Message::Status(StatusMsg::Disconnected));
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

    pub(super) fn app_over(d: &Arc<FakeDaemon>, state: Option<std::path::PathBuf>) -> App {
        let dc: Arc<dyn DaemonClient> = Arc::clone(d) as Arc<dyn DaemonClient>;
        let mut app = App::new(Some(dc), state);
        app.queue_rpcs();
        app
    }

    pub(super) fn fake_arc() -> (tempfile::TempDir, Arc<FakeDaemon>) {
        let (tmp, d) = fake();
        (tmp, Arc::new(d))
    }

    fn changed(app: &mut App, path: &str, kind: ChangeKind) {
        let _ = app.send(Message::Status(StatusMsg::Notification(
            Notification::FileChanged {
                path: path.to_owned(),
                kind,
                hash: None,
                origin: Origin::Daemon,
            },
        )));
    }

    fn shadow(app: &mut App) {
        let _ = app.send(Message::Status(StatusMsg::Notification(
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
        let _ = app.send(Message::Explorer(ExplorerMsg::Toggle("wp4tree".into())));
        let _ = app.send(Message::Explorer(ExplorerMsg::Toggle(
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
        let _ = app.send(Message::Status(StatusMsg::Notification(
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
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp4ops/one.md".into())));
        assert_eq!(app.open_paths(), ["wp4ops/one.md"]);

        let _ = app.send(Message::Explorer(ExplorerMsg::BeginRename));
        assert_eq!(
            app.explorer.edit.as_ref().map(|e| e.input.as_str()),
            Some("one.md")
        );
        let _ = app.send(Message::Explorer(ExplorerMsg::Input("two.md".into())));
        let _ = app.send(Message::Explorer(ExplorerMsg::Submit));
        assert!(
            d.read_file("wp4ops/two.md").is_ok(),
            "renamed through the daemon"
        );
        assert!(app.explorer.contains("wp4ops/two.md"));
        assert!(!app.explorer.contains("wp4ops/one.md"));
        assert_eq!(app.open_paths(), ["wp4ops/two.md"], "the tab follows");
        changed(&mut app, "wp4ops/two.md", ChangeKind::Renamed);
        assert!(!app.explorer.stale, "own rename is matched");

        let _ = app.send(Message::Explorer(ExplorerMsg::BeginMove));
        let _ = app.send(Message::Explorer(ExplorerMsg::Input("wp4moved".into())));
        let _ = app.send(Message::Explorer(ExplorerMsg::Submit));
        assert!(app.explorer.contains("wp4moved/two.md"));
        assert_eq!(app.explorer.selected.as_deref(), Some("wp4moved/two.md"));

        let _ = app.send(Message::Explorer(ExplorerMsg::BeginNew));
        let _ = app.send(Message::Explorer(ExplorerMsg::Input("fresh".into())));
        let _ = app.send(Message::Explorer(ExplorerMsg::Submit));
        assert!(app.explorer.contains("wp4moved/fresh.md"));
        assert_eq!(
            app.active_tab().map(|t| t.path.as_str()),
            Some("wp4moved/fresh.md")
        );

        let _ = app.send(Message::Explorer(ExplorerMsg::Select(
            "wp4moved/two.md".into(),
        )));
        let _ = app.send(Message::Explorer(ExplorerMsg::Trash));
        let _ = app.send(Message::Dialog(crate::dialog::DialogMsg::Ok));
        assert!(matches!(
            d.read_file("wp4moved/two.md"),
            Err(DaemonError::NotFound(_))
        ));
        assert!(!app.explorer.contains("wp4moved/two.md"));
        assert!(!app.open_paths().contains(&"wp4moved/two.md".to_owned()));
        assert!(app.closed().contains(&"wp4moved/two.md".to_owned()));
    }

    #[test]
    fn rename_and_trash_refuse_a_dirty_open_note() {
        use iced::widget::text_editor::{Action, Edit};
        let (_tmp, d) = fake_arc();
        d.create_note("wp4dirty/one.md", "body").expect("create");
        let mut app = app_over(&d, None);
        let _ = app.send(Message::Explorer(ExplorerMsg::Open(
            "wp4dirty/one.md".into(),
        )));
        let _ = app.send(Message::Source(crate::message::SourceMsg::Action(
            Action::Edit(Edit::Insert('x')),
        )));
        assert!(app.active_doc().is_some_and(|t| t.is_dirty()));

        let _ = app.send(Message::Explorer(ExplorerMsg::BeginRename));
        let _ = app.send(Message::Explorer(ExplorerMsg::Input("two.md".into())));
        let _ = app.send(Message::Explorer(ExplorerMsg::Submit));
        assert!(d.read_file("wp4dirty/one.md").is_ok(), "not renamed");
        assert!(d.read_file("wp4dirty/two.md").is_err());
        assert!(app.active_doc().is_some_and(|t| t.is_dirty()), "edits kept");

        let _ = app.send(Message::Explorer(ExplorerMsg::Select(
            "wp4dirty/one.md".into(),
        )));
        let _ = app.send(Message::Explorer(ExplorerMsg::Trash));
        assert!(d.read_file("wp4dirty/one.md").is_ok(), "not trashed");
        assert!(app.status.error_text().is_some());
    }

    /// Plan round-7-feedback-decisions WP-2: a dirty note's delete opens the unsaved-edits
    /// dialog; Cancel keeps everything.
    fn dirty_delete_app(d: &Arc<FakeDaemon>, path: &str) -> App {
        use iced::widget::text_editor::{Action, Edit};
        d.create_note(path, "body").expect("create");
        let mut app = app_over(d, None);
        let _ = app.send(Message::Explorer(ExplorerMsg::Open(path.into())));
        let _ = app.send(Message::Source(crate::message::SourceMsg::Action(
            Action::Edit(Edit::Insert('x')),
        )));
        let _ = app.send(Message::Explorer(ExplorerMsg::DeleteActive));
        assert!(matches!(
            app.dialog.as_ref().map(|d| &d.content),
            Some(crate::dialog::Content::DeleteUnsaved(p)) if p == path
        ));
        app
    }

    fn confirm_shown(app: &App) -> bool {
        app.dialog
            .as_ref()
            .and_then(|d| d.delete_confirm())
            .is_some()
    }

    #[test]
    fn dirty_delete_save_then_delete_writes_then_deletes() {
        use crate::dialog::DialogMsg;
        let (_tmp, d) = fake_arc();
        let mut app = dirty_delete_app(&d, "r7del/s.md");
        let _ = app.send(Message::Dialog(DialogMsg::Ok));
        assert_eq!(
            d.read_file("r7del/s.md").expect("file").0,
            b"xbody",
            "saved first"
        );
        assert!(confirm_shown(&app), "the delete confirm follows");
        let _ = app.send(Message::Dialog(DialogMsg::Ok));
        assert!(matches!(
            d.read_file("r7del/s.md"),
            Err(DaemonError::NotFound(_))
        ));
        assert!(app.open_paths().is_empty());
    }

    #[test]
    fn dirty_delete_discard_then_delete() {
        use crate::dialog::DialogMsg;
        let (_tmp, d) = fake_arc();
        let mut app = dirty_delete_app(&d, "r7del/d.md");
        let _ = app.send(Message::Dialog(DialogMsg::Alt));
        assert!(
            app.doc("r7del/d.md").is_some_and(|t| !t.is_dirty()),
            "edits dropped"
        );
        assert!(confirm_shown(&app));
        let _ = app.send(Message::Dialog(DialogMsg::Ok));
        assert!(matches!(
            d.read_file("r7del/d.md"),
            Err(DaemonError::NotFound(_))
        ));
    }

    #[test]
    fn dirty_delete_cancel_keeps_note_and_edits() {
        use crate::dialog::DialogMsg;
        let (_tmp, d) = fake_arc();
        let mut app = dirty_delete_app(&d, "r7del/c.md");
        let _ = app.send(Message::Dialog(DialogMsg::Cancel));
        assert!(app.dialog.is_none());
        assert_eq!(d.read_file("r7del/c.md").expect("file").0, b"body");
        assert!(
            app.doc("r7del/c.md")
                .is_some_and(|t| t.is_dirty() && t.text() == "xbody")
        );
    }

    /// WP-2 (round 7): an explorer rename of a clean open note keeps cursor, selection, undo
    /// depth and top_line, with no conflict.
    #[test]
    fn explorer_rename_of_a_clean_note_keeps_editor_state() {
        let (_tmp, d) = fake_arc();
        let body: String = (0..80).map(|i| format!("line {i}\n")).collect();
        d.create_note("r7ren/one.md", &body).expect("create");
        let mut app = app_over(&d, None);
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("r7ren/one.md".into())));
        let doc = app.doc_mut("r7ren/one.md").expect("open");
        doc.select_range((2, 0), (2, 4));
        assert!(doc.paste("LINE".into(), false, std::time::Instant::now()));
        let job = doc.begin_save(false).expect("save");
        let outcome = job.run(&*d);
        let _ = app.send(job.done(outcome));
        let doc = app.doc_mut("r7ren/one.md").expect("open");
        doc.scroll_to_line(30);
        doc.select_range((5, 1), (5, 3));
        let before = (doc.cursor(), doc.history().undo_len(), doc.top_line());
        assert!(!doc.is_dirty() && before.1 > 0);
        let _ = app.send(Message::Explorer(ExplorerMsg::Select(
            "r7ren/one.md".into(),
        )));
        let _ = app.send(Message::Explorer(ExplorerMsg::BeginRename));
        let _ = app.send(Message::Explorer(ExplorerMsg::Input("two.md".into())));
        let _ = app.send(Message::Explorer(ExplorerMsg::Submit));
        assert_eq!(app.open_paths(), ["r7ren/two.md"]);
        let doc = app.doc("r7ren/two.md").expect("followed");
        assert_eq!(
            (doc.cursor(), doc.history().undo_len(), doc.top_line()),
            before
        );
        assert_eq!(doc.path(), "r7ren/two.md");
        assert!(!doc.is_dirty() && doc.conflict().is_none());
    }

    #[test]
    fn shadow_mode_blocks_file_ops_but_not_navigation() {
        let (_tmp, d) = fake_arc();
        d.create_note("wp4sh/keep.md", "k").expect("create");
        let mut app = app_over(&d, None);
        shadow(&mut app);
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp4sh/keep.md".into())));
        assert_eq!(app.open_paths(), ["wp4sh/keep.md"], "navigation works");
        let _ = app.send(Message::Explorer(ExplorerMsg::BeginNew));
        assert!(app.explorer.edit.is_none());
        assert_eq!(
            app.status.error_text(),
            Some(crate::explorer::SHADOW_REASON)
        );
        let _ = app.send(Message::Explorer(ExplorerMsg::Trash));
        assert!(d.read_file("wp4sh/keep.md").is_ok());
        let _ = app.send(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::AddActive)));
        assert!(app.bookmarks.items.is_empty());
        assert!(d.read_file(BOOKMARKS_PATH).is_err());
        let _ = app.send(Message::Switcher(SwitcherMsg::Open));
        let _ = app.send(Message::Switcher(SwitcherMsg::Query(
            "nothing like it".into(),
        )));
        let _ = app.send(Message::Switcher(SwitcherMsg::Create));
        assert!(d.read_file("nothing like it.md").is_err());
    }

    pub(super) fn register_create(d: &Arc<FakeDaemon>) {
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
        let _ = app.send(Message::Switcher(SwitcherMsg::Open));
        assert!(app.switcher.open);
        let _ = app.send(Message::Switcher(SwitcherMsg::Query("works".into())));
        assert_eq!(app.switcher.current(), Some("wp4sw/Works.md"));
        let _ = app.send(Message::Switcher(SwitcherMsg::Submit));
        assert!(!app.switcher.open);
        assert_eq!(
            app.active_tab().map(|t| t.path.as_str()),
            Some("wp4sw/Works.md")
        );

        let _ = app.send(Message::Switcher(SwitcherMsg::Open));
        let _ = app.send(Message::Switcher(SwitcherMsg::Query(
            "qqzz brand idea".into(),
        )));
        assert!(app.switcher.results.is_empty());
        let _ = app.send(Message::Switcher(SwitcherMsg::Submit));
        assert!(d.read_file("qqzz brand idea.md").is_ok(), "note.create ran");
        assert_eq!(
            app.active_tab().map(|t| t.path.as_str()),
            Some("qqzz brand idea.md")
        );

        // Shift+Enter creates even when something matches.
        let _ = app.send(Message::Switcher(SwitcherMsg::Open));
        let _ = app.send(Message::Switcher(SwitcherMsg::Query("wp4sw/Works2".into())));
        let _ = app.send(Message::Modifiers(iced::keyboard::Modifiers::SHIFT));
        let _ = app.send(Message::Switcher(SwitcherMsg::Submit));
        assert!(d.read_file("wp4sw/Works2.md").is_ok());
    }

    #[test]
    fn bookmarks_load_add_and_conflict() {
        let (_tmp, d) = fake_arc();
        d.create_note("wp4bm/a.md", "a").expect("create");
        let mut app = app_over(&d, None);
        assert!(
            app.bookmarks.items.is_empty()
                && app
                    .settings
                    .file(BOOKMARKS_PATH)
                    .is_some_and(|f| f.base.is_none())
        );
        let _ = app.send(Message::Tabs(TabsMsg::Open("wp4bm/a.md".into())));
        let _ = app.send(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::AddActive)));
        let (bytes, h1) = d.read_file(BOOKMARKS_PATH).expect("created");
        assert_eq!(
            crate::bookmarks::parse(&bytes).expect("parse"),
            [crate::bookmarks::Bookmark {
                name: "a".into(),
                path: "wp4bm/a.md".into(),
                keys: vec![],
            }]
        );
        assert_eq!(
            app.settings
                .file(BOOKMARKS_PATH)
                .and_then(|f| f.base.as_ref()),
            Some(&h1)
        );

        // Loading from an existing file.
        let fresh = app_over(&d, None);
        assert_eq!(fresh.bookmarks.items.len(), 1);

        // Someone else rewrites the file: the next edit hits a CAS conflict and reloads.
        let other = "[[bookmark]]\nname = \"x\"\npath = \"x.md\"\n\n[[bookmark]]\nname = \"y\"\npath = \"y.md\"\n";
        d.write_note(BOOKMARKS_PATH, other, &h1).expect("external");
        let _ = app.send(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::Remove(0))));
        assert!(
            app.status
                .error_text()
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
        let _ = app.send(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::Down(0))));
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
        let _ = app.send(Message::Tabs(TabsMsg::Open(
            "daily/day/2026-12-31.md".into(),
        )));
        let day_pane = app.focus;
        let _ = app.send(Message::Panes(PanesMsg::SplitRight));
        assert_ne!(app.focus, day_pane);
        let _ = app.send(Message::Tabs(TabsMsg::Open("wp4nav/other.md".into())));
        let other_pane = app.focus;

        let tomorrow = NaiveDate::from_ymd_opt(2027, 1, 1).expect("date");
        let _ = app.send(Message::Nav(NavMsg::Go {
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
        assert!(app.closed().contains(&"daily/day/2026-12-31.md".to_owned()));

        let _ = app.send(Message::Nav(NavMsg::Go {
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
        let _ = app.send(Message::Tabs(TabsMsg::Open(
            "daily/day/2026-10-13.md".into(),
        )));
        shadow(&mut app);
        let pane = app.focus;
        let _ = app.send(Message::Nav(NavMsg::Go {
            pane,
            target: NavTarget::Period(
                Period::Day,
                NaiveDate::from_ymd_opt(2026, 10, 14).expect("d"),
            ),
        }));
        assert_eq!(app.status.error_text(), Some("not materialized (shadow)"));
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
        let _ = app.send(Message::Tabs(TabsMsg::Open("wp4p/a.md".into())));
        let first = app.focus;
        let _ = app.send(Message::Panes(PanesMsg::SplitDown));
        let second = app.focus;
        assert_eq!(app.panes.len(), 2);
        let _ = app.send(Message::Tabs(TabsMsg::Open("wp4p/b.md".into())));
        let _ = app.send(Message::Tabs(TabsMsg::Open("wp4p/c.md".into())));
        assert_eq!(
            app.panes.get(second).map(|s| s.tabs.len()),
            Some(2),
            "opens into focus"
        );
        assert_eq!(app.panes.get(first).map(|s| s.tabs.len()), Some(1));
        // Re-opening a tab open in another pane focuses it.
        let _ = app.send(Message::Tabs(TabsMsg::Open("wp4p/a.md".into())));
        assert_eq!(app.focus, first);
        let _ = app.send(Message::Panes(PanesMsg::Focus(second)));

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
        app.clear_closed();
        let _ = app.send(Message::Panes(PanesMsg::Close(second)));
        assert_eq!(app.panes.len(), 1);
        assert_eq!(app.closed(), ["wp4p/b.md", "wp4p/c.md"]);
        assert_eq!(app.focus, first);
        let _ = app.send(Message::Tabs(TabsMsg::Close(first, 0)));
        assert_eq!(app.closed().last().map(String::as_str), Some("wp4p/a.md"));
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

    pub(super) fn title(app: &mut App, path: &str, value: &str) {
        let _ = app.send(Message::Reading(ReadingMsg::TitleInput {
            path: path.into(),
            value: value.into(),
        }));
    }

    #[test]
    fn filename_rename_dispatches_explorer_flow_and_retargets() {
        let (_tmp, d) = fake_arc();
        d.create_note("wp2ren/one.md", "body").expect("create");
        let mut app = app_over(&d, None);
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp2ren/one.md".into())));
        assert_eq!(app.reading.title("wp2ren/one.md"), "one");
        title(&mut app, "wp2ren/one.md", "two");
        assert_eq!(app.reading.title("wp2ren/one.md"), "two");
        let _ = app.send(Message::Reading(ReadingMsg::TitleSubmit {
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
        let _ = app.send(Message::Reading(ReadingMsg::TitleFocus {
            path: "wp2ren/two.md".into(),
            focused: false,
        }));
        assert_eq!(app.open_paths(), ["wp2ren/three.md"]);
        title(&mut app, "wp2ren/three.md", "four");
        let _ = app.send(Message::Reading(ReadingMsg::TitleRevert));
        assert_eq!(app.reading.title("wp2ren/three.md"), "three");
        let _ = app.send(Message::Reading(ReadingMsg::TitleSubmit {
            path: "wp2ren/three.md".into(),
        }));
        assert_eq!(app.open_paths(), ["wp2ren/three.md"], "reverted: no rename");
    }

    #[test]
    fn filename_rename_daemon_error_surfaces_and_keeps_tab() {
        let (_tmp, d) = fake_arc();
        d.create_note("wp2err/one.md", "body").expect("create");
        let mut app = app_over(&d, None);
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp2err/one.md".into())));
        // The file vanished behind the editor's back: the daemon's rename fails (NotFound).
        std::fs::remove_file(d.vault_root().join("wp2err/one.md")).expect("rm");
        title(&mut app, "wp2err/one.md", "two");
        let _ = app.send(Message::Reading(ReadingMsg::TitleSubmit {
            path: "wp2err/one.md".into(),
        }));
        assert!(app.status.error_text().is_some(), "daemon error surfaced");
        assert_eq!(app.open_paths(), ["wp2err/one.md"], "tab unchanged");
        assert_eq!(app.reading.title("wp2err/one.md"), "one");
        // A slash is refused before any RPC.
        app.status.notice = None;
        title(&mut app, "wp2err/one.md", "a/b");
        let _ = app.send(Message::Reading(ReadingMsg::TitleSubmit {
            path: "wp2err/one.md".into(),
        }));
        assert_eq!(app.status.error_text(), Some("invalid name"));
    }

    #[test]
    fn filename_rename_flushes_a_dirty_buffer_first() {
        use iced::widget::text_editor::{Action, Edit};
        let (_tmp, d) = fake_arc();
        d.create_note("wp2dirty/one.md", "body").expect("create");
        let mut app = app_over(&d, None);
        let _ = app.send(Message::Explorer(ExplorerMsg::Open(
            "wp2dirty/one.md".into(),
        )));
        let _ = app.send(Message::Source(crate::message::SourceMsg::Action(
            Action::Edit(Edit::Insert('x')),
        )));
        assert!(app.active_doc().is_some_and(|t| t.is_dirty()));
        title(&mut app, "wp2dirty/one.md", "two");
        let _ = app.send(Message::Reading(ReadingMsg::TitleSubmit {
            path: "wp2dirty/one.md".into(),
        }));
        assert_eq!(app.open_paths(), ["wp2dirty/two.md"]);
        assert_eq!(d.read_file("wp2dirty/two.md").expect("renamed").0, b"xbody");
        assert!(app.active_doc().is_some_and(|t| !t.is_dirty()));
    }

    #[test]
    fn reading_prop_toggle_edits_the_buffer_and_rerenders() {
        let (_tmp, d) = fake_arc();
        d.create_note("wp4prop/t.md", "---\nread: false\n---\nx\n")
            .expect("create");
        let mut app = app_over(&d, None);
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp4prop/t.md".into())));
        let _ = app.send(Message::Reading(ReadingMsg::ToggleProp {
            path: "wp4prop/t.md".into(),
            key: "read".into(),
        }));
        let tab = app.active_doc().expect("tab");
        assert_eq!(tab.text(), "---\nread: true\n---\nx\n");
        assert!(tab.is_dirty(), "saved by the normal autosave path");
        let doc = app.reading.doc("wp4prop/t.md").expect("rendered");
        assert_eq!(doc.seq, tab.edit_seq(), "re-rendered from the buffer");
        assert_eq!(
            doc.model.properties[0].values,
            [editor_model::render::PropValue::Bool(true)]
        );
    }

    /// Plan round-6-feedback-todos WP-6: a sidebar boolean property toggles through Reading's
    /// frontmatter path and the panel re-renders it.
    #[test]
    fn sidebar_boolean_toggles_through_the_frontmatter_path() {
        use crate::panels::props::Rendered;
        let (_tmp, d) = fake_arc();
        d.create_note("wp6prop/t.md", "---\nread: false\n---\nx\n")
            .expect("create");
        let mut app = app_over(&d, None);
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp6prop/t.md".into())));
        app.panels.visible = true;
        let _ = app.send(Message::Panels(crate::message::PanelsMsg::Tick));
        let check = |app: &App| {
            app.panels
                .props
                .iter()
                .find(|p| p.key == "read")
                .map(|p| p.value.clone())
        };
        assert_eq!(check(&app), Some(Rendered::Check(false)));
        let _ = app.send(crate::panels::check_press("wp6prop/t.md", "read"));
        assert_eq!(
            app.active_doc().expect("tab").text(),
            "---\nread: true\n---\nx\n"
        );
        let _ = app.send(Message::Panels(crate::message::PanelsMsg::Tick));
        assert_eq!(check(&app), Some(Rendered::Check(true)));
    }

    #[test]
    fn reading_task_toggle_edits_the_buffer_and_rerenders() {
        let (_tmp, d) = fake_arc();
        d.create_note("wp2task/t.md", "- [ ] a\n- [x] b\n")
            .expect("create");
        let mut app = app_over(&d, None);
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp2task/t.md".into())));
        let _ = app.send(Message::Reading(ReadingMsg::ToggleTask {
            path: "wp2task/t.md".into(),
            line: 0,
        }));
        let tab = app.active_doc().expect("tab");
        assert_eq!(tab.text(), "- [x] a\n- [x] b\n");
        assert!(tab.is_dirty(), "saved by the normal autosave path");
        assert_eq!(
            d.read_file("wp2task/t.md").expect("file").0,
            b"- [ ] a\n- [x] b\n"
        );
        let doc = app.reading.doc("wp2task/t.md").expect("rendered");
        assert_eq!(doc.seq, tab.edit_seq(), "re-rendered from the buffer");
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
        app.queue_rpcs();
        (tmp, d, app)
    }

    fn open(app: &mut App, p: &str) {
        let _ = app.send(Message::Explorer(ExplorerMsg::Open(p.into())));
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
        let _ = app.send(Message::Panes(PanesMsg::Back(pane)));
        assert_eq!(active(&app).as_deref(), Some("h3/b.md"));
        let _ = app.send(Message::Panes(PanesMsg::BackFocused));
        assert_eq!(active(&app).as_deref(), Some("h3/a.md"));
        let _ = app.send(Message::Panes(PanesMsg::BackFocused));
        assert_eq!(active(&app).as_deref(), Some("h3/a.md"), "no-op at start");
        let _ = app.send(Message::Panes(PanesMsg::ForwardFocused));
        assert_eq!(active(&app).as_deref(), Some("h3/b.md"));
        assert_eq!(history(&app).len(), 3, "back/forward never push");
        // Re-opening the current entry does not duplicate it; a new open drops forward.
        open(&mut app, "h3/b.md");
        open(&mut app, "h3/a.md");
        assert_eq!(history(&app), ["h3/a.md", "h3/b.md", "h3/a.md"]);
        let _ = app.send(Message::Panes(PanesMsg::Forward(pane)));
        assert_eq!(active(&app).as_deref(), Some("h3/a.md"));
    }

    #[test]
    fn rename_retargets_history_entries() {
        let (_tmp, _d, mut app) = app_with(&["h3r/one.md", "h3r/other.md"], None);
        open(&mut app, "h3r/one.md");
        open(&mut app, "h3r/other.md");
        let _ = app.send(Message::Reading(ReadingMsg::TitleInput {
            path: "h3r/other.md".into(),
            value: "two".into(),
        }));
        let _ = app.send(Message::Reading(ReadingMsg::TitleSubmit {
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
        let _ = app.send(Message::Reading(ReadingMsg::SetMode {
            path: "m3/a.md".into(),
            mode: ViewMode::Split,
        }));
        let _ = app.send(Message::Reading(ReadingMsg::CycleMode));
        assert_eq!(app.reading.mode("m3/a.md"), ViewMode::Source);
        let _ = app.send(Message::Reading(ReadingMsg::CycleMode));
        assert_eq!(app.reading.mode("m3/a.md"), ViewMode::Reading);
    }

    #[test]
    fn mode_menu_opens_and_closes_on_select() {
        let (_tmp, _d, mut app) = app_with(&["m3m/a.md"], None);
        open(&mut app, "m3m/a.md");
        let pane = app.focus;
        let _ = app.send(Message::Panes(PanesMsg::ModeMenu(Some(pane))));
        assert_eq!(app.mode_menu, Some(pane));
        drop(app.view());
        let _ = app.send(Message::Reading(ReadingMsg::SetMode {
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
        let _ = app.send(Message::Switcher(SwitcherMsg::Open));
        drop(app.view());
        let close = app.overlay_close().expect("switcher open");
        assert!(matches!(close, Message::Switcher(SwitcherMsg::Close)));
        let _ = app.send(close);
        assert!(!app.switcher.open);
        let _ = app.send(Message::Palette(PaletteMsg::Open));
        drop(app.view());
        let close = app.overlay_close().expect("palette open");
        assert!(matches!(close, Message::Palette(PaletteMsg::Close)));
        let _ = app.send(close);
        assert!(!app.palette.open);
        assert!(app.overlay_close().is_none());
    }

    /// plan editor-ui-round-3 WP-5: Esc clears a non-empty input first, then closes.
    #[test]
    fn escape_clears_then_closes_switcher_and_palette() {
        let (_tmp, _d, mut app) = app_with(&[], None);
        let _ = app.send(Message::Switcher(SwitcherMsg::Open));
        let empty = app.switcher.results.clone();
        let _ = app.send(Message::Switcher(SwitcherMsg::Query("zzqq".into())));
        let refocus = app.send(Message::Switcher(SwitcherMsg::Escape));
        assert!(
            refocus.units() > 0,
            "editor-round-4 WP-1: clear re-focuses the input"
        );
        assert!(app.switcher.open, "first Esc only clears");
        assert!(app.switcher.query.is_empty());
        assert_eq!(app.switcher.results, empty, "back to the empty-query list");
        let _ = app.send(Message::Switcher(SwitcherMsg::Escape));
        assert!(!app.switcher.open);

        let _ = app.send(Message::Palette(PaletteMsg::Open));
        let empty = app.palette.results.clone();
        let _ = app.send(Message::Palette(PaletteMsg::Query("zzqq".into())));
        let refocus = app.send(Message::Palette(PaletteMsg::Escape));
        assert!(
            refocus.units() > 0,
            "editor-round-4 WP-1: clear re-focuses the input"
        );
        assert!(app.palette.open, "first Esc only clears");
        assert!(app.palette.query.is_empty());
        assert_eq!(app.palette.results, empty);
        let _ = app.send(Message::Palette(PaletteMsg::Escape));
        assert!(!app.palette.open);
    }

    #[test]
    fn side_collapse_and_borders_persist() {
        let tmp = tempfile::tempdir().expect("tmp");
        let state = tmp.path().join("state/editor.json");
        let (_tmp, d, mut app) = app_with(&[], Some(state.clone()));
        assert!(!app.ui.side_collapsed && !app.ui.pane_borders);
        let _ = app.send(Message::Panes(PanesMsg::ToggleSide));
        let _ = app.send(Message::Panes(PanesMsg::ToggleBorders));
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
            Some(0)
        );
        assert_eq!(crate::source::status_notice(Mode::Live), None);
        assert!(crate::source::status_notice(Mode::Shadow).is_some());
        let (_tmp, _d, mut app) = app_with(&["d3/t.md"], None);
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("d3/t.md".into())));
        let tab = app.active_tab().expect("tab");
        // WP-6: the title text never carries the marker; the marker is a separate glyph slot.
        let ps = app.panes.get(app.focus).expect("pane");
        assert_eq!(app.tab_header_title(ps, tab), "t");
        assert_eq!(
            crate::header::marker(
                app.active_doc().expect("doc").is_dirty(),
                app.active_doc().expect("doc").is_saving()
            ),
            ""
        );
        let _ = app.send(Message::Reading(ReadingMsg::SetMode {
            path: "d3/t.md".into(),
            mode: ViewMode::Source,
        }));
        drop(app.view());
        let _ = app.send(Message::Source(crate::message::SourceMsg::Action(
            iced::widget::text_editor::Action::Edit(iced::widget::text_editor::Edit::Insert('y')),
        )));
        assert!(app.active_doc().expect("doc").is_dirty());
        let tab = app.active_tab().expect("tab");
        let ps = app.panes.get(app.focus).expect("pane");
        assert_eq!(app.tab_header_title(ps, tab), "t", "no marker text");
        assert_eq!(crate::header::marker(true, false), "•");
        drop(app.view());
    }

    // ---- editor-feedback-round-2 WP-6: pane header ----

    use super::wp4::{app_over, fake_arc, title};

    #[test]
    fn breadcrumb_filename_edit_runs_in_source_mode_and_renames() {
        let (_tmp, d) = fake_arc();
        d.create_note("wp6ren/one.md", "body").expect("create");
        let mut app = app_over(&d, None);
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp6ren/one.md".into())));
        let _ = app.send(Message::Reading(ReadingMsg::SetMode {
            path: "wp6ren/one.md".into(),
            mode: ViewMode::Source,
        }));
        assert!(!app.reading.ticks() && !app.reading.listens_escape());
        title(&mut app, "wp6ren/one.md", "two");
        // Source-only, yet the blur poll and Escape listener run while the edit is pending.
        assert!(app.reading.ticks(), "blur-poll tick in Source mode");
        assert!(app.reading.listens_escape(), "Escape in Source mode");
        drop(app.view());
        let _ = app.send(Message::Reading(ReadingMsg::TitleRevert));
        assert!(!app.reading.title_pending());
        title(&mut app, "wp6ren/one.md", "two");
        let _ = app.send(Message::Reading(ReadingMsg::TitleSubmit {
            path: "wp6ren/one.md".into(),
        }));
        assert!(
            d.read_file("wp6ren/two.md").is_ok(),
            "renamed via the daemon"
        );
        assert_eq!(app.open_paths(), ["wp6ren/two.md"]);
        assert!(!app.reading.ticks());
    }

    #[test]
    fn tab_titles_follow_the_pref() {
        let (_tmp, d) = fake_arc();
        d.create_note("wp6t/a/n.md", "---\ntitle: Front Title\n---\nbody")
            .expect("create");
        d.create_note("wp6t/b/n.md", "plain").expect("create");
        let mut app = app_over(&d, None);
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp6t/a/n.md".into())));
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp6t/b/n.md".into())));
        let titles = |app: &App| {
            let ps = app.panes.get(app.focus).expect("pane");
            ps.tabs
                .iter()
                .map(|t| app.tab_header_title(ps, t))
                .collect::<Vec<_>>()
        };
        assert_eq!(titles(&app), ["Front Title", "n"]);
        app.prefs.tab_title = crate::header::TabTitle::Unambiguous;
        assert_eq!(titles(&app), ["a/n", "b/n"]);
        app.prefs.tab_title = crate::header::TabTitle::Path;
        assert_eq!(titles(&app), ["wp6t/a/n.md", "wp6t/b/n.md"]);
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
        app.queue_rpcs();
        (tmp, d, app)
    }

    fn open(app: &mut App, p: &str) {
        let _ = app.send(Message::Explorer(ExplorerMsg::Open(p.into())));
    }

    fn type_char(app: &mut App, c: char) {
        let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
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
        // Plan editor-feedback-round-2 WP-1: the collision binds neither claimant.
        assert_eq!(
            notes,
            [
                "close_tab: Ctrl+W conflicts with a (not bound)",
                "a: Ctrl+W conflicts with close_tab (not bound)"
            ]
        );
    }

    #[test]
    fn default_action_chords_run_presets_and_are_noops_when_absent() {
        let (_tmp, d, mut app) = app_with(&[("wp4k/a.md", "x")]);
        // No actions registered: Ctrl+O / Ctrl+R do nothing, and no "Open today" entry exists.
        let _ = app.send(Message::Palette(PaletteMsg::Key(c("Ctrl+O"))));
        let _ = app.send(Message::Palette(PaletteMsg::Key(c("Ctrl+R"))));
        assert!(app.status.error_text().is_none(), "{:?}", app.status.notice);
        assert!(!app.palette.entries.iter().any(|e| e.title == "Open today"));
        let periods = register_counting(&d, "period.open");
        let lints = register_counting(&d, "lint-note");
        app.palette.invalidate();
        let _ = app.send(Message::Palette(PaletteMsg::Key(c("Ctrl+O"))));
        open(&mut app, "wp4k/a.md");
        let _ = app.send(Message::Palette(PaletteMsg::Key(c("Ctrl+R"))));
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
        let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
            path: "wp4c/a.md".into(),
            mode: ViewMode::Source,
        }));
        type_char(&mut app, 'x');
        let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
        assert!(app.open_paths().is_empty(), "closed after the save");
        assert_eq!(d.read_file("wp4c/a.md").expect("file").0, b"xa");

        // Conflict: the file changes on disk under a dirty buffer → the close is cancelled.
        open(&mut app, "wp4c/b.md");
        let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
            path: "wp4c/b.md".into(),
            mode: ViewMode::Source,
        }));
        type_char(&mut app, 'y');
        let (_, h) = d.read_file("wp4c/b.md").expect("file");
        d.write_note("wp4c/b.md", "theirs", &h)
            .expect("external write");
        let _ = app.send(Message::Tabs(TabsMsg::Close(app.focus, 0)));
        assert_eq!(app.open_paths(), ["wp4c/b.md"], "close waits on the dialog");
        assert!(app.dialog.is_some(), "plan editor-data-safety WP-2: dialog");
        assert!(!app.exiting);
    }

    #[test]
    fn quit_flushes_dirty_buffers_then_exits_and_cancels_on_failure() {
        let (_tmp, d, mut app) = app_with(&[("wp4q/a.md", "a")]);
        open(&mut app, "wp4q/a.md");
        let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
            path: "wp4q/a.md".into(),
            mode: ViewMode::Source,
        }));
        type_char(&mut app, 'x');
        let (_, h) = d.read_file("wp4q/a.md").expect("file");
        d.write_note("wp4q/a.md", "theirs", &h)
            .expect("external write");
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert!(!app.exiting, "a failed save stops the quit");
        assert!(app.dialog.is_some(), "plan editor-data-safety WP-2: dialog");

        let (_tmp2, d2, mut app2) = app_with(&[("wp4q/b.md", "b")]);
        open(&mut app2, "wp4q/b.md");
        let _ = app2.send(Message::Reading(crate::message::ReadingMsg::SetMode {
            path: "wp4q/b.md".into(),
            mode: ViewMode::Source,
        }));
        type_char(&mut app2, 'y');
        let _ = app2.send(Message::Tabs(TabsMsg::Quit));
        assert!(app2.exiting);
        assert_eq!(d2.read_file("wp4q/b.md").expect("file").0, b"yb");
    }

    #[test]
    fn go_to_tab_selects_in_the_focused_pane_and_ignores_out_of_range() {
        let (_tmp, _d, mut app) = app_with(&[("wp4g/a.md", "a"), ("wp4g/b.md", "b")]);
        open(&mut app, "wp4g/a.md");
        open(&mut app, "wp4g/b.md");
        let _ = app.send(Message::Palette(PaletteMsg::Key(c("Alt+1"))));
        assert_eq!(app.active_tab().map(|t| t.path.as_str()), Some("wp4g/a.md"));
        let _ = app.send(Message::Tabs(TabsMsg::GoTo(2)));
        assert_eq!(app.active_tab().map(|t| t.path.as_str()), Some("wp4g/b.md"));
        let _ = app.send(Message::Tabs(TabsMsg::GoTo(5)));
        assert_eq!(app.active_tab().map(|t| t.path.as_str()), Some("wp4g/b.md"));
    }

    #[test]
    fn find_selects_matches_in_source_with_wrap() {
        let (_tmp, _d, mut app) = app_with(&[("wp4f/a.md", "one Foo\ntwo foo\nfoo")]);
        open(&mut app, "wp4f/a.md");
        let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
            path: "wp4f/a.md".into(),
            mode: ViewMode::Source,
        }));
        let _ = app.send(Message::Palette(PaletteMsg::Key(c("Ctrl+F"))));
        assert!(app.panes.get(app.focus).is_some_and(|ps| ps.find.is_some()));
        let _ = app.send(Message::Find(FindMsg::Query("FOO".into())));
        let sel = |app: &App| app.active_doc().and_then(|t| t.selection());
        let line = |app: &App| app.active_doc().map(|t| t.cursor().position.line);
        assert_eq!(sel(&app).as_deref(), Some("Foo"));
        assert_eq!(line(&app), Some(0));
        let _ = app.send(Message::Find(FindMsg::Submit));
        assert_eq!(line(&app), Some(1));
        let _ = app.send(Message::Find(FindMsg::Next));
        assert_eq!(line(&app), Some(2));
        let _ = app.send(Message::Find(FindMsg::Next));
        assert_eq!(line(&app), Some(0), "wraps forward");
        let _ = app.send(Message::Modifiers(iced::keyboard::Modifiers::SHIFT));
        let _ = app.send(Message::Find(FindMsg::Submit));
        assert_eq!(line(&app), Some(2), "Shift+Enter wraps back");
        let bar = app
            .panes
            .get(app.focus)
            .and_then(|ps| ps.find.clone())
            .expect("bar");
        assert_eq!(bar.count(), "3/3");
        drop(app.view());
        let _ = app.send(Message::Find(FindMsg::Close));
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

// ---------------------------------------------------------------------------------------------
// editor-feedback-round-2 WP-3: file auto-save timing and flush points (FakeDaemon)
// ---------------------------------------------------------------------------------------------

mod round2_wp3 {
    use iced::widget::text_editor::{Action, Edit};

    use super::*;
    use crate::message::{ExplorerMsg, SourceMsg};
    use crate::panes;
    use crate::source::SaveOutcome;

    fn setup(notes: &[&str]) -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
        let (tmp, d) = fake();
        let d = Arc::new(d);
        for p in notes {
            d.create_note(p, "body").expect("create");
        }
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let mut app = App::new(Some(dc), None);
        app.queue_rpcs();
        for p in notes {
            let _ = app.send(Message::Explorer(ExplorerMsg::Open((*p).to_owned())));
        }
        (tmp, d, app)
    }

    fn tab<'a>(app: &'a mut App, path: &str) -> &'a mut crate::source::SourceBuffer {
        app.doc_mut(path).expect("open")
    }

    fn type_x(app: &mut App) {
        let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
            Edit::Insert('x'),
        ))));
    }

    fn dirty_in_place(app: &mut App, path: &str) {
        let _ = tab(app, path).perform(
            Action::Edit(Edit::Insert('y')),
            false,
            std::time::Instant::now(),
        );
    }

    fn select(app: &mut App, path: &str) {
        let (p, i) = panes::find(&app.panes, path).expect("open");
        let _ = app.send(Message::Tabs(TabsMsg::Select(p, i)));
    }

    #[test]
    fn autosave_delay_is_one_second() {
        assert_eq!(crate::source::AUTOSAVE_DELAY, Duration::from_secs(1));
    }

    #[test]
    fn switching_tabs_away_from_a_dirty_note_starts_its_save() {
        let (_tmp, _d, mut app) = setup(&["w3s/a.md", "w3s/b.md"]);
        select(&mut app, "w3s/a.md");
        type_x(&mut app);
        assert!(tab(&mut app, "w3s/a.md").is_dirty());
        assert!(!tab(&mut app, "w3s/a.md").is_saving());
        select(&mut app, "w3s/b.md");
        assert!(
            tab(&mut app, "w3s/a.md").is_saving(),
            "flush started on switch"
        );
        // Alt+N back: leaving the clean tab starts nothing.
        let _ = app.send(Message::Tabs(TabsMsg::GoTo(1)));
        assert!(!tab(&mut app, "w3s/b.md").is_saving());
    }

    #[test]
    fn unfocus_flushes_every_dirty_note() {
        let (_tmp, _d, mut app) = setup(&["w3u/a.md", "w3u/b.md", "w3u/c.md"]);
        select(&mut app, "w3u/a.md");
        type_x(&mut app);
        dirty_in_place(&mut app, "w3u/b.md");
        let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
        assert!(tab(&mut app, "w3u/a.md").is_saving());
        assert!(tab(&mut app, "w3u/b.md").is_saving());
        assert!(!tab(&mut app, "w3u/c.md").is_saving(), "clean: no-op");
    }

    #[test]
    fn flush_skips_conflicted_and_shadow_buffers() {
        let (_tmp, _d, mut app) = setup(&["w3c/a.md", "w3c/b.md"]);
        select(&mut app, "w3c/a.md");
        type_x(&mut app);
        let t = tab(&mut app, "w3c/a.md");
        let job = t.begin_save(false).expect("job");
        let _ = t.finish_save(job.seq, SaveOutcome::Conflict(ContentHash::from_hex("abc")));
        assert!(t.conflict().is_some());
        select(&mut app, "w3c/b.md");
        let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
        let t = tab(&mut app, "w3c/a.md");
        assert!(!t.is_saving(), "a conflicted buffer is never auto-saved");
        assert!(t.is_dirty());

        let _ = app.send(Message::Status(StatusMsg::Notification(
            Notification::ModeChanged { mode: Mode::Shadow },
        )));
        dirty_in_place(&mut app, "w3c/b.md");
        select(&mut app, "w3c/a.md");
        let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
        assert!(!tab(&mut app, "w3c/b.md").is_saving(), "shadow: no-op");
    }

    #[test]
    fn edit_during_a_flush_started_save_requeues() {
        let (_tmp, _d, mut app) = setup(&["w3r/a.md", "w3r/b.md"]);
        select(&mut app, "w3r/a.md");
        type_x(&mut app);
        let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
        let t = tab(&mut app, "w3r/a.md");
        assert!(t.is_saving());
        let seq = t.edit_seq();
        type_x(&mut app);
        // A second flush while saving only queues the follow-up.
        let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
        let t = tab(&mut app, "w3r/a.md");
        let again = t
            .finish_save(seq, SaveOutcome::Saved(ContentHash::from_hex("h1")))
            .expect("saved");
        assert!(again, "the newer edit re-queues a save");
        assert!(t.is_dirty());
    }

    /// Starts a flush (async task, left in flight) and returns its seq.
    fn in_flight(app: &mut App, path: &str) -> u64 {
        select(app, path);
        type_x(app);
        let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
        let t = tab(app, path);
        assert!(t.is_saving());
        t.edit_seq()
    }

    fn land(app: &mut App, path: &str, seq: u64, outcome: SaveOutcome) {
        let _ = app.send(Message::Source(SourceMsg::Saved {
            path: path.to_owned(),
            seq,
            outcome,
            after: None,
        }));
    }

    #[test]
    fn quit_waits_for_an_in_flight_save_then_exits() {
        let (_tmp, _d, mut app) = setup(&["w3q/a.md"]);
        let seq = in_flight(&mut app, "w3q/a.md");
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert!(!app.exiting, "waits instead of refusing");
        assert!(app.status.error_text().is_none(), "{:?}", app.status.notice);
        land(
            &mut app,
            "w3q/a.md",
            seq,
            SaveOutcome::Saved(ContentHash::from_hex("h1")),
        );
        assert!(app.exiting, "exits once the save lands clean");
    }

    #[test]
    fn quit_while_saving_then_conflict_cancels() {
        let (_tmp, _d, mut app) = setup(&["w3qc/a.md"]);
        let seq = in_flight(&mut app, "w3qc/a.md");
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        land(
            &mut app,
            "w3qc/a.md",
            seq,
            SaveOutcome::Conflict(ContentHash::from_hex("abc")),
        );
        assert!(!app.exiting);
        assert!(app.quit.is_none(), "quit state cleared, not hanging");
        assert!(app.dialog.is_some(), "plan editor-data-safety WP-2: dialog");
    }

    #[test]
    fn close_while_saving_closes_after_it_lands() {
        let (_tmp, _d, mut app) = setup(&["w3x/a.md", "w3x/b.md"]);
        let seq = in_flight(&mut app, "w3x/a.md");
        let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
        assert!(panes::find(&app.panes, "w3x/a.md").is_some(), "still open");
        assert!(app.status.error_text().is_none(), "{:?}", app.status.notice);
        land(
            &mut app,
            "w3x/a.md",
            seq,
            SaveOutcome::Saved(ContentHash::from_hex("h1")),
        );
        assert!(panes::find(&app.panes, "w3x/a.md").is_none(), "closed");
        assert!(panes::find(&app.panes, "w3x/b.md").is_some());
    }
}

// ---------------------------------------------------------------------------------------------
// editor-feedback-round-2 WP-7: shared documents + Quick Switch modes (FakeDaemon)
// ---------------------------------------------------------------------------------------------

mod round2_wp7 {
    use std::collections::HashSet;

    use iced::widget::pane_grid::Pane;
    use iced::widget::text_editor::{Action, Edit};

    use super::*;
    use crate::app::Tab;
    use crate::header::QuickSwitchOpen;
    use crate::message::{ExplorerMsg, PanesMsg, SourceMsg, SwitcherMsg};
    use crate::reading::ViewMode;
    use crate::source::SaveOutcome;

    /// Opens `notes` into one pane (rpc inline).
    fn setup(notes: &[&str]) -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
        let (tmp, d) = fake();
        let d = Arc::new(d);
        for p in notes {
            d.create_note(p, "body").expect("create");
        }
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let mut app = App::new(Some(dc), None);
        app.queue_rpcs();
        for p in notes {
            let _ = app.send(Message::Explorer(ExplorerMsg::Open((*p).to_owned())));
        }
        (tmp, d, app)
    }

    fn type_x(app: &mut App) {
        let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
            Edit::Insert('x'),
        ))));
    }

    fn quick(app: &mut App, mode: QuickSwitchOpen, path: &str) {
        app.prefs.quick_switch_open = mode;
        let _ = app.send(Message::Switcher(SwitcherMsg::Pick(path.to_owned())));
    }

    fn split(app: &mut App) -> Pane {
        let _ = app.send(Message::Panes(PanesMsg::SplitRight));
        app.focus
    }

    /// Every `(pane, tab id)` showing `path`.
    fn views(app: &App, path: &str) -> Vec<(Pane, u64)> {
        app.panes
            .iter()
            .flat_map(|(p, ps)| {
                ps.tabs
                    .iter()
                    .filter(|t| t.path == path)
                    .map(move |t| (*p, t.id))
            })
            .collect()
    }

    fn tab_paths(app: &App, pane: Pane) -> Vec<String> {
        app.panes
            .get(pane)
            .map(|ps| ps.tabs.iter().map(|t| t.path.clone()).collect())
            .unwrap_or_default()
    }

    /// `w7/a.md` open in the left pane and duplicated into the (focused) right pane.
    fn duplicated() -> (tempfile::TempDir, Arc<FakeDaemon>, App, Pane, Pane) {
        let (tmp, d, mut app) = setup(&["w7/a.md"]);
        let left = app.focus;
        let right = split(&mut app);
        quick(&mut app, QuickSwitchOpen::Duplicate, "w7/a.md");
        assert_eq!(views(&app, "w7/a.md").len(), 2);
        (tmp, d, app, left, right)
    }

    fn saved(app: &mut App, path: &str, seq: u64) {
        let _ = app.send(Message::Source(SourceMsg::Saved {
            path: path.to_owned(),
            seq,
            outcome: SaveOutcome::Saved(ContentHash::from_hex("h1")),
            after: None,
        }));
    }

    #[test]
    fn image_pins_are_per_view() {
        use crate::image_cache::{Decoded, ImageCache, view_owner};
        let decoded = || Decoded {
            handle: iced::widget::image::Handle::from_rgba(1, 1, vec![0; 4]),
            width: 1,
            height: 1,
            bytes: 4,
        };
        let mut c = ImageCache::with_budget(1 << 20);
        let (v1, v2) = (view_owner("a.md", 1), view_owner("a.md", 2));
        assert!(c.show(&v1, "k"));
        assert!(!c.show(&v2, "k"), "the second view waits on the same load");
        assert!(c.loaded("k", Some(decoded())));
        // show / show / hide: the other view keeps the pin.
        c.hide(&v1, "k");
        assert!(c.contains("k"), "the other view still shows it");
        assert_eq!(c.pins("k"), 1);
        c.hide(&v2, "k");
        assert!(!c.contains("k"), "no view shows it: released");
        // A bare-path release (last tab closed) covers every view.
        assert!(c.show(&v1, "k"));
        assert!(c.loaded("k", Some(decoded())));
        c.show(&v2, "k");
        c.release_owner("a.md");
        assert!(c.is_empty());
        // ... and never another note whose path merely starts with it.
        assert!(c.show(&view_owner("a.md.md", 3), "j"));
        assert!(c.loaded("j", Some(decoded())));
        c.retain_owner("a.md", &HashSet::new());
        assert!(c.contains("j"));
    }

    #[test]
    fn duplicate_tabs_share_one_document_and_its_edits() {
        let (_tmp, _d, mut app, left, right) = duplicated();
        assert_eq!(app.docs.len(), 1);
        type_x(&mut app);
        assert!(app.doc("w7/a.md").is_some_and(|d| d.is_dirty()));
        app.focus = left;
        let text = app.active_doc().expect("doc").text();
        assert!(text.starts_with('x'), "{text}");
        assert_eq!(tab_paths(&app, right), ["w7/a.md"]);
    }

    #[test]
    fn save_completion_with_two_tabs_of_one_path() {
        let (_tmp, _d, mut app, _left, _right) = duplicated();
        type_x(&mut app);
        let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
        let seq = app.doc("w7/a.md").expect("doc").edit_seq();
        assert!(app.doc("w7/a.md").expect("doc").is_saving());
        saved(&mut app, "w7/a.md", seq);
        let doc = app.doc("w7/a.md").expect("doc");
        assert!(!doc.is_dirty() && !doc.is_saving());
        assert_eq!(views(&app, "w7/a.md").len(), 2, "both tabs stay");
        // The autosave timer is path-keyed too: the one (clean) document answers it.
        let _ = app.send(Message::Source(SourceMsg::AutosaveDue {
            path: "w7/a.md".into(),
            seq,
        }));
        assert!(!app.doc("w7/a.md").expect("doc").is_saving(), "clean");
    }

    #[test]
    fn closing_one_duplicate_keeps_the_others_document_and_reading_state() {
        let (_tmp, _d, mut app, left, right) = duplicated();
        assert!(app.reading.doc("w7/a.md").is_some(), "rendered");
        let _ = app.send(Message::Tabs(TabsMsg::Close(right, 0)));
        let v = views(&app, "w7/a.md");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].0, left);
        assert!(app.doc("w7/a.md").is_some());
        assert!(app.reading.doc("w7/a.md").is_some(), "reading state kept");
        let _ = app.send(Message::Tabs(TabsMsg::Close(left, 0)));
        assert!(app.doc("w7/a.md").is_none(), "dropped with the last tab");
        assert!(app.reading.doc("w7/a.md").is_none());
    }

    #[test]
    fn e1_closing_either_dirty_duplicate_closes_exactly_that_tab() {
        for close_left in [true, false] {
            // Different panes (the save-before-close runs inline).
            let (_tmp, d, mut app, left, right) = duplicated();
            type_x(&mut app);
            let target = if close_left { left } else { right };
            let all = views(&app, "w7/a.md");
            let gone = all.iter().find(|v| v.0 == target).expect("view").1;
            let _ = app.send(Message::Tabs(TabsMsg::Close(target, 0)));
            let now = views(&app, "w7/a.md");
            assert_eq!(now.len(), 1, "close_left={close_left}");
            assert_ne!(now[0].1, gone, "close_left={close_left}");
            assert_ne!(now[0].0, target);
            assert!(app.doc("w7/a.md").is_some_and(|d| !d.is_dirty()), "saved");
            assert!(d.read_file("w7/a.md").expect("file").0.starts_with(b"x"));

            // Same pane, closed while a save is in flight (the close waits on it).
            let (_tmp, _d, mut app) = setup(&["w7s/a.md"]);
            let pane = app.focus;
            let extra = Tab::new("w7s/a.md");
            let extra_id = extra.id;
            let first_id = app.active_note().expect("tab").id;
            if let Some(ps) = app.panes.get_mut(pane) {
                ps.tabs.push(extra);
            }
            type_x(&mut app);
            let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
            let seq = app.doc("w7s/a.md").expect("doc").edit_seq();
            let i = usize::from(!close_left);
            let _ = app.send(Message::Tabs(TabsMsg::Close(pane, i)));
            assert_eq!(views(&app, "w7s/a.md").len(), 2, "waits on the save");
            saved(&mut app, "w7s/a.md", seq);
            let kept = if close_left { extra_id } else { first_id };
            assert_eq!(
                views(&app, "w7s/a.md"),
                [(pane, kept)],
                "close_left={close_left}"
            );
            assert!(app.doc("w7s/a.md").is_some(), "shared document intact");
        }
    }

    #[test]
    fn e2_shared_document_is_shaped_from_the_union_of_its_views() {
        let (_tmp, d, mut app, left, right) = duplicated();
        d.create_note("w7/b.md", "other").expect("create");
        let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
            path: "w7/a.md".into(),
            mode: ViewMode::Source,
        }));
        assert!(app.doc("w7/a.md").expect("doc").is_shaped());
        // The right pane switches to another note: the left pane still shows a.md.
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("w7/b.md".into())));
        assert_eq!(app.focus, right);
        assert_eq!(tab_paths(&app, right), ["w7/a.md", "w7/b.md"]);
        assert!(
            app.doc("w7/a.md").expect("doc").is_shaped(),
            "still visible in the left pane"
        );
        // Hidden in both panes → plain text.
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("w7/b.md".into())));
        let _ = app.send(Message::Tabs(TabsMsg::Close(left, 0)));
        assert!(app.doc("w7/a.md").is_some_and(|d| !d.is_shaped()));
    }

    #[test]
    fn quit_with_duplicates_awaits_the_document_once() {
        let (_tmp, _d, mut app, _left, _right) = duplicated();
        type_x(&mut app);
        let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
        let seq = app.doc("w7/a.md").expect("doc").edit_seq();
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert_eq!(app.quit_waits(), Some(1));
        saved(&mut app, "w7/a.md", seq);
        assert!(app.exiting);
    }

    #[test]
    fn move_carries_unsaved_edits_and_undo() {
        let (_tmp, _d, mut app) = setup(&["w7m/a.md"]);
        let left = app.focus;
        let id = app.active_note().expect("tab").id;
        type_x(&mut app);
        let right = split(&mut app);
        quick(&mut app, QuickSwitchOpen::Move, "w7m/a.md");
        assert!(tab_paths(&app, left).is_empty());
        assert_eq!(views(&app, "w7m/a.md"), [(right, id)], "the same tab moved");
        assert_eq!(app.focus, right);
        let doc = app.active_doc().expect("doc");
        assert!(doc.is_dirty() && doc.text().starts_with('x'));
        let _ = app.send(Message::Source(SourceMsg::Undo));
        assert_eq!(app.active_doc().expect("doc").text(), "body");
    }

    #[test]
    fn quick_switch_modes_cover_other_pane_active_pane_and_nowhere() {
        for mode in QuickSwitchOpen::ALL {
            // Open in another pane.
            let (_tmp, d, mut app) = setup(&["w7q/a.md"]);
            d.create_note("w7q/n.md", "new").expect("create");
            let left = app.focus;
            let right = split(&mut app);
            quick(&mut app, mode, "w7q/a.md");
            let (want_left, want_right, focus): (&[&str], &[&str], Pane) = match mode {
                QuickSwitchOpen::Switch => (&["w7q/a.md"], &[], left),
                QuickSwitchOpen::Move => (&[], &["w7q/a.md"], right),
                QuickSwitchOpen::Duplicate => (&["w7q/a.md"], &["w7q/a.md"], right),
            };
            assert_eq!(tab_paths(&app, left), want_left, "{mode:?}");
            assert_eq!(tab_paths(&app, right), want_right, "{mode:?}");
            assert_eq!(app.focus, focus, "{mode:?}");
            assert_eq!(app.docs.len(), 1, "{mode:?}");

            // Open nowhere: opens in the active pane.
            quick(&mut app, mode, "w7q/n.md");
            assert_eq!(
                app.active_note().map(|t| t.path.as_str()),
                Some("w7q/n.md"),
                "{mode:?}"
            );
            assert_eq!(app.focus, focus, "{mode:?}");

            // Already in the active pane: just focused.
            let (_tmp2, _d2, mut app2) = setup(&["w7q/a.md", "w7q/b.md"]);
            let pane = app2.focus;
            quick(&mut app2, mode, "w7q/a.md");
            assert_eq!(tab_paths(&app2, pane), ["w7q/a.md", "w7q/b.md"], "{mode:?}");
            assert_eq!(
                app2.active_note().map(|t| t.path.as_str()),
                Some("w7q/a.md"),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn restore_keeps_the_same_path_in_two_panes() {
        let (tmp, d) = fake();
        let d = Arc::new(d);
        d.create_note("w7r/a.md", "body").expect("create");
        let state = tmp.path().join("editor.json");
        {
            let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
            let mut app = App::new(Some(dc), Some(state.clone()));
            app.queue_rpcs();
            let _ = app.send(Message::Explorer(ExplorerMsg::Open("w7r/a.md".into())));
            split(&mut app);
            quick(&mut app, QuickSwitchOpen::Duplicate, "w7r/a.md");
            assert_eq!(views(&app, "w7r/a.md").len(), 2);
        }
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let app = App::new(Some(dc), Some(state));
        assert_eq!(views(&app, "w7r/a.md").len(), 2, "both tabs restore");
        assert_eq!(app.docs.len(), 1, "one document");
    }

    #[test]
    fn rename_retargets_every_tab_and_the_document_key() {
        let (_tmp, d, mut app, _left, _right) = duplicated();
        let ids: Vec<u64> = views(&app, "w7/a.md").iter().map(|v| v.1).collect();
        d.rename("w7/a.md", "w7/z.md").expect("rename");
        let _ = app.send(Message::Explorer(ExplorerMsg::Done(
            crate::explorer::FileOpDone {
                op: crate::explorer::FileOp::Rename {
                    from: "w7/a.md".into(),
                    to: "w7/z.md".into(),
                },
                result: Ok("w7/z.md".into()),
            },
        )));
        assert!(views(&app, "w7/a.md").is_empty());
        let now: Vec<u64> = views(&app, "w7/z.md").iter().map(|v| v.1).collect();
        assert_eq!(now, ids, "every tab follows, ids kept");
        assert!(app.doc("w7/a.md").is_none());
        assert_eq!(app.doc("w7/z.md").map(|d| d.path()), Some("w7/z.md"));
        assert_eq!(app.docs.len(), 1);
        assert!(app.closed().is_empty(), "SR13: a rename closes no tab");
    }
}

/// plan editor-data-safety WP-1: daemon reconnect (all with `FakeDaemon`).
mod data_safety_wp1 {
    use std::sync::atomic::{AtomicU32, Ordering};

    use iced::widget::text_editor::{Action, Edit};

    use super::*;
    use crate::daemon::Connector;
    use crate::message::{ExplorerMsg, SourceMsg};
    use crate::source::SaveOutcome;

    /// A connector over `d` that fails `fails` times first; returns it and its call counter.
    fn flaky(d: &Arc<FakeDaemon>, fails: u32) -> (Connector, Arc<AtomicU32>) {
        let calls = Arc::new(AtomicU32::new(0));
        let (d, n) = (Arc::clone(d), Arc::clone(&calls));
        let c: Connector = Arc::new(move || {
            if n.fetch_add(1, Ordering::SeqCst) < fails {
                Err(DaemonError::Disconnected)
            } else {
                Ok(Arc::clone(&d) as Arc<dyn DaemonClient>)
            }
        });
        (c, calls)
    }

    fn setup(notes: &[&str], fails: u32) -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
        let (tmp, d) = fake();
        let d = Arc::new(d);
        for p in notes {
            d.create_note(p, "body").expect("create");
        }
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let (c, _) = flaky(&d, fails);
        let mut app = App::new(Some(dc), None).with_connector(c);
        app.queue_rpcs();
        for p in notes {
            let _ = app.send(Message::Explorer(ExplorerMsg::Open((*p).to_owned())));
        }
        (tmp, d, app)
    }

    fn type_x(app: &mut App) {
        let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
            Edit::Insert('x'),
        ))));
    }

    fn reconnect(app: &mut App) {
        let _ = app.send(Message::Status(StatusMsg::Disconnected));
        let _ = app.send(Message::Status(StatusMsg::ReconnectDue));
    }

    fn write_disk(d: &FakeDaemon, path: &str, body: &str) {
        std::fs::write(d.vault_root().join(path), body).expect("write");
    }

    fn doc<'a>(app: &'a mut App, path: &str) -> &'a mut crate::source::SourceBuffer {
        app.docs.get_mut(path).expect("doc")
    }

    fn land(app: &mut App, path: &str, seq: u64, outcome: SaveOutcome) {
        let _ = app.send(Message::Source(SourceMsg::Saved {
            path: path.to_owned(),
            seq,
            outcome,
            after: None,
        }));
    }

    #[test]
    fn backoff_is_capped_and_never_zero() {
        assert_eq!(crate::app::reconnect_delay(0), Duration::from_millis(500));
        assert_eq!(crate::app::reconnect_delay(1), Duration::from_secs(1));
        assert_eq!(crate::app::reconnect_delay(30), Duration::from_secs(5));
    }

    #[test]
    fn failing_connector_then_success_installs_a_new_client_and_bumps_the_epoch() {
        let (_tmp, _d, mut app) = setup(&[], 2);
        assert_eq!(app.connection_epoch(), 0);
        let _ = app.send(Message::Status(StatusMsg::Disconnected));
        assert!(app.status.line().starts_with("reconnecting"));
        for _ in 0..2 {
            let _ = app.send(Message::Status(StatusMsg::ReconnectDue));
            assert!(!app.status.connected);
            assert_eq!(app.connection_epoch(), 0);
        }
        let _ = app.send(Message::Status(StatusMsg::ReconnectDue));
        assert!(app.status.connected);
        assert!(!app.status.reconnecting);
        assert_eq!(app.connection_epoch(), 1);
        assert!(app.status.line().starts_with("connected"));
    }

    #[test]
    fn boot_without_a_daemon_connects_once_the_connector_succeeds() {
        let (_tmp, d) = fake();
        let d = Arc::new(d);
        let (c, calls) = flaky(&d, 1);
        let mut app = App::new(None, None).with_connector(c);
        app.queue_rpcs();
        assert!(app.daemon.is_none());
        // The subscription reports "no connection" at boot.
        reconnect(&mut app);
        assert!(app.daemon.is_none() && app.status.reconnecting);
        let _ = app.send(Message::Status(StatusMsg::ReconnectDue));
        assert!(app.daemon.is_some());
        assert!(app.status.connected);
        assert_eq!(app.status.mode, Mode::Live);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(
            !app.explorer.paths.is_empty(),
            "file tree loaded on connect"
        );
    }

    #[test]
    fn dirty_document_whose_save_failed_is_saved_after_reconnect() {
        let (_tmp, _d, mut app) = setup(&["r1/a.md"], 0);
        type_x(&mut app);
        let _ = app.send(Message::Source(SourceMsg::Save));
        let seq = doc(&mut app, "r1/a.md").edit_seq();
        land(
            &mut app,
            "r1/a.md",
            seq,
            SaveOutcome::Failed("daemon disconnected".into()),
        );
        assert!(!doc(&mut app, "r1/a.md").is_saving());
        reconnect(&mut app);
        let t = doc(&mut app, "r1/a.md");
        assert!(t.is_saving(), "resync schedules the save");
        assert!(t.conflict().is_none());
    }

    #[test]
    fn dirty_document_whose_disk_changed_gets_the_conflict_bar() {
        let (_tmp, d, mut app) = setup(&["r2/a.md"], 0);
        type_x(&mut app);
        write_disk(&d, "r2/a.md", "changed elsewhere");
        reconnect(&mut app);
        let t = doc(&mut app, "r2/a.md");
        assert!(t.conflict().is_some());
        assert!(!t.is_saving(), "no save over a changed file");
        assert!(t.is_dirty());
    }

    #[test]
    fn clean_document_reloads_when_the_disk_differs() {
        let (_tmp, d, mut app) = setup(&["r3/a.md"], 0);
        write_disk(&d, "r3/a.md", "new text");
        reconnect(&mut app);
        let t = doc(&mut app, "r3/a.md");
        assert_eq!(t.text(), "new text");
        assert!(!t.is_dirty());
    }

    #[test]
    fn dirty_document_whose_text_already_landed_becomes_clean() {
        let (_tmp, d, mut app) = setup(&["r4/a.md"], 0);
        type_x(&mut app);
        let text = doc(&mut app, "r4/a.md").text();
        write_disk(&d, "r4/a.md", &text);
        reconnect(&mut app);
        let t = doc(&mut app, "r4/a.md");
        assert!(!t.is_dirty() && !t.is_saving() && t.conflict().is_none());
    }

    /// Audit E1: the reconnect arrives before the old save's failure completion; the resync
    /// waits for it, and the queued `save_again` is not stranded.
    #[test]
    fn reconnect_before_the_old_saves_failure_resyncs_after_it() {
        let (_tmp, _d, mut app) = setup(&["r5/a.md"], 0);
        type_x(&mut app);
        let _ = app.send(Message::Source(SourceMsg::Save));
        let seq = doc(&mut app, "r5/a.md").edit_seq();
        type_x(&mut app);
        // A second save request while the first is in flight queues `save_again`.
        let _ = app.send(Message::Source(SourceMsg::Save));
        reconnect(&mut app);
        assert_eq!(app.connection_epoch(), 1);
        assert!(doc(&mut app, "r5/a.md").is_saving(), "still the old save");
        land(
            &mut app,
            "r5/a.md",
            seq,
            SaveOutcome::Failed("daemon disconnected".into()),
        );
        let t = doc(&mut app, "r5/a.md");
        assert!(
            t.is_saving(),
            "resync launched a fresh save after the failure"
        );
        assert!(t.is_dirty());
        assert!(!t.take_resync_pending());
    }
}

/// plan editor-data-safety WP-2: unsaved-edits dialog; missing-file conflict (all `FakeDaemon`).
mod data_safety_wp2 {
    use iced::widget::text_editor::{Action, Edit};

    use super::*;
    use crate::dialog::{DialogMsg, LossReason, OnOk};
    use crate::header::QuickSwitchOpen;
    use crate::message::{ExplorerMsg, PaletteMsg, PanesMsg, SourceMsg, SwitcherMsg};
    use crate::source::SaveOutcome;

    fn setup(notes: &[&str]) -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
        let (tmp, d) = fake();
        let d = Arc::new(d);
        for p in notes {
            d.create_note(p, "body").expect("create");
        }
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let mut app = App::new(Some(dc), None);
        app.queue_rpcs();
        for p in notes {
            let _ = app.send(Message::Explorer(ExplorerMsg::Open((*p).to_owned())));
        }
        (tmp, d, app)
    }

    fn type_x(app: &mut App) {
        let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
            Edit::Insert('x'),
        ))));
    }

    fn dialog_notes(app: &App) -> Vec<(String, LossReason)> {
        app.dialog
            .as_ref()
            .and_then(|d| d.unsaved_edits())
            .map(|u| u.notes.clone())
            .unwrap_or_default()
    }

    fn deleted(app: &mut App, path: &str) {
        let _ = app.send(Message::Status(StatusMsg::Notification(
            Notification::FileChanged {
                path: path.to_owned(),
                kind: cv_protocol::ChangeKind::Deleted,
                hash: None,
                origin: cv_protocol::Origin::Daemon,
            },
        )));
    }

    /// Runs `job` (a `begin_save` / `keep_mine` result) on the daemon and lands it.
    fn land(app: &mut App, d: &FakeDaemon, job: Option<crate::source::SaveJob>) {
        let job = job.expect("a save starts");
        let outcome = job.run(d);
        let _ = app.send(job.done(outcome));
    }

    fn save(app: &mut App, d: &FakeDaemon, path: &str) {
        let job = app.doc_mut(path).and_then(|t| t.begin_save(false));
        land(app, d, job);
    }

    fn keep_mine(app: &mut App, d: &FakeDaemon, path: &str) {
        let job = app.doc_mut(path).and_then(|t| t.keep_mine(false));
        land(app, d, job);
    }

    fn missing(app: &App, path: &str) -> bool {
        app.doc(path)
            .is_some_and(crate::source::SourceBuffer::is_missing)
    }

    #[test]
    fn quit_with_a_disconnected_dirty_note_shows_the_dialog_ok_exits_cancel_returns() {
        let (_tmp, d, mut app) = setup(&["d2q/a.md"]);
        type_x(&mut app);
        app.status.connected = false;
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert!(!app.exiting);
        assert_eq!(
            dialog_notes(&app),
            [("d2q/a.md".to_owned(), LossReason::Disconnected)]
        );
        let _ = app.send(Message::Dialog(DialogMsg::Cancel));
        assert!(app.dialog.is_none() && !app.exiting);
        assert!(app.doc("d2q/a.md").is_some_and(|t| t.is_dirty()));
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert!(app.dialog.is_some());
        let _ = app.send(Message::Dialog(DialogMsg::Ok));
        assert!(app.exiting);
        assert_eq!(
            d.read_file("d2q/a.md").expect("file").0,
            b"body",
            "nothing written"
        );
    }

    #[test]
    fn quit_with_every_flush_succeeding_exits_without_a_dialog() {
        let (_tmp, d, mut app) = setup(&["d2s/a.md"]);
        type_x(&mut app);
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert!(app.dialog.is_none());
        assert!(app.exiting);
        assert_eq!(d.read_file("d2s/a.md").expect("file").0, b"xbody");
    }

    /// WP-1 hand-off: a quit awaiting a save that failed during an outage lands in the dialog.
    #[test]
    fn quit_awaiting_a_save_that_failed_shows_the_dialog() {
        let (_tmp, _d, mut app) = setup(&["d2f/a.md"]);
        type_x(&mut app);
        let job = app.doc_mut("d2f/a.md").and_then(|t| t.begin_save(false));
        let seq = job.expect("job").seq;
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert_eq!(app.quit_waits(), Some(1));
        let _ = app.send(Message::Source(SourceMsg::Saved {
            path: "d2f/a.md".into(),
            seq,
            outcome: SaveOutcome::Failed("daemon disconnected".into()),
            after: None,
        }));
        assert!(!app.exiting);
        assert_eq!(dialog_notes(&app).len(), 1);
        assert_eq!(app.dialog.as_ref().map(|d| &d.on_ok), Some(&OnOk::Quit));
    }

    #[test]
    fn quit_with_only_a_validation_blocked_settings_edit_shows_the_dialog() {
        let (_tmp, _d, mut app) = setup(&[]);
        let mut f = crate::settings::ConfigFile::new("_custom/crucible-vault/x.toml", None);
        f.dirty = true; // `doc: None` → `wants_save()` is false: the flush can never write it.
        app.settings.files.insert(f.path.clone(), f);
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert!(!app.exiting);
        let u = app
            .dialog
            .as_ref()
            .and_then(|d| d.unsaved_edits())
            .expect("dialog");
        assert_eq!(u.settings, ["_custom/crucible-vault/x.toml"]);
        let _ = app.send(Message::Dialog(DialogMsg::Ok));
        assert!(app.exiting);
    }

    #[test]
    fn tab_close_of_a_conflicted_duplicate_closes_only_that_tab_on_ok() {
        let (_tmp, d, mut app) = setup(&["d2c/a.md"]);
        let _ = app.send(Message::Panes(PanesMsg::SplitRight));
        app.prefs.quick_switch_open = QuickSwitchOpen::Duplicate;
        let _ = app.send(Message::Switcher(SwitcherMsg::Pick("d2c/a.md".into())));
        let count = |app: &App| {
            app.panes
                .iter()
                .flat_map(|(_, ps)| ps.tabs.iter())
                .filter(|t| t.path == "d2c/a.md")
                .count()
        };
        assert_eq!(count(&app), 2);
        type_x(&mut app);
        let (_, h) = d.read_file("d2c/a.md").expect("file");
        d.write_note("d2c/a.md", "theirs", &h).expect("external");
        save(&mut app, &d, "d2c/a.md");
        assert!(app.doc("d2c/a.md").is_some_and(|t| t.conflict().is_some()));
        let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
        assert_eq!(
            dialog_notes(&app),
            [("d2c/a.md".to_owned(), LossReason::Conflict)]
        );
        let _ = app.send(Message::Dialog(DialogMsg::Cancel));
        assert_eq!(count(&app), 2, "Cancel keeps it");
        let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
        let _ = app.send(Message::Dialog(DialogMsg::Ok));
        assert_eq!(count(&app), 1, "only that tab closed");
        assert!(
            app.doc("d2c/a.md").is_some_and(|t| t.is_dirty()),
            "the other tab keeps the shared document"
        );
        assert_eq!(d.read_file("d2c/a.md").expect("file").0, b"theirs");
    }

    /// Audit E2 (a): a delete notification under a dirty buffer → missing-file state; Keep
    /// mine recreates the file at its exact path.
    #[test]
    fn deleted_under_edits_enters_missing_state_and_keep_mine_recreates() {
        let (_tmp, d, mut app) = setup(&["d2m/a.md"]);
        type_x(&mut app);
        d.delete("d2m/a.md").expect("delete");
        deleted(&mut app, "d2m/a.md");
        assert!(missing(&app, "d2m/a.md"));
        keep_mine(&mut app, &d, "d2m/a.md");
        assert_eq!(d.read_file("d2m/a.md").expect("file").0, b"xbody");
        let t = app.doc("d2m/a.md").expect("doc");
        assert!(!t.is_dirty() && t.conflict().is_none());
    }

    /// Audit E2 (b): a save CAS conflict with an empty current hash → missing; Take theirs
    /// discards the edits and closes the note's tabs (no re-read of a missing file).
    #[test]
    fn save_conflict_against_no_file_is_missing_and_take_theirs_closes_tabs() {
        let (_tmp, d, mut app) = setup(&["d2t/a.md"]);
        type_x(&mut app);
        std::fs::remove_file(d.vault_root().join("d2t/a.md")).expect("rm");
        save(&mut app, &d, "d2t/a.md");
        assert!(missing(&app, "d2t/a.md"));
        let _ = app.send(Message::Source(SourceMsg::TakeTheirs("d2t/a.md".into())));
        assert!(app.open_paths().is_empty());
        assert!(app.doc("d2t/a.md").is_none());
        assert!(d.read_file("d2t/a.md").is_err(), "nothing written");
    }

    /// WP-1 hand-off: a reconnect resync whose read is NotFound enters the missing state.
    #[test]
    fn reconnect_resync_of_a_vanished_file_enters_missing_state() {
        let (_tmp, d, app) = setup(&["d2r/a.md"]);
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let mut app = app.with_connector(Arc::new(move || Ok(Arc::clone(&dc))));
        type_x(&mut app);
        std::fs::remove_file(d.vault_root().join("d2r/a.md")).expect("rm");
        let _ = app.send(Message::Status(StatusMsg::Disconnected));
        let _ = app.send(Message::Status(StatusMsg::ReconnectDue));
        assert!(missing(&app, "d2r/a.md"));
    }

    /// Audit B1: Keep mine when another file took the path meanwhile → the normal conflict,
    /// and no alternate (suffixed) file is created.
    #[test]
    fn keep_mine_when_the_path_was_taken_conflicts_without_an_alternate_file() {
        let (_tmp, d, mut app) = setup(&["d2b/a.md"]);
        type_x(&mut app);
        d.delete("d2b/a.md").expect("delete");
        deleted(&mut app, "d2b/a.md");
        assert!(missing(&app, "d2b/a.md"));
        std::fs::write(d.vault_root().join("d2b/a.md"), "other").expect("write");
        keep_mine(&mut app, &d, "d2b/a.md");
        let t = app.doc("d2b/a.md").expect("doc");
        assert!(
            t.conflict().is_some() && !t.is_missing(),
            "a normal conflict"
        );
        assert_eq!(d.read_file("d2b/a.md").expect("file").0, b"other");
        let names: Vec<_> = std::fs::read_dir(d.vault_root().join("d2b"))
            .expect("dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, ["a.md"], "no alternate file");
    }

    /// Audit E3: the open dialog takes input priority; Escape = Cancel; a repeated close
    /// request is ignored.
    #[test]
    fn dialog_is_modal_and_escape_cancels() {
        let (_tmp, _d, mut app) = setup(&["d2e/a.md", "d2e/b.md"]);
        type_x(&mut app);
        app.status.connected = false;
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        let before = app.dialog.clone();
        assert!(before.is_some());
        let text = app.active_doc().map(crate::source::SourceBuffer::text);
        // Global chords, editor input, focus-loss flush, tab close, repeated close request.
        let ctrl_w = crate::palette::chord::parse("Ctrl+W").expect("chord");
        let _ = app.send(Message::Palette(PaletteMsg::Key(ctrl_w)));
        type_x(&mut app);
        let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
        let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert_eq!(app.open_paths().len(), 2, "no tab closed");
        assert_eq!(
            app.active_doc().map(crate::source::SourceBuffer::text),
            text
        );
        assert_eq!(app.dialog, before, "repeated close ignored");
        assert!(app.quit.is_none() && !app.exiting);
        // Escape: the dialog key listener maps it to Cancel (the switcher's Close too).
        let esc = iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers: iced::keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        };
        assert!(matches!(
            crate::dialog::key_event(&esc),
            Some(Message::Dialog(DialogMsg::Cancel))
        ));
        let _ = app.send(Message::Switcher(SwitcherMsg::Close));
        assert!(app.dialog.is_none() && !app.exiting);
        // Enter on the unsaved-edits dialog is Cancel (the safe default), never OK.
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        let _ = app.send(Message::Dialog(DialogMsg::Default));
        assert!(app.dialog.is_none() && !app.exiting);
    }
}

/// plan editor-ui-round-3 WP-2: explorer/switcher follow index membership (audit B1) and Reading
/// re-lowers on a daemon-origin rewrite (the lint path).
mod ui_round3_wp2 {
    use iced::widget::text_editor::{Action, Edit};

    use super::wp4::{app_over, fake_arc};
    use super::*;
    use crate::explorer::{FileOp, FileOpDone};
    use crate::message::{ExplorerMsg, ReadingMsg, SourceMsg, SwitcherMsg, TabsMsg};
    use crate::reading::ViewMode;

    fn generation(app: &mut App, d: &FakeDaemon) {
        let n = d.generation().expect("gen");
        let _ = app.send(Message::Status(StatusMsg::Notification(
            Notification::IndexGeneration { n },
        )));
    }

    fn candidates(app: &App) -> Vec<String> {
        app.switcher
            .results
            .iter()
            .map(|r| r.path.clone())
            .collect()
    }

    fn file_changed(app: &mut App, path: &str, kind: ChangeKind, origin: Origin) {
        let _ = app.send(Message::Status(StatusMsg::Notification(
            Notification::FileChanged {
                path: path.to_owned(),
                kind,
                hash: None,
                origin,
            },
        )));
    }

    #[test]
    fn ignore_rebuild_removes_and_restores_in_explorer_and_switcher() {
        let (_tmp, d) = fake_arc();
        let vault = d.vault_root().to_path_buf();
        d.create_note("r3ign/x.md", "x").expect("create");
        let mut app = app_over(&d, None);
        generation(&mut app, &d);
        let _ = app.send(Message::Switcher(SwitcherMsg::Open));
        assert!(app.explorer.contains("r3ign/x.md"));
        assert!(candidates(&app).contains(&"r3ign/x.md".to_owned()));

        std::fs::write(vault.join(".gitignore"), "r3ign/\n").expect("ignore");
        d.create_note("r3other/a.md", "a").expect("rebuild");
        generation(&mut app, &d);
        assert!(!app.explorer.stale);
        assert!(!app.explorer.contains("r3ign/x.md"), "ignored: dropped");
        assert!(!candidates(&app).contains(&"r3ign/x.md".to_owned()));

        std::fs::remove_file(vault.join(".gitignore")).expect("unignore");
        d.create_note("r3other/b.md", "b").expect("rebuild");
        generation(&mut app, &d);
        assert!(app.explorer.contains("r3ign/x.md"), "restored");
        assert!(candidates(&app).contains(&"r3ign/x.md".to_owned()));
    }

    #[test]
    fn op_or_delta_insert_of_non_indexed_path_is_dropped() {
        let (_tmp, d) = fake_arc();
        let vault = d.vault_root().to_path_buf();
        std::fs::write(vault.join(".gitignore"), "r3ign/\n").expect("ignore");
        d.create_note("r3ok/a.md", "a").expect("rebuild");
        let mut app = app_over(&d, None);
        std::fs::create_dir_all(vault.join("r3ign")).expect("mkdir");
        std::fs::write(vault.join("r3ign/new.md"), "n").expect("write");
        let _ = app.send(Message::Explorer(ExplorerMsg::Done(FileOpDone {
            op: FileOp::Create {
                path: "r3ign/new.md".into(),
            },
            result: Ok("r3ign/new.md".into()),
        })));
        assert!(!app.explorer.contains("r3ign/new.md"), "op insert dropped");
        file_changed(
            &mut app,
            "r3ign/new.md",
            ChangeKind::Created,
            Origin::External,
        );
        assert!(
            !app.explorer.contains("r3ign/new.md"),
            "delta insert dropped"
        );
        // An indexed path still inserts.
        d.create_note("r3ok/b.md", "b").expect("create");
        let _ = app.send(Message::Explorer(ExplorerMsg::Done(FileOpDone {
            op: FileOp::Create {
                path: "r3ok/b.md".into(),
            },
            result: Ok("r3ok/b.md".into()),
        })));
        assert!(app.explorer.contains("r3ok/b.md"));
    }

    /// The Lint repro: a daemon-origin rewrite of an open note's frontmatter reaches the editor
    /// as `file_changed` without a new index generation (incremental update). Clean buffer: the
    /// reload must re-lower Reading. Dirty buffer: conflict bar, nothing overwritten.
    #[test]
    fn reading_relowers_after_daemon_rewrite_clean_and_dirty() {
        let (_tmp, d) = fake_arc();
        let vault = d.vault_root().to_path_buf();
        for p in ["r3lint/a.md", "r3lint/b.md"] {
            d.create_note(p, "---\nstatus: old\n---\nbody\n")
                .expect("create");
        }
        let mut app = app_over(&d, None);
        generation(&mut app, &d);
        for p in ["r3lint/a.md", "r3lint/b.md"] {
            let _ = app.send(Message::Tabs(TabsMsg::Open(p.to_owned())));
            let _ = app.send(Message::Reading(ReadingMsg::SetMode {
                path: p.to_owned(),
                mode: ViewMode::Reading,
            }));
        }
        let shown = |app: &App, p: &str| format!("{:?}", app.reading.doc(p).expect("doc").model);
        assert!(shown(&app, "r3lint/a.md").contains("old"));

        // b is dirty (an in-memory edit; b is the active tab).
        let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
            Edit::Insert('Z'),
        ))));
        assert!(app.doc("r3lint/b.md").expect("b").is_dirty());
        let gen_before = d.generation().expect("gen");

        // The lint write: daemon-origin, incremental (no generation notification).
        for p in ["r3lint/a.md", "r3lint/b.md"] {
            std::fs::write(vault.join(p), "---\nstatus: linted\n---\nbody\n").expect("write");
            file_changed(&mut app, p, ChangeKind::Modified, Origin::Daemon);
        }
        assert_eq!(d.generation().expect("gen"), gen_before);
        let _ = app.send(Message::Reading(ReadingMsg::Tick));
        assert!(
            shown(&app, "r3lint/a.md").contains("linted"),
            "clean buffer: Reading shows the new frontmatter"
        );
        let b = app.doc("r3lint/b.md").expect("b");
        assert!(
            b.is_dirty() && b.conflict().is_some(),
            "dirty: conflict bar"
        );
        assert!(
            !b.text().contains("linted"),
            "dirty buffer never overwritten"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// plan editor-ui-round-3 WP-4: context menus
// ---------------------------------------------------------------------------------------------

mod ui_round3_wp4 {
    use super::wp4::{app_over, fake_arc};
    use super::*;
    use crate::context_menu::{Item, MenuMsg, Target};
    use crate::message::{ExplorerMsg, SourceMsg, SwitcherMsg};

    const AT: iced::Point = iced::Point::new(30.0, 40.0);

    fn menu(m: MenuMsg) -> Message {
        Message::Menu(m)
    }

    fn open(app: &mut App, p: &str) {
        let _ = app.send(Message::Explorer(ExplorerMsg::Open(p.into())));
    }

    fn tab_target(app: &App, path: &str) -> Target {
        let ps = app.panes.get(app.focus).expect("pane");
        let tab = ps.tabs.iter().find(|t| t.path == path).expect("tab");
        Target::Tab {
            pane: app.focus,
            id: tab.id,
            path: path.to_owned(),
        }
    }

    fn tab_paths(app: &App) -> Vec<String> {
        app.panes
            .get(app.focus)
            .map(|ps| ps.tabs.iter().map(|t| t.path.clone()).collect())
            .unwrap_or_default()
    }

    fn row(path: &str, folder: bool) -> Target {
        Target::Row {
            path: path.into(),
            folder,
        }
    }

    #[test]
    fn menu_opens_at_the_cursor_and_closes_on_escape_outside_click_and_choice() {
        let (_tmp, d) = fake_arc();
        d.create_note("m4/a.md", "x").expect("create");
        let mut app = app_over(&d, None);
        // The right-press wrapper's message carries the press point (the anchor).
        let Message::Menu(MenuMsg::Open(t, at)) =
            crate::context_menu::open_msg(row("m4/a.md", false))(AT)
        else {
            panic!("open message");
        };
        assert_eq!((t, at), (row("m4/a.md", false), AT));
        let _ = app.send(menu(MenuMsg::Open(row("m4/a.md", false), AT)));
        let m = app.context_menu.as_ref().expect("open");
        assert_eq!(m.at, iced::Point::new(30.0, 40.0));
        // Escape (the switcher's Close) is the menu's Close.
        let _ = app.send(Message::Switcher(SwitcherMsg::Close));
        assert!(app.context_menu.is_none());
        // Outside click.
        let _ = app.send(menu(MenuMsg::Open(row("m4/a.md", false), AT)));
        let _ = app.send(menu(MenuMsg::Close));
        assert!(app.context_menu.is_none());
        // Choosing an item closes it and runs it.
        let _ = app.send(menu(MenuMsg::Open(row("m4/a.md", false), AT)));
        let _ = app.send(menu(MenuMsg::Choose(Item::Open)));
        assert!(app.context_menu.is_none());
        assert_eq!(app.active_tab().map(|t| t.path.as_str()), Some("m4/a.md"));
        // Pane input is dropped while it is open.
        let _ = app.send(menu(MenuMsg::Open(row("m4/a.md", false), AT)));
        let _ = app.send(Message::Source(SourceMsg::Action(
            iced::widget::text_editor::Action::Edit(iced::widget::text_editor::Edit::Insert('Z')),
        )));
        assert!(!app.active_doc().expect("doc").is_dirty());
        assert!(app.context_menu.is_some());
    }

    #[test]
    fn task_status_menu_sets_the_marker_as_one_undo_step() {
        let (_tmp, d) = fake_arc();
        d.create_note("m4t/t.md", "- [ ] a\n- [x] b\n")
            .expect("create");
        let mut app = app_over(&d, None);
        open(&mut app, "m4t/t.md");
        let _ = app.send(menu(MenuMsg::Open(
            Target::Task {
                path: "m4t/t.md".into(),
                line: 1,
            },
            AT,
        )));
        let _ = app.send(menu(MenuMsg::Choose(Item::TaskStatus('!'))));
        let doc = app.active_doc().expect("doc");
        assert_eq!(doc.text(), "- [ ] a\n- [!] b\n");
        assert!(doc.is_dirty(), "saved by the normal autosave path");
        assert_eq!(
            d.read_file("m4t/t.md").expect("file").0,
            b"- [ ] a\n- [x] b\n"
        );
        let _ = app.send(Message::Source(SourceMsg::Undo));
        assert_eq!(app.active_doc().expect("doc").text(), "- [ ] a\n- [x] b\n");
    }

    #[test]
    fn tab_menu_close_others_copy_path_reveal_and_delete() {
        let (_tmp, d) = fake_arc();
        for p in ["m4tab/a.md", "m4tab/b.md", "m4tab/c.md"] {
            d.create_note(p, "x").expect("create");
        }
        let mut app = app_over(&d, None);
        for p in ["m4tab/a.md", "m4tab/b.md", "m4tab/c.md"] {
            open(&mut app, p);
        }
        assert_eq!(tab_paths(&app).len(), 3);
        let t = tab_target(&app, "m4tab/b.md");
        let _ = app.send(menu(MenuMsg::Open(t.clone(), AT)));
        let _ = app.send(menu(MenuMsg::Choose(Item::CloseOthers)));
        assert_eq!(tab_paths(&app), ["m4tab/b.md"]);

        let _ = app.send(menu(MenuMsg::Open(t.clone(), AT)));
        let _ = app.send(menu(MenuMsg::Choose(Item::CopyPath)));
        assert_eq!(app.copied(), ["m4tab/b.md"]);

        app.ui.side_collapsed = true;
        let _ = app.send(menu(MenuMsg::Open(t.clone(), AT)));
        let _ = app.send(menu(MenuMsg::Choose(Item::Reveal)));
        assert!(!app.ui.side_collapsed);
        assert!(app.ui.expanded.contains("m4tab"));
        assert_eq!(app.explorer.selected.as_deref(), Some("m4tab/b.md"));

        let _ = app.send(menu(MenuMsg::Open(t, AT)));
        let _ = app.send(menu(MenuMsg::Choose(Item::Delete)));
        assert!(app.dialog.is_some(), "Delete routes to the delete dialog");
        assert!(app.context_menu.is_none());
        // The dialog outranks the menu: a right-press is dropped.
        let t = tab_target(&app, "m4tab/b.md");
        let _ = app.send(menu(MenuMsg::Open(t, AT)));
        assert!(app.context_menu.is_none());
    }

    #[test]
    fn explorer_row_menu_open_in_new_pane_rename_and_folder_new_note() {
        let (_tmp, d) = fake_arc();
        for p in ["m4row/a.md", "m4row/b.md"] {
            d.create_note(p, "x").expect("create");
        }
        let mut app = app_over(&d, None);
        open(&mut app, "m4row/a.md");
        let first = app.focus;
        let _ = app.send(menu(MenuMsg::Open(row("m4row/a.md", false), AT)));
        let _ = app.send(menu(MenuMsg::Choose(Item::OpenInNewPane)));
        assert_eq!(app.panes.len(), 2);
        assert_ne!(app.focus, first);
        assert_eq!(
            app.active_tab().map(|t| t.path.as_str()),
            Some("m4row/a.md")
        );

        let _ = app.send(menu(MenuMsg::Open(row("m4row/b.md", false), AT)));
        let _ = app.send(menu(MenuMsg::Choose(Item::Rename)));
        let e = app.explorer.edit.as_ref().expect("inline edit");
        assert_eq!(e.kind, crate::explorer::EditKind::Rename);
        assert_eq!(e.target, "m4row/b.md");
        assert_eq!(e.input, "b.md");

        let _ = app.send(menu(MenuMsg::Open(row("m4row", true), AT)));
        let _ = app.send(menu(MenuMsg::Choose(Item::NewNote)));
        let e = app.explorer.edit.as_ref().expect("inline edit");
        assert_eq!(e.kind, crate::explorer::EditKind::New);
        assert_eq!(e.target, "m4row");
    }
}

// Bug fix editor-find-return-leak: keys pressed in the find bar must never edit the note. Drives
// the real widget tree (headless tiny-skia renderer) so widget focus is exercised end to end.
mod find_return_leak {
    use iced::advanced::widget::{Operation, Tree, operation};
    use iced::advanced::{Shell, clipboard, layout, mouse, renderer::Headless};
    use iced::keyboard::{self, Key, Location, Modifiers, key};
    use iced::widget::text_editor::{Action, Motion};
    use iced::{Event, Font, Pixels, Rectangle, Size};

    use super::*;
    use crate::message::{ExplorerMsg, FindMsg, PaletteMsg, SourceMsg};
    use crate::reading::ViewMode;

    const PATH: &str = "frl/a.md";
    const BODY: &str = "- one\n- two foo";

    pub(super) fn renderer() -> iced::Renderer {
        let fut =
            <iced::Renderer as Headless>::new(Font::default(), Pixels(16.0), Some("tiny-skia"));
        let mut fut = std::pin::pin!(fut);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        loop {
            if let std::task::Poll::Ready(r) = fut.as_mut().poll(&mut cx) {
                return r.expect("headless renderer");
            }
        }
    }

    /// A window-less UI: the app's view tree kept across rebuilds like iced's `UserInterface`.
    struct Ui {
        renderer: iced::Renderer,
        tree: Option<Tree>,
        /// Window height (800 unless a test needs a tall page on screen).
        height: f32,
    }

    impl Ui {
        fn with<R>(
            &mut self,
            app: &App,
            f: impl FnOnce(
                &mut iced::Element<'_, Message>,
                &mut Tree,
                layout::Layout<'_>,
                &iced::Renderer,
            ) -> R,
        ) -> R {
            let mut el = app.view();
            let tree = match &mut self.tree {
                Some(t) => {
                    t.diff(&el);
                    t
                }
                None => self.tree.insert(Tree::new(&el)),
            };
            let limits = layout::Limits::new(Size::ZERO, Size::new(1200.0, self.height));
            let node = el.as_widget_mut().layout(tree, &self.renderer, &limits);
            f(&mut el, tree, layout::Layout::new(&node), &self.renderer)
        }

        fn focus(&mut self, app: &App, id: iced::widget::Id) {
            self.with(app, |el, tree, l, r| {
                let mut op = operation::focusable::focus::<()>(id);
                el.as_widget_mut()
                    .operate(tree, l, r, &mut op as &mut dyn Operation<()>);
            });
        }

        /// Sends `event` through the tree, then feeds the published messages to the app.
        fn send(&mut self, app: &mut App, event: &Event) {
            self.send_at(app, event, mouse::Cursor::Unavailable);
        }

        fn send_at(&mut self, app: &mut App, event: &Event, cursor: mouse::Cursor) {
            let height = self.height;
            let msgs = self.with(app, |el, tree, l, r| {
                let mut msgs = Vec::new();
                let mut shell = Shell::new(&mut msgs);
                let viewport = Rectangle::with_size(Size::new(1200.0, height));
                el.as_widget_mut().update(
                    tree,
                    event,
                    l,
                    cursor,
                    r,
                    &mut clipboard::Null,
                    &mut shell,
                    &viewport,
                );
                msgs
            });
            for m in msgs {
                // The runtime side of `FindMsg::Focus`'s task (`operation::focus(input)`), which
                // the window-less harness cannot execute from the returned `Task`.
                let refocus = match &m {
                    Message::Find(FindMsg::Focus(p)) => Some(crate::find::input_id(*p)),
                    _ => None,
                };
                let _ = app.send(m);
                if let Some(id) = refocus {
                    self.focus(app, id);
                }
            }
        }
    }

    impl Ui {
        fn click(&mut self, app: &mut App, at: iced::Point) {
            let c = mouse::Cursor::Available(at);
            self.send_at(
                app,
                &Event::Mouse(mouse::Event::CursorMoved { position: at }),
                c,
            );
            self.send_at(
                app,
                &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                c,
            );
            self.send_at(
                app,
                &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                c,
            );
        }
    }

    fn press(k: Key, text: Option<&str>) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: k.clone(),
            modified_key: k,
            physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
            text: text.map(Into::into),
            repeat: false,
        })
    }

    fn text_of(app: &App) -> String {
        app.active_doc()
            .map(crate::source::SourceBuffer::text)
            .expect("doc")
    }

    /// A source-mode note with its caret on a list line, the editor focused (the user was
    /// typing), then Ctrl+F with a query typed into the bar.
    fn setup() -> (tempfile::TempDir, App, Ui) {
        let (tmp, d) = fake();
        let d = Arc::new(d);
        d.create_note(PATH, BODY).expect("create");
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let mut app = App::new(Some(dc), None);
        app.queue_rpcs();
        let _ = app.send(Message::Explorer(ExplorerMsg::Open(PATH.into())));
        let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
            path: PATH.into(),
            mode: ViewMode::Source,
        }));
        let _ = app.send(Message::Source(SourceMsg::Action(Action::Move(
            Motion::DocumentEnd,
        ))));
        let mut ui = Ui {
            renderer: renderer(),
            tree: None,
            height: 800.0,
        };
        ui.focus(&app, crate::source::editor_id(PATH));
        let _ = app.send(Message::Palette(PaletteMsg::Key(
            crate::palette::chord::parse("Ctrl+F").expect("chord"),
        )));
        assert!(app.panes.get(app.focus).is_some_and(|ps| ps.find.is_some()));
        // FindMsg::Open's task: focus the bar's input.
        ui.focus(&app, crate::find::input_id(app.focus));
        let _ = app.send(Message::Find(FindMsg::Query("foo".into())));
        (tmp, app, ui)
    }

    /// Note at (600, 400); find input at (400, 140); the bar's ChevronDown (Next) at (1131, 141).
    const IN_NOTE: iced::Point = iced::Point::new(600.0, 400.0);
    const IN_INPUT: iced::Point = iced::Point::new(400.0, 140.0);
    const ON_BAR: iced::Point = iced::Point::new(1131.0, 141.0);

    #[test]
    fn find_input_click_takes_focus_from_the_note() {
        let (_tmp, mut app, mut ui) = setup();
        // Click into the note, then back into the find input: the editor must lose focus.
        ui.click(&mut app, IN_NOTE);
        ui.click(&mut app, IN_INPUT);
        for _ in 0..3 {
            ui.send(&mut app, &press(Key::Named(key::Named::Enter), Some("\r")));
        }
        ui.send(&mut app, &press(Key::Named(key::Named::Tab), Some("\t")));
        ui.send(&mut app, &press(Key::Named(key::Named::Backspace), None));
        ui.send(&mut app, &press(Key::Character("x".into()), Some("x")));
        assert_eq!(text_of(&app), BODY, "find-bar keys leaked into the note");
        let q = app.panes.get(app.focus).and_then(|ps| ps.find.clone());
        assert_eq!(
            q.map(|b| b.query).as_deref(),
            Some("fox"),
            "keys went to the bar"
        );
    }

    #[test]
    fn find_bar_click_then_return_never_edits_the_note() {
        let (_tmp, mut app, mut ui) = setup();
        // Click into the note, then on the bar beside the input (Next chevron), then Return x12.
        ui.click(&mut app, IN_NOTE);
        ui.click(&mut app, ON_BAR);
        for _ in 0..12 {
            ui.send(&mut app, &press(Key::Named(key::Named::Enter), Some("\r")));
        }
        assert_eq!(text_of(&app), BODY, "Return leaked into the note");
    }

    /// Collects text and text-input bounds (headless locator for widgets without ids).
    #[derive(Default)]
    struct Spots {
        texts: Vec<(Rectangle, String)>,
        inputs: Vec<Rectangle>,
    }

    impl Operation<()> for Spots {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<()>)) {
            operate(self);
        }
        fn text(&mut self, _id: Option<&iced::widget::Id>, b: Rectangle, t: &str) {
            self.texts.push((b, t.to_owned()));
        }
        fn text_input(
            &mut self,
            _id: Option<&iced::widget::Id>,
            b: Rectangle,
            _s: &mut dyn operation::TextInput,
        ) {
            self.inputs.push(b);
        }
    }

    fn spots(ui: &mut Ui, app: &App) -> Spots {
        ui.with(app, |el, tree, l, r| {
            let mut op = Spots::default();
            el.as_widget_mut()
                .operate(tree, l, r, &mut op as &mut dyn Operation<()>);
            op
        })
    }

    /// Plan round-6-feedback-todos WP-8: a mouse click on an Ignore Shorts typeahead candidate
    /// adds the chip, and the list stays open (chip adders keep it open for the next pick).
    #[test]
    fn ignore_shorts_candidate_click_adds_the_chip() {
        use crate::settings::{Section, SettingsMsg};
        const PIPES: &str = "_custom/crucible-vault/pipelines.toml";
        const SOURCES: &str = "_custom/crucible-vault/sources.toml";
        let (_tmp, d) = fake();
        let d = Arc::new(d);
        d.create_note(SOURCES, "[[youtube]]\nchannel = \"Chan\"\nid = \"UC123\"\n")
            .expect("sources");
        d.create_note(PIPES, "[ignore_shorts]\nchannels = []\n")
            .expect("pipes");
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let mut app = App::new(Some(dc), None);
        app.queue_rpcs();
        let _ = app.send(Message::Explorer(ExplorerMsg::Open(
            crate::settings::SETTINGS_TAB.into(),
        )));
        let _ = app.send(Message::Settings(SettingsMsg::Section(Section::Pipelines)));
        let mut ui = Ui {
            renderer: renderer(),
            tree: None,
            height: 3000.0,
        };
        // The chip adder: the 160 px input level with the field's help text.
        let s = spots(&mut ui, &app);
        let help = s
            .texts
            .iter()
            .find(|(_, t)| t == "Channel ids or names")
            .map(|(b, _)| b.center_y())
            .expect("channels help");
        let input = s
            .inputs
            .iter()
            .filter(|b| (b.width - 160.0).abs() < 0.5)
            .min_by(|a, b| {
                (a.center_y() - help)
                    .abs()
                    .total_cmp(&(b.center_y() - help).abs())
            })
            .copied()
            .expect("chip adder input");
        ui.click(&mut app, input.center());
        ui.send(&mut app, &press(Key::Character("c".into()), Some("c")));
        assert!(
            app.settings
                .form
                .typeahead
                .as_ref()
                .is_some_and(|a| a.ta.open),
            "typing opened the list"
        );
        // A second keystroke still reaches the input (focus survived the list appearing).
        ui.send(&mut app, &press(Key::Character("h".into()), Some("h")));
        let draft = app
            .settings
            .form
            .draft(
                PIPES,
                &crate::settings::form::draft_key(
                    "chip",
                    &cv_schema::Path::parse("ignore_shorts.channels").expect("p"),
                ),
            )
            .map(str::to_owned);
        assert_eq!(draft.as_deref(), Some("ch"), "input kept focus");
        let s = spots(&mut ui, &app);
        let cand = s
            .texts
            .iter()
            .find(|(_, t)| t == "Chan (UC123)")
            .map(|(b, _)| *b)
            .expect("candidate shown");
        ui.click(&mut app, cand.center());
        let doc = app
            .settings
            .file(PIPES)
            .and_then(|f| f.doc.as_ref())
            .map(ToString::to_string)
            .unwrap_or_default();
        assert!(doc.contains("\"UC123\""), "chip added: {doc}");
        assert!(
            app.settings
                .form
                .typeahead
                .as_ref()
                .is_some_and(|a| a.ta.open),
            "chip adders keep the list open"
        );
    }

    /// Plan round-7-feedback-decisions WP-5: Ignore Shorts is a vertical add/drop list. A picked
    /// channel leaves the suggestions; after the save lands and its FileChanged echo arrives,
    /// typing in the adder again and clicking a candidate still adds it; a remove round-trips to
    /// the expected TOML string array.
    #[test]
    fn ignore_shorts_list_add_again_across_save_echo() {
        use crate::settings::{FormMsg, Section, SettingsMsg};
        const PIPES: &str = "_custom/crucible-vault/pipelines.toml";
        const SOURCES: &str = "_custom/crucible-vault/sources.toml";
        let (_tmp, d) = fake();
        let d = Arc::new(d);
        d.create_note(
            SOURCES,
            "[[youtube]]\nchannel = \"Chan\"\nid = \"UC123\"\n\n[[youtube]]\nchannel = \"Chao\"\nid = \"UC456\"\n",
        )
        .expect("sources");
        d.create_note(PIPES, "[ignore_shorts]\nchannels = []\n")
            .expect("pipes");
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let mut app = App::new(Some(dc), None);
        app.queue_rpcs();
        let _ = app.send(Message::Explorer(ExplorerMsg::Open(
            crate::settings::SETTINGS_TAB.into(),
        )));
        let _ = app.send(Message::Settings(SettingsMsg::Section(Section::Pipelines)));
        let mut ui = Ui {
            renderer: renderer(),
            tree: None,
            height: 3000.0,
        };
        let chans = cv_schema::Path::parse("ignore_shorts.channels").expect("p");
        let adder = |ui: &mut Ui, app: &App| {
            let s = spots(ui, app);
            let help = s
                .texts
                .iter()
                .find(|(_, t)| t == "Channel ids or names")
                .map(|(b, _)| b.center_y())
                .expect("channels help");
            s.inputs
                .iter()
                .filter(|b| (b.width - 160.0).abs() < 0.5)
                .min_by(|a, b| {
                    (a.center_y() - help)
                        .abs()
                        .total_cmp(&(b.center_y() - help).abs())
                })
                .copied()
                .expect("list adder input")
        };
        let pick = |ui: &mut Ui, app: &mut App, label: &str| {
            let input = adder(ui, app);
            ui.click(app, input.center());
            ui.send(app, &press(Key::Character("c".into()), Some("c")));
            let s = spots(ui, app);
            let cand = s
                .texts
                .iter()
                .find(|(_, t)| t == label)
                .map(|(b, _)| *b)
                .unwrap_or_else(|| panic!("candidate {label} shown"));
            ui.click(app, cand.center());
        };
        let save = |app: &mut App| {
            let _ = app.send(Message::Settings(SettingsMsg::Save(PIPES.into())));
            let (bytes, h) = d.read_file(PIPES).expect("saved");
            let _ = app.send(Message::Status(StatusMsg::Notification(
                Notification::FileChanged {
                    path: PIPES.to_owned(),
                    kind: cv_protocol::ChangeKind::Modified,
                    hash: Some(h.as_str().to_owned()),
                    origin: cv_protocol::Origin::Daemon,
                },
            )));
            String::from_utf8(bytes).expect("utf8")
        };
        pick(&mut ui, &mut app, "Chan (UC123)");
        // The picked channel leaves the suggestions.
        let left: Vec<String> = app
            .settings
            .ta_visible(&[])
            .into_iter()
            .map(|c| c.value)
            .collect();
        assert!(!left.iter().any(|v| v == "UC123"), "{left:?}");
        assert!(left.iter().any(|v| v == "UC456"), "{left:?}");
        let text = save(&mut app);
        assert!(text.contains("channels = [\"UC123\"]"), "{text}");
        // Across the save and its echo: pick again.
        pick(&mut ui, &mut app, "Chao (UC456)");
        let text = save(&mut app);
        assert!(text.contains("channels = [\"UC123\", \"UC456\"]"), "{text}");
        // The list shows one row per channel (vertical: distinct rows).
        let s = spots(&mut ui, &app);
        let ys: Vec<f32> = ["Chan (UC123)", "Chao (UC456)"]
            .iter()
            .map(|l| {
                s.texts
                    .iter()
                    .find(|(_, t)| t == l)
                    .map(|(b, _)| b.center_y())
                    .expect("row")
            })
            .collect();
        assert!(ys[1] > ys[0] + 10.0, "vertical rows: {ys:?}");
        let _ = app.send(Message::Settings(SettingsMsg::Form(
            PIPES.into(),
            FormMsg::ChipRemove {
                path: chans,
                index: 0,
            },
        )));
        let text = save(&mut app);
        assert!(text.contains("channels = [\"UC456\"]"), "{text}");
    }

    /// Plan round-7 WP-3: the dedicated Log icon-button opens `/:log` and resets unread; a
    /// click on the notice text opens it too.
    #[test]
    fn log_button_and_notice_open_the_log() {
        let mut app = App::new(None, None);
        app.status
            .notify(crate::app::Severity::Warning, "first trouble");
        let unread = app.status.log.unread;
        assert!(unread >= 1);
        let mut ui = Ui {
            renderer: renderer(),
            tree: None,
            height: 800.0,
        };
        let s = spots(&mut ui, &app);
        let label = format!("☰ Log {unread}");
        let at = s
            .texts
            .iter()
            .find(|(_, t)| *t == label)
            .map(|(b, _)| b.center())
            .expect("log button");
        ui.click(&mut app, at);
        assert!(app.active_tab().is_some_and(|t| t.is_log()));
        assert_eq!(app.status.log.unread, 0, "opening resets unread");
        // Back to no Log tab focused; the notice itself is clickable.
        let _ = app.send(Message::Tabs(crate::message::TabsMsg::CloseActive));
        assert!(!app.active_tab().is_some_and(|t| t.is_log()));
        app.status
            .notify(crate::app::Severity::Error, "second trouble");
        let s = spots(&mut ui, &app);
        let at = s
            .texts
            .iter()
            .find(|(_, t)| t.contains("second trouble"))
            .map(|(b, _)| b.center())
            .expect("notice text");
        ui.click(&mut app, at);
        assert!(app.active_tab().is_some_and(|t| t.is_log()));
        assert_eq!(app.status.log.unread, 0);
    }
}

/// Plan usability-sprint-captures-links WP-5: `title` → filename sync.
mod title_sync_wp5 {
    use std::time::Instant;

    use super::wp4::{app_over, fake_arc};
    use super::*;
    use crate::message::ExplorerMsg;

    const OLD: &str = "---\ntitle: Old\n---\nx\n";

    /// Opens `path` and rewrites the `title` value `Old` (line 1, cols 7..10) to `new`.
    fn open_and_retitle(d: &Arc<FakeDaemon>, path: &str, new: &str) -> App {
        d.create_note(path, OLD).expect("create");
        let mut app = app_over(d, None);
        let _ = app.send(Message::Explorer(ExplorerMsg::Open(path.into())));
        let doc = app.doc_mut(path).expect("open");
        doc.select_range((1, 7), (1, 10));
        assert!(doc.paste(new.into(), false, Instant::now()));
        app
    }

    fn save(app: &mut App, d: &FakeDaemon, path: &str) {
        let job = app
            .doc_mut(path)
            .and_then(|t| t.begin_save(false))
            .expect("save starts");
        let outcome = job.run(d);
        let _ = app.send(job.done(outcome));
    }

    fn gone(d: &FakeDaemon, path: &str) -> bool {
        matches!(d.read_file(path), Err(DaemonError::NotFound(_)))
    }

    #[test]
    fn title_edit_renames_a_note_named_after_its_title() {
        let (_tmp, d) = fake_arc();
        let mut app = open_and_retitle(&d, "ts/Old.md", "New? One");
        save(&mut app, &d, "ts/Old.md");
        assert!(gone(&d, "ts/Old.md"));
        let (bytes, _) = d.read_file("ts/New One.md").expect("renamed");
        assert_eq!(bytes, b"---\ntitle: New? One\n---\nx\n");
        assert_eq!(app.open_paths(), ["ts/New One.md"], "the tab follows");
        assert!(app.doc("ts/Old.md").is_none(), "doc key moved");
        assert!(app.doc("ts/New One.md").is_some_and(|t| !t.is_dirty()));
        assert!(app.explorer.contains("ts/New One.md"));
        // A further save with the same title does not rename again.
        let doc = app.doc_mut("ts/New One.md").expect("open");
        doc.select_range((3, 0), (3, 1));
        assert!(doc.paste("y".into(), false, Instant::now()));
        save(&mut app, &d, "ts/New One.md");
        assert_eq!(app.open_paths(), ["ts/New One.md"]);
    }

    /// Plan round-7-feedback-decisions WP-2: the title-sync rename of a (now clean) note keeps
    /// cursor, selection, undo depth and top_line, with no conflict.
    #[test]
    fn title_sync_rename_keeps_editor_state() {
        let (_tmp, d) = fake_arc();
        let mut app = open_and_retitle(&d, "r7ts/Old.md", "Fresh");
        let doc = app.doc_mut("r7ts/Old.md").expect("open");
        doc.select_range((3, 0), (3, 1));
        let before = (doc.cursor(), doc.history().undo_len(), doc.top_line());
        assert!(before.1 > 0);
        save(&mut app, &d, "r7ts/Old.md");
        assert_eq!(app.open_paths(), ["r7ts/Fresh.md"]);
        let doc = app.doc("r7ts/Fresh.md").expect("followed");
        assert_eq!(
            (doc.cursor(), doc.history().undo_len(), doc.top_line()),
            before
        );
        assert!(!doc.is_dirty() && doc.conflict().is_none());
    }

    #[test]
    fn title_edit_leaves_a_note_with_another_name_alone() {
        let (_tmp, d) = fake_arc();
        let mut app = open_and_retitle(&d, "ts2/Custom.md", "New");
        save(&mut app, &d, "ts2/Custom.md");
        assert_eq!(app.open_paths(), ["ts2/Custom.md"]);
        assert!(gone(&d, "ts2/New.md"));
    }

    #[test]
    fn title_edit_onto_an_existing_file_does_not_rename() {
        let (_tmp, d) = fake_arc();
        d.create_note("ts3/New.md", "other").expect("create");
        let mut app = open_and_retitle(&d, "ts3/Old.md", "New");
        save(&mut app, &d, "ts3/Old.md");
        assert_eq!(app.open_paths(), ["ts3/Old.md"]);
        assert_eq!(d.read_file("ts3/New.md").expect("kept").0, b"other");
        assert!(
            app.status
                .error_text()
                .is_some_and(|e| e.contains("already exists")),
            "{:?}",
            app.status.notice
        );
    }

    /// Audit E2: edits typed after the save landed but before the rename completes follow the
    /// tab to `to`, are kept (conflict bar), and nothing is ever written at `from`.
    #[test]
    fn edit_during_title_rename_is_kept_and_from_is_not_recreated() {
        let (_tmp, d) = fake_arc();
        let mut app = open_and_retitle(&d, "ts5/Old.md", "New");
        app.rpc_exec = crate::app::RpcExec::Pool; // hold the rename RPC; complete it by hand below.
        save(&mut app, &d, "ts5/Old.md");
        let doc = app.doc_mut("ts5/Old.md").expect("still at from");
        assert!(doc.is_renaming());
        let seq = doc.edit_seq();
        doc.select_range((3, 0), (3, 1));
        assert!(doc.paste("typed".into(), false, Instant::now()));
        assert!(
            doc.begin_save(false).is_none(),
            "saves held during the rename"
        );
        let typed = doc.edit_seq();
        d.rename("ts5/Old.md", "ts5/New.md").expect("rename");
        let _ = app.send(Message::TitleRenamed {
            from: "ts5/Old.md".into(),
            to: "ts5/New.md".into(),
            seq,
            result: Ok("ts5/New.md".into()),
        });
        // The typed edit's autosave, addressed to `from`, is a no-op after the follow.
        let _ = app.send(Message::Source(crate::message::SourceMsg::AutosaveDue {
            path: "ts5/Old.md".into(),
            seq: typed,
        }));
        assert_eq!(app.open_paths(), ["ts5/New.md"]);
        let doc = app.doc("ts5/New.md").expect("followed");
        assert_eq!(doc.text(), "---\ntitle: New\n---\ntyped\n");
        assert!(doc.is_dirty());
        assert!(gone(&d, "ts5/Old.md"));
        // Keep mine saves the text at `to`.
        let job = app
            .doc_mut("ts5/New.md")
            .and_then(|t| t.keep_mine(false))
            .expect("keep mine");
        let outcome = job.run(d.as_ref());
        let _ = app.send(job.done(outcome));
        assert_eq!(
            d.read_file("ts5/New.md").expect("to").0,
            b"---\ntitle: New\n---\ntyped\n"
        );
        assert!(gone(&d, "ts5/Old.md"));
    }

    /// A follow-up save in flight defers the rename until it lands.
    #[test]
    fn title_rename_waits_for_an_in_flight_follow_up_save() {
        let (_tmp, d) = fake_arc();
        let mut app = open_and_retitle(&d, "ts6/Old.md", "New");
        let first = app
            .doc_mut("ts6/Old.md")
            .and_then(|t| t.begin_save(false))
            .expect("save");
        let doc = app.doc_mut("ts6/Old.md").expect("open");
        doc.select_range((3, 0), (3, 1));
        assert!(doc.paste("z".into(), false, Instant::now()));
        assert!(doc.begin_save(false).is_none(), "queued as save_again");
        let outcome = first.run(d.as_ref());
        let _ = app.send(first.done(outcome));
        assert_eq!(
            app.open_paths(),
            ["ts6/Old.md"],
            "deferred while the follow-up saves"
        );
        assert!(app.doc("ts6/Old.md").is_some_and(|t| t.is_saving()));
        // The follow-up's task is not run by the harness: land it by hand.
        let doc = app.doc("ts6/Old.md").expect("open");
        let (seq, text) = (doc.edit_seq(), doc.text());
        let base = d.read_file("ts6/Old.md").expect("disk").1;
        let h = d.write_note("ts6/Old.md", &text, &base).expect("write");
        let _ = app.send(Message::Source(crate::message::SourceMsg::Saved {
            path: "ts6/Old.md".into(),
            seq,
            outcome: crate::source::SaveOutcome::Saved(h),
            after: None,
        }));
        assert_eq!(app.open_paths(), ["ts6/New.md"]);
        assert_eq!(
            d.read_file("ts6/New.md").expect("to").0,
            b"---\ntitle: New\n---\nz\n"
        );
    }
}

// plan editor-round-4 WP-4: image context menu, explorer row Move…, hover-to-focus.
mod round4_wp4 {
    use std::sync::Mutex;

    use super::wp4::{app_over, fake_arc};
    use super::*;
    use crate::clipboard_image::{ClipboardWriter, copy_image};
    use crate::context_menu::{Item, MenuMsg, Target, items};
    use crate::message::{PaletteMsg, PanesMsg, SourceMsg};
    use iced::widget::pane_grid::Pane;

    const AT: iced::Point = iced::Point::new(10.0, 10.0);

    #[derive(Default)]
    struct FakeWriter(Mutex<Vec<(String, Vec<u8>)>>);
    impl ClipboardWriter for FakeWriter {
        fn write(&self, mime: &str, bytes: &[u8]) -> std::result::Result<(), String> {
            self.0
                .lock()
                .expect("lock")
                .push((mime.to_owned(), bytes.to_vec()));
            Ok(())
        }
    }
    struct FailWriter;
    impl ClipboardWriter for FailWriter {
        fn write(&self, _: &str, _: &[u8]) -> std::result::Result<(), String> {
            Err("xclip unavailable: not found".into())
        }
    }

    fn img(src: &str, remote: bool) -> Target {
        Target::Image {
            src: src.into(),
            remote,
        }
    }

    fn labels(t: &Target) -> Vec<Item> {
        items(t).into_iter().map(|(i, _)| i).collect()
    }

    #[test]
    fn image_menu_items_vault_vs_remote() {
        assert_eq!(
            labels(&img("a/p.png", false)),
            [Item::CopyImage, Item::CopyPath]
        );
        assert_eq!(labels(&img("https://x/p.png", true)), [Item::CopyUrl]);
        for t in [img("a/p.png", false), img("https://x/p.png", true)] {
            assert!(!labels(&t).contains(&Item::OpenInNewPane), "audit E1");
        }
    }

    #[test]
    fn copy_image_writes_bytes_with_mime() {
        let (_tmp, d) = fake_arc();
        let p = d.vault_root().join("att");
        std::fs::create_dir_all(&p).expect("mkdir");
        std::fs::write(p.join("p.jpg"), b"\xff\xd8JPEG").expect("write");
        let w = FakeWriter::default();
        let r = copy_image(d.as_ref(), &w, "att/p.jpg");
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(
            *w.0.lock().expect("lock"),
            [("image/jpeg".to_owned(), b"\xff\xd8JPEG".to_vec())]
        );
        // A failing writer / missing file is an Err (status notice), never a panic.
        assert!(copy_image(d.as_ref(), &FailWriter, "att/p.jpg").is_err());
        assert!(copy_image(d.as_ref(), &w, "att/missing.png").is_err());
        let mut app = app_over(&d, None);
        let _ = app.send(Message::Menu(MenuMsg::ImageCopied(Err("boom".into()))));
        assert_eq!(app.status.error_text(), Some("boom"));
    }

    #[test]
    fn image_menu_copy_actions_route() {
        let (_tmp, d) = fake_arc();
        let mut app = app_over(&d, None);
        let _ = app.send(Message::Menu(MenuMsg::Open(img("att/p.png", false), AT)));
        let _ = app.send(Message::Menu(MenuMsg::Choose(Item::CopyImage)));
        assert_eq!(app.copied_images(), ["att/p.png"]);
        let _ = app.send(Message::Menu(MenuMsg::Open(img("att/p.png", false), AT)));
        let _ = app.send(Message::Menu(MenuMsg::Choose(Item::CopyPath)));
        let _ = app.send(Message::Menu(MenuMsg::Open(
            img("https://x/r.png", true),
            AT,
        )));
        let _ = app.send(Message::Menu(MenuMsg::Choose(Item::CopyUrl)));
        assert_eq!(app.copied(), ["att/p.png", "https://x/r.png"]);
        assert!(app.context_menu.is_none());
    }

    #[test]
    fn explorer_row_menu_has_move_and_view_has_no_button_row() {
        let (_tmp, d) = fake_arc();
        d.create_note("r4mv/a.md", "x").expect("create");
        let row = Target::Row {
            path: "r4mv/a.md".into(),
            folder: false,
        };
        assert!(labels(&row).contains(&Item::Move));
        let mut app = app_over(&d, None);
        let _ = app.send(Message::Menu(MenuMsg::Open(row, AT)));
        let _ = app.send(Message::Menu(MenuMsg::Choose(Item::Move)));
        let e = app.explorer.edit.as_ref().expect("inline edit");
        assert_eq!(e.kind, crate::explorer::EditKind::Move);
        assert_eq!(e.target, "r4mv/a.md");
        // The button row is gone: its buttons no longer appear in the explorer view source.
        let src = include_str!("explorer.rs");
        let view = &src[src.find("pub fn view<'a>").expect("view")..];
        let view = &view[..view.find("\n}\n").expect("end")];
        for label in ["\"New note\"", "\"Rename\"", "\"Move\"", "\"Trash\""] {
            assert!(!view.contains(label), "{label}");
        }
    }

    #[test]
    fn focus_follows_mouse_pref_round_trips() {
        use crate::settings::editor_prefs::{PrefEdit, apply, parse};
        let med = crate::theme::Surround::Med;
        assert!(!parse("", med).expect("p").focus_follows_mouse);
        let mut doc = cv_schema::ConfigDoc::parse("").expect("doc");
        apply(&mut doc, &PrefEdit::FocusFollowsMouse(true)).expect("apply");
        let text = doc.to_string();
        assert!(text.contains("focus_follows_mouse = true"), "{text}");
        assert!(parse(&text, med).expect("p").focus_follows_mouse);
    }

    /// Two panes: `a` in the first, `b` in the second (focused); hover focus on.
    fn two_panes(app: &mut App) -> (Pane, Pane) {
        let _ = app.send(Message::Tabs(TabsMsg::Open("r4h/a.md".into())));
        let first = app.focus;
        let _ = app.send(Message::Panes(PanesMsg::SplitRight));
        let second = app.focus;
        let _ = app.send(Message::Tabs(TabsMsg::Open("r4h/b.md".into())));
        app.prefs.focus_follows_mouse = true;
        (first, second)
    }

    #[test]
    fn hover_focus_ignored_when_off_with_modal_or_drag() {
        let (_tmp, d) = fake_arc();
        d.create_note("r4h/a.md", "aaa\n").expect("create");
        d.create_note("r4h/b.md", "bbb\n").expect("create");
        let mut app = app_over(&d, None);
        let (first, second) = two_panes(&mut app);
        app.prefs.focus_follows_mouse = false;
        let _ = app.send(Message::Panes(PanesMsg::Hover(first)));
        assert_eq!(app.focus, second, "pref off");
        app.prefs.focus_follows_mouse = true;
        let _ = app.send(Message::Palette(PaletteMsg::Open));
        assert!(app.palette.open);
        let _ = app.send(Message::Panes(PanesMsg::Hover(first)));
        assert_eq!(app.focus, second, "palette open");
        let _ = app.send(Message::Palette(PaletteMsg::Close));
        let _ = app.send(Message::Menu(MenuMsg::Open(img("x.png", false), AT)));
        let _ = app.send(Message::Panes(PanesMsg::Hover(first)));
        assert_eq!(app.focus, second, "context menu open");
        let _ = app.send(Message::Menu(MenuMsg::Close));
        let _ = app.send(Message::Panes(PanesMsg::MouseButton(true)));
        let _ = app.send(Message::Panes(PanesMsg::Hover(first)));
        assert_eq!(app.focus, second, "drag in progress");
        let _ = app.send(Message::Panes(PanesMsg::MouseButton(false)));
        let _ = app.send(Message::Panes(PanesMsg::Hover(first)));
        assert_eq!(app.focus, first);
    }

    #[test]
    fn hover_focus_routes_source_actions_to_the_entered_pane() {
        use iced::widget::text_editor::{Action, Edit};
        let (_tmp, d) = fake_arc();
        d.create_note("r4h/a.md", "aaa\n").expect("create");
        d.create_note("r4h/b.md", "bbb\n").expect("create");
        let mut app = app_over(&d, None);
        let (first, _second) = two_panes(&mut app);
        let _ = app.send(Message::Panes(PanesMsg::Hover(first)));
        assert_eq!(app.focus, first);
        assert_eq!(app.active_note().map(|t| t.path.as_str()), Some("r4h/a.md"));
        let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
            Edit::Insert('Z'),
        ))));
        assert!(app.doc("r4h/a.md").expect("a").text().contains('Z'));
        assert!(!app.doc("r4h/b.md").expect("b").text().contains('Z'));
    }
}

// ---------------------------------------------------------------------------------------------
// plan round-5-reading-pickers-intake WP-9: the `[[` note typeahead in Source
// ---------------------------------------------------------------------------------------------

mod round5_wp9 {
    use iced::keyboard;
    use iced::widget::text_editor::{self, Action, Binding, Edit, KeyPress};

    use super::*;
    use crate::message::{ExplorerMsg, LinkKey, ReadingMsg, SourceMsg};
    use crate::reading::ViewMode;

    type Fixture = (tempfile::TempDir, Arc<FakeDaemon>, App);

    const NOTE: &str = "w9/host.md";

    fn setup() -> Fixture {
        let (tmp, d) = fake();
        let d = Arc::new(d);
        d.create_note(NOTE, "").expect("create");
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let mut app = App::new(Some(dc), None);
        app.queue_rpcs();
        let _ = app.send(Message::Explorer(ExplorerMsg::Open(NOTE.into())));
        let _ = app.send(Message::Reading(ReadingMsg::SetMode {
            path: NOTE.into(),
            mode: ViewMode::Source,
        }));
        app.explorer.paths = [NOTE, "a/Alpha.md", "b/Beta.md", "x/Gamma.md", "y/Gamma.md"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        (tmp, d, app)
    }

    fn typ(app: &mut App, s: &str) {
        for c in s.chars() {
            let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
                Edit::Insert(c),
            ))));
        }
    }

    fn key(app: &mut App, k: LinkKey) {
        let _ = app.send(Message::Source(SourceMsg::LinkKey(k)));
    }

    fn text(app: &App) -> String {
        app.doc(NOTE).expect("doc").text()
    }

    fn labels(app: &App) -> Vec<String> {
        app.link_ta
            .as_ref()
            .expect("open")
            .visible
            .iter()
            .map(|c| c.value.clone())
            .collect()
    }

    #[test]
    fn double_bracket_opens_and_typing_narrows() {
        let (_t, _d, mut app) = setup();
        typ(&mut app, "see [");
        assert!(app.link_ta.is_none());
        typ(&mut app, "[");
        assert!(labels(&app).len() >= 4);
        typ(&mut app, "alp");
        assert_eq!(labels(&app).first().map(String::as_str), Some("a/Alpha.md"));
        assert_eq!(app.link_ta.as_ref().map(|a| a.query.as_str()), Some("alp"));
        typ(&mut app, "]]");
        assert!(app.link_ta.is_none(), "`]]` closes");
    }

    #[test]
    fn enter_inserts_stem_or_path_form_and_closes() {
        let (_t, _d, mut app) = setup();
        typ(&mut app, "[[alph");
        key(&mut app, LinkKey::Enter);
        assert!(app.link_ta.is_none());
        assert_eq!(text(&app), "[[Alpha]]");
        assert_eq!(app.doc(NOTE).expect("doc").cursor().position.column, 9);
        // Namesakes: x/Gamma.md wins the stem, y/Gamma.md gets the path form.
        typ(&mut app, " [[y/gamma");
        assert_eq!(labels(&app).first().map(String::as_str), Some("y/Gamma.md"));
        key(&mut app, LinkKey::Tab);
        assert_eq!(text(&app), "[[Alpha]] [[y/Gamma]]");
    }

    #[test]
    fn escape_closes_and_keeps_text_keys_do_not_leak() {
        let (_t, _d, mut app) = setup();
        typ(&mut app, "[[que");
        key(&mut app, LinkKey::Down);
        key(&mut app, LinkKey::Up);
        key(&mut app, LinkKey::Down);
        assert_eq!(text(&app), "[[que");
        key(&mut app, LinkKey::Escape);
        assert!(app.link_ta.is_none());
        assert_eq!(text(&app), "[[que");
        // The binding: with the list open, the keys become LinkKeys (never buffer edits).
        let kp = |k: keyboard::key::Named| KeyPress {
            key: keyboard::Key::Named(k),
            modified_key: keyboard::Key::Named(k),
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            status: text_editor::Status::Focused { is_hovered: false },
        };
        for (n, want) in [
            (keyboard::key::Named::ArrowUp, LinkKey::Up),
            (keyboard::key::Named::ArrowDown, LinkKey::Down),
            (keyboard::key::Named::Enter, LinkKey::Enter),
            (keyboard::key::Named::Tab, LinkKey::Tab),
            (keyboard::key::Named::Escape, LinkKey::Escape),
        ] {
            assert!(matches!(
                crate::source::test_key_binding_link(kp(n)),
                Some(Binding::Custom(Message::Source(SourceMsg::LinkKey(k)))) if k == want
            ));
        }
        assert!(!matches!(
            crate::source::test_key_binding(kp(keyboard::key::Named::ArrowDown)),
            Some(Binding::Custom(Message::Source(SourceMsg::LinkKey(_))))
        ));
    }

    #[test]
    fn zero_candidates_enter_falls_through_and_backspace_past_closes() {
        let (_t, _d, mut app) = setup();
        typ(&mut app, "[[zzzqqq");
        assert!(labels(&app).is_empty());
        key(&mut app, LinkKey::Enter);
        assert!(app.link_ta.is_none());
        assert_eq!(text(&app), "[[zzzqqq\n");
        typ(&mut app, "[[");
        assert!(app.link_ta.is_some());
        let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
            Edit::Backspace,
        ))));
        assert!(app.link_ta.is_none());
    }

    #[test]
    fn works_on_split_source_side_and_closes_on_reading() {
        let (_t, _d, mut app) = setup();
        let _ = app.send(Message::Reading(ReadingMsg::SetMode {
            path: NOTE.into(),
            mode: ViewMode::Split,
        }));
        typ(&mut app, "[[bet");
        assert_eq!(labels(&app).first().map(String::as_str), Some("b/Beta.md"));
        key(&mut app, LinkKey::Enter);
        assert_eq!(text(&app), "[[Beta]]");
        typ(&mut app, "[[");
        assert!(app.link_ta.is_some());
        let _ = app.send(Message::Reading(ReadingMsg::SetMode {
            path: NOTE.into(),
            mode: ViewMode::Reading,
        }));
        assert!(app.link_ta.is_none());
    }
}

// Plan round-7 WP-4 (audit E1): the Source↔Reading scroll matrix. Drives the real widget tree
// (headless tiny-skia) and executes every programmatic scroll / probe the app emits
// (`reading::ScrollIntent`) against the laid-out tree, so positions are measured post-layout:
// Reading = the scrollable's offset and the block at the viewport top (the app's `ProbeOp`);
// Source = the editor's real scroll, read from the caret rectangle the focused `text_editor`
// reports to the input method (never the `top_line` estimate).
mod scroll_matrix {
    use iced::advanced::widget::operation::scrollable as op;
    use iced::advanced::widget::{Operation, Tree, operation};
    use iced::advanced::{InputMethod, Shell, clipboard, layout, mouse};
    use iced::keyboard::{self, Key, Location, Modifiers, key};
    use iced::widget::text_editor::{Action, Motion};
    use iced::{Event, Rectangle, Size, Vector};

    use super::*;
    use crate::message::{FindMsg, ReadingMsg};
    use crate::reading::{ScrollIntent, ViewMode};

    const PATH: &str = "srm/long.md";
    const W: f32 = 1000.0;
    const H: f32 = 700.0;

    /// 160 blocks: headings, short paragraphs and every fourth paragraph long enough to wrap
    /// several times; "needle" sits in a few blocks spread through the note.
    fn body() -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        for i in 0..80 {
            let _ = write!(s, "## Section {i}\n\n");
            if i % 4 == 1 {
                for w in 0..160 {
                    let _ = write!(s, "word{i}x{w} ");
                }
                s.push_str("end\n\n");
            } else if i % 13 == 5 {
                let _ = write!(s, "Paragraph {i} has the needle in it.\n\n");
            } else {
                let _ = write!(s, "Paragraph {i} is short.\n\n");
            }
        }
        s
    }

    struct Ui {
        renderer: iced::Renderer,
        tree: Option<Tree>,
        /// The note's render model (Source mode keeps no reading doc), mapping lines to blocks.
        model: editor_model::render::RenderModel,
    }

    fn intents() -> Vec<ScrollIntent> {
        crate::reading::INTENTS.with(|i| std::mem::take(&mut *i.borrow_mut()))
    }

    impl Ui {
        fn with<R>(
            &mut self,
            app: &App,
            f: impl FnOnce(
                &mut iced::Element<'_, Message>,
                &mut Tree,
                layout::Layout<'_>,
                &iced::Renderer,
            ) -> R,
        ) -> R {
            let mut el = app.view();
            let tree = match &mut self.tree {
                Some(t) => {
                    t.diff(&el);
                    t
                }
                None => self.tree.insert(Tree::new(&el)),
            };
            let limits = layout::Limits::new(Size::ZERO, Size::new(W, H));
            let node = el.as_widget_mut().layout(tree, &self.renderer, &limits);
            f(&mut el, tree, layout::Layout::new(&node), &self.renderer)
        }

        fn operate(&mut self, app: &App, o: &mut dyn Operation<()>) {
            self.with(app, |el, tree, l, r| {
                el.as_widget_mut().operate(tree, l, r, o);
            });
        }

        /// Sends `event`, feeds the messages to the app, then runs the emitted intents.
        /// Returns the input method the tree requested.
        fn send(&mut self, app: &mut App, event: &Event, cursor: mouse::Cursor) -> InputMethod {
            let (msgs, im) = self.with(app, |el, tree, l, r| {
                let mut msgs = Vec::new();
                let mut shell = Shell::new(&mut msgs);
                el.as_widget_mut().update(
                    tree,
                    event,
                    l,
                    cursor,
                    r,
                    &mut clipboard::Null,
                    &mut shell,
                    &Rectangle::with_size(Size::new(W, H)),
                );
                let im = shell.input_method().clone();
                (msgs, im)
            });
            for m in msgs {
                let _ = app.send(m);
            }
            self.drain(app);
            im
        }

        fn redraw(&mut self, app: &mut App) -> InputMethod {
            self.send(
                app,
                &Event::Window(iced::window::Event::RedrawRequested(
                    std::time::Instant::now(),
                )),
                mouse::Cursor::Unavailable,
            )
        }

        fn update(&mut self, app: &mut App, m: Message) {
            let _ = app.send(m);
            self.drain(app);
        }

        /// Executes the app's pending scroll / probe intents on the laid-out tree (probe
        /// answers are fed back, and their own intents run in turn).
        fn drain(&mut self, app: &mut App) {
            for _ in 0..50 {
                let pending = intents();
                if pending.is_empty() {
                    return;
                }
                for i in pending {
                    self.apply(app, i);
                }
            }
            panic!("intent loop never settled");
        }

        fn apply(&mut self, app: &mut App, i: ScrollIntent) {
            let id = crate::find::reading_scroll_id;
            match i {
                ScrollIntent::SnapTo { path, view, y } => {
                    let mut o = op::snap_to::<()>(
                        id(&path, view),
                        op::RelativeOffset {
                            x: Some(0.0),
                            y: Some(y),
                        },
                    );
                    self.operate(app, &mut o);
                }
                ScrollIntent::ScrollTo { path, view, y } => {
                    let mut o = op::scroll_to::<()>(
                        id(&path, view),
                        op::AbsoluteOffset {
                            x: None,
                            y: Some(y),
                        },
                    );
                    self.operate(app, &mut o);
                }
                ScrollIntent::Step { path, view, dy } => {
                    let mut o =
                        op::scroll_by::<()>(id(&path, view), op::AbsoluteOffset { x: 0.0, y: dy });
                    self.operate(app, &mut o);
                }
                ScrollIntent::Page { path, view, down } => {
                    let g = self.geometry(app, &path, view).expect("reading scrollable");
                    let dy = crate::page::page_delta(g.bounds.height, down);
                    let mut o =
                        op::scroll_by::<()>(id(&path, view), op::AbsoluteOffset { x: 0.0, y: dy });
                    self.operate(app, &mut o);
                }
                ScrollIntent::SourceRelayout { path, lines } => {
                    // After the next layout (the caret move scrolls the editor there).
                    self.with(app, |_, _, _, _| ());
                    let _ = app.send(Message::Source(crate::message::SourceMsg::Viewport {
                        path,
                        lines,
                    }));
                }
                ScrollIntent::Probe {
                    path,
                    view,
                    blocks,
                    scroll_to,
                } => {
                    let mut p = crate::reading::probe_op(&path, view, blocks);
                    self.operate(app, &mut operation::black_box(&mut p));
                    let probe = match p.finish() {
                        operation::Outcome::Some(p) => Some(p),
                        _ => None,
                    };
                    let _ = app.send(Message::Reading(ReadingMsg::Probed {
                        path,
                        view,
                        probe,
                        scroll_to,
                    }));
                }
            }
        }

        fn geometry(&mut self, app: &App, path: &str, view: u64) -> Option<Geometry> {
            let mut g = GeomOp {
                id: crate::find::reading_scroll_id(path, view),
                out: None,
            };
            self.operate(app, &mut g);
            g.out
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct Geometry {
        bounds: Rectangle,
        content: Rectangle,
        offset: f32,
    }

    struct GeomOp {
        id: iced::widget::Id,
        out: Option<Geometry>,
    }

    impl Operation<()> for GeomOp {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<()>)) {
            operate(self);
        }
        fn scrollable(
            &mut self,
            id: Option<&iced::widget::Id>,
            bounds: Rectangle,
            content: Rectangle,
            t: Vector,
            _s: &mut dyn operation::Scrollable,
        ) {
            if id == Some(&self.id) {
                self.out = Some(Geometry {
                    bounds,
                    content,
                    offset: t.y,
                });
            }
        }
    }

    /// A measured place: the top block, and the edge when one is reached (`Some(true)` = end).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Place {
        block: usize,
        edge: Option<bool>,
    }

    fn view(app: &App) -> u64 {
        app.view_of(PATH)
    }

    fn block_of_line(ui: &Ui, line: usize) -> usize {
        ui.model.block_for_line(line).expect("block")
    }

    /// Reading's measured place.
    fn reading_place(ui: &mut Ui, app: &App) -> Place {
        let v = view(app);
        let g = ui
            .geometry(app, PATH, v)
            .expect("reading scrollable laid out");
        let n = app.reading.doc(PATH).map_or(0, |d| d.model.blocks.len());
        let mut p = crate::reading::probe_op(PATH, v, n);
        ui.operate(app, &mut operation::black_box(&mut p));
        let operation::Outcome::Some(probe) = p.finish() else {
            panic!("probe found no scrollable");
        };
        let max = (g.content.height - g.bounds.height).max(0.0);
        let edge = if g.offset <= 0.5 {
            Some(false)
        } else if g.offset >= max - 0.5 {
            Some(true)
        } else {
            None
        };
        Place {
            block: probe.top_block().unwrap_or(0),
            edge,
        }
    }

    fn caret_y(im: &InputMethod) -> f32 {
        match im {
            InputMethod::Enabled { cursor, .. } => cursor.y,
            InputMethod::Disabled => panic!("source editor reports no caret (not focused?)"),
        }
    }

    fn src(app: &mut App) -> &mut iced::widget::text_editor::Content {
        app.docs.get_mut(PATH).expect("doc").content_mut()
    }

    fn focus_source(ui: &mut Ui, app: &App) {
        let mut f = operation::focusable::focus::<()>(crate::source::editor_id(PATH));
        ui.operate(app, &mut f);
    }

    /// Source's measured place: the source line at the editor's viewport top, from the real
    /// scroll (caret offset vs. the fully-scrolled-up caret, in visual rows, mapped to a source
    /// line by walking visual rows down from the start). Leaves scroll (to the row; the end
    /// exactly) and caret as found.
    fn source_place(ui: &mut Ui, app: &mut App) -> Place {
        source_place_at(ui, app, 0)
    }

    /// [`source_place`] of the line `extra` visual rows below the viewport top (plan
    /// round-8 WP-4: the anchor height).
    fn source_place_at(ui: &mut Ui, app: &mut App, extra: i32) -> Place {
        focus_source(ui, app);
        // With a selection (find in Source) the editor reports the first *visible* selection
        // rectangle to the input method — (0, 0) once the calibration scroll hides it
        // (iced_widget 0.14.2 text_editor.rs:359-364) — so measure with it collapsed (the
        // caret stays put, so the view does not move) and put the selection back below.
        let saved = src(app).cursor();
        src(app).move_to(iced::widget::text_editor::Cursor {
            position: saved.position,
            selection: None,
        });
        let y_a = caret_y(&ui.redraw(app));
        // At the end when one more row down changes nothing.
        src(app).perform(Action::Scroll { lines: 1 });
        let at_end = (caret_y(&ui.redraw(app)) - y_a).abs() < 0.5;
        if !at_end {
            src(app).perform(Action::Scroll { lines: -1 });
        }
        src(app).perform(Action::Scroll { lines: -1_000_000 });
        let y_top = caret_y(&ui.redraw(app));
        src(app).perform(Action::Scroll { lines: 1 });
        let lh = y_top - caret_y(&ui.redraw(app));
        assert!(lh > 1.0, "the note must overflow the source viewport");
        #[allow(clippy::cast_possible_truncation)]
        let rows = ((y_top - y_a) / lh + 0.05).floor().max(0.0) as i32;
        let restore = |ui: &mut Ui, app: &mut App| {
            src(app).perform(Action::Scroll { lines: -1_000_000 });
            let _ = ui.redraw(app);
            let by = if at_end { 1_000_000 } else { rows };
            src(app).perform(Action::Scroll { lines: by });
            let y = caret_y(&ui.redraw(app));
            assert!(
                (y - y_a).abs() < lh,
                "source scroll restored: a {y_a} top {y_top} lh {lh} rows {rows} now {y}"
            );
        };
        // Rows to source line: walk the caret `rows` visual rows down from the start. Twice:
        // lines cosmic has not shaped yet (a find jump skips them) count as one row each on
        // the first walk, which shapes them; the second walk counts wrapped rows exactly.
        for _ in 0..2 {
            src(app).move_to(iced::widget::text_editor::Cursor {
                position: iced::widget::text_editor::Position { line: 0, column: 0 },
                selection: None,
            });
            for _ in 0..rows + extra {
                src(app).perform(Action::Move(Motion::Down));
            }
        }
        let line = src(app).cursor().position.line;
        src(app).move_to(saved);
        let _ = ui.redraw(app); // the caret move scrolls to it here
        restore(ui, app);
        let edge = if rows == 0 {
            Some(false)
        } else if at_end {
            Some(true)
        } else {
            None
        };
        Place {
            block: block_of_line(ui, line),
            edge,
        }
    }

    fn place(ui: &mut Ui, app: &mut App) -> Place {
        if app.reading.mode(PATH) == ViewMode::Source {
            source_place(ui, app)
        } else {
            reading_place(ui, app)
        }
    }

    fn setup(mode: ViewMode) -> (tempfile::TempDir, App, Ui) {
        let (tmp, d) = fake();
        let d = Arc::new(d);
        d.create_note(PATH, &body()).expect("create");
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let mut app = App::new(Some(dc), None);
        app.queue_rpcs();
        let _ = intents();
        let mut ui = Ui {
            renderer: super::find_return_leak::renderer(),
            tree: None,
            model: editor_model::render::RenderModel::default(),
        };
        ui.update(&mut app, Message::Tabs(TabsMsg::Open(PATH.into())));
        assert_eq!(app.active_tab().map(|t| t.path.as_str()), Some(PATH));
        // Rendered once in Reading, so every mode can map source lines to blocks.
        for m in [ViewMode::Reading, mode] {
            ui.update(
                &mut app,
                Message::Reading(ReadingMsg::SetMode {
                    path: PATH.into(),
                    mode: m,
                }),
            );
            ui.update(&mut app, Message::Reading(ReadingMsg::Tick));
            if let Some(d) = app.reading.doc(PATH) {
                ui.model = d.model.clone();
            }
        }
        assert!(ui.model.blocks.len() > 100, "reading doc rendered");
        let _ = src(&mut app);
        let _ = ui.redraw(&mut app);
        let _ = ui.redraw(&mut app);
        if mode == ViewMode::Source {
            focus_source(&mut ui, &app);
        }
        (tmp, app, ui)
    }

    const NOTE_AT: iced::Point = iced::Point::new(W * 0.6, H * 0.5);

    fn wheel(ui: &mut Ui, app: &mut App, down: bool, n: usize) {
        let c = mouse::Cursor::Available(NOTE_AT);
        let _ = ui.send(
            app,
            &Event::Mouse(mouse::Event::CursorMoved { position: NOTE_AT }),
            c,
        );
        for _ in 0..n {
            let y = if down { -3.0 } else { 3.0 };
            let _ = ui.send(
                app,
                &Event::Mouse(mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Lines { x: 0.0, y },
                }),
                c,
            );
        }
    }

    fn key_press(ui: &mut Ui, app: &mut App, k: key::Named) {
        let k = Key::Named(k);
        let _ = ui.send(
            app,
            &Event::Keyboard(keyboard::Event::KeyPressed {
                key: k.clone(),
                modified_key: k,
                physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                location: Location::Standard,
                modifiers: Modifiers::empty(),
                text: None,
                repeat: false,
            }),
            mouse::Cursor::Unavailable,
        );
    }

    #[derive(Clone, Copy, Debug)]
    enum Input {
        Wheel,
        Drag,
        Page,
        Line,
        Edge,
        Find,
    }

    /// Applies `input` (`down` = towards the end) the way the user drives it in the current
    /// mode; "up" starts from well down the note.
    fn drive(ui: &mut Ui, app: &mut App, input: Input, down: bool) {
        let source = app.reading.mode(PATH) == ViewMode::Source;
        if !down {
            wheel(ui, app, true, 40);
        }
        match input {
            Input::Wheel => wheel(ui, app, down, 12),
            Input::Drag => {
                let v = view(app);
                let y = if down { 0.55 } else { 0.25 };
                ui.apply(
                    app,
                    ScrollIntent::SnapTo {
                        path: PATH.into(),
                        view: v,
                        y,
                    },
                );
                ui.update(
                    app,
                    Message::Reading(ReadingMsg::Scrolled {
                        path: PATH.into(),
                        view: v,
                        y,
                    }),
                );
            }
            Input::Page => {
                for _ in 0..4 {
                    if source {
                        let k = if down {
                            key::Named::PageDown
                        } else {
                            key::Named::PageUp
                        };
                        key_press(ui, app, k);
                    } else {
                        ui.update(app, Message::Panes(crate::message::PanesMsg::Page(down)));
                    }
                }
            }
            Input::Line => {
                for _ in 0..30 {
                    if source {
                        let k = if down {
                            key::Named::ArrowDown
                        } else {
                            key::Named::ArrowUp
                        };
                        key_press(ui, app, k);
                    } else {
                        let m = if down {
                            crate::message::PaletteMsg::Down
                        } else {
                            crate::message::PaletteMsg::Up
                        };
                        ui.update(app, Message::Palette(m));
                    }
                }
            }
            Input::Edge => ui.update(app, Message::Reading(ReadingMsg::GoToEdge { bottom: down })),
            Input::Find => {
                ui.update(app, Message::Find(FindMsg::Open));
                ui.update(app, Message::Find(FindMsg::Query("needle".into())));
                for _ in 0..3 {
                    let m = if down { FindMsg::Next } else { FindMsg::Prev };
                    ui.update(app, Message::Find(m));
                }
            }
        }
        let _ = ui.redraw(app);
    }

    /// Same top block, or the same edge when the edge was reached.
    fn same(a: Place, b: Place) -> bool {
        a.block == b.block || (a.edge.is_some() && a.edge == b.edge)
    }

    /// Runs one case; `Err(why)` when the place did not survive.
    fn case(origin: ViewMode, input: Input, down: bool, switch: bool) -> Result<(), String> {
        let (_tmp, mut app, mut ui) = setup(origin);
        drive(&mut ui, &mut app, input, down);
        let before = place(&mut ui, &mut app);
        if matches!(input, Input::Find) {
            ui.update(&mut app, Message::Find(FindMsg::Close));
            let _ = ui.redraw(&mut app);
            let closed = place(&mut ui, &mut app);
            if !same(before, closed) {
                return Err(format!("find close: {before:?} -> {closed:?}"));
            }
        }
        if !switch {
            return Ok(());
        }
        let to = match origin {
            ViewMode::Source => ViewMode::Reading,
            ViewMode::Reading | ViewMode::Split => ViewMode::Source,
        };
        ui.update(
            &mut app,
            Message::Reading(ReadingMsg::SetMode {
                path: PATH.into(),
                mode: to,
            }),
        );
        let _ = ui.redraw(&mut app);
        let _ = ui.redraw(&mut app);
        let switched = place(&mut ui, &mut app);
        if !same(before, switched) {
            return Err(format!("mode switch: {before:?} -> {switched:?}"));
        }
        Ok(())
    }

    fn run(cases: &[(ViewMode, Input, bool, bool)]) {
        let mut failed = Vec::new();
        for &(origin, input, down, switch) in cases {
            let dir = if down { "down" } else { "up" };
            let name = format!("{origin:?}/{input:?}/{dir}");
            if let Err(why) = case(origin, input, down, switch) {
                failed.push(format!("{name}: {why}"));
            }
        }
        assert!(
            failed.is_empty(),
            "{} of {} cases failed:\n{}",
            failed.len(),
            cases.len(),
            failed.join("\n")
        );
    }

    const INPUTS: [Input; 6] = [
        Input::Wheel,
        Input::Drag,
        Input::Page,
        Input::Line,
        Input::Edge,
        Input::Find,
    ];

    /// Reading → Source after Ctrl+PgUp/PgDn lands at the edge; find navigation in Reading
    /// and in Split (the no-visible-occurrence path) keeps its place when find closes.
    #[test]
    fn reading_place_survives_edge_switch_and_find_close() {
        let mut cases = Vec::new();
        for down in [true, false] {
            cases.push((ViewMode::Reading, Input::Edge, down, true));
            cases.push((ViewMode::Reading, Input::Find, down, false));
            cases.push((ViewMode::Split, Input::Find, down, false));
        }
        run(&cases);
    }

    /// Reading → Source block carry after wheel, scrollbar drag, PageUp/PageDown, line steps
    /// and find navigation, both directions.
    #[test]
    fn reading_block_carries_to_source() {
        let mut cases = Vec::new();
        for input in [
            Input::Wheel,
            Input::Drag,
            Input::Page,
            Input::Line,
            Input::Find,
        ] {
            for down in [true, false] {
                cases.push((ViewMode::Reading, input, down, true));
            }
        }
        run(&cases);
    }

    /// Source → Reading: wheel, PageUp/PageDown, arrow steps and Ctrl+PgUp/PgDn × both
    /// directions.
    #[test]
    fn source_place_survives_switch_and_find_close() {
        let mut cases = Vec::new();
        for input in INPUTS {
            if matches!(input, Input::Drag | Input::Find) {
                continue;
            }
            for down in [true, false] {
                cases.push((ViewMode::Source, input, down, true));
            }
        }
        run(&cases);
    }

    /// Source/Wheel → Split: the source pane stays shown, so its top must not move (the
    /// switch measures the Source top with a caret hit-test and puts the caret back).
    #[test]
    fn source_wheel_top_survives_switch_to_split() {
        for down in [true, false] {
            let (_tmp, mut app, mut ui) = setup(ViewMode::Source);
            drive(&mut ui, &mut app, Input::Wheel, down);
            let before = source_place(&mut ui, &mut app);
            ui.update(
                &mut app,
                Message::Reading(ReadingMsg::SetMode {
                    path: PATH.into(),
                    mode: ViewMode::Split,
                }),
            );
            let _ = ui.redraw(&mut app);
            let _ = ui.redraw(&mut app);
            let after = source_place(&mut ui, &mut app);
            assert!(same(before, after), "down={down}: {before:?} -> {after:?}");
        }
    }

    /// Plan round-8 WP-4: the place under the 50% anchor (Reading: the probe's anchor block;
    /// Source: the line half the measured viewport rows below the top).
    fn anchor_place(ui: &mut Ui, app: &mut App) -> Place {
        if app.reading.mode(PATH) == ViewMode::Source {
            let rows = app.docs.get(PATH).map_or(0, |d| d.visible_lines());
            let half = i32::try_from(crate::source::rows_at(0.5, rows)).unwrap_or(0);
            return source_place_at(ui, app, half);
        }
        let top = reading_place(ui, app);
        let v = view(app);
        let n = app.reading.doc(PATH).map_or(0, |d| d.model.blocks.len());
        let mut p = crate::reading::probe_op(PATH, v, n);
        ui.operate(app, &mut operation::black_box(&mut p));
        let operation::Outcome::Some(probe) = p.finish() else {
            panic!("probe found no scrollable");
        };
        Place {
            block: probe.anchor_block(0.5).unwrap_or(0),
            edge: top.edge,
        }
    }

    /// Plan round-8 WP-4: with `scroll_anchor = 50`, wheels `n` steps down from `origin`, then
    /// switches mode; returns the (before, after) anchor places.
    fn anchored_switch(origin: ViewMode, n: usize) -> (Place, Place) {
        let (_tmp, mut app, mut ui) = setup(origin);
        app.prefs.scroll_anchor = 50;
        app.reading.set_anchor(app.prefs.anchor());
        wheel(&mut ui, &mut app, true, n);
        let _ = ui.redraw(&mut app);
        let before = anchor_place(&mut ui, &mut app);
        let to = if origin == ViewMode::Source {
            ViewMode::Reading
        } else {
            ViewMode::Source
        };
        ui.update(
            &mut app,
            Message::Reading(ReadingMsg::SetMode {
                path: PATH.into(),
                mode: to,
            }),
        );
        let _ = ui.redraw(&mut app);
        let _ = ui.redraw(&mut app);
        (before, anchor_place(&mut ui, &mut app))
    }

    /// Plan round-8 WP-4: mid-document, the block under the 50% anchor survives Reading →
    /// Source and Source → Reading.
    #[test]
    fn anchor_50_carries_mid_document_both_ways() {
        // Wheel counts that put a short block (not a long wrapped paragraph spanning both the
        // top and the anchor) under the anchor, so a top carry would fail here.
        for (origin, n) in [(ViewMode::Reading, 16), (ViewMode::Source, 20)] {
            let (before, after) = anchored_switch(origin, n);
            assert!(before.edge.is_none(), "{origin:?}: mid-document {before:?}");
            assert_eq!(
                before.block, after.block,
                "{origin:?}: {before:?} -> {after:?}"
            );
        }
    }

    /// Plan round-8 WP-4 (audit M2): a target past the last reachable top lands at the end —
    /// the anchor shift is applied to the unclamped target, then clamped once.
    #[test]
    fn anchor_50_near_end_lands_at_the_end() {
        for origin in [ViewMode::Reading, ViewMode::Source] {
            let (before, after) = anchored_switch(origin, 400);
            assert_eq!(before.edge, Some(true), "{origin:?}: wheeled to the end");
            assert!(
                after.edge == Some(true) || after.block == before.block,
                "{origin:?}: {before:?} -> {after:?}"
            );
        }
    }

    /// Source find navigation, then find close and a switch to Reading.
    #[test]
    fn source_find_place_carries_to_reading() {
        run(&[
            (ViewMode::Source, Input::Find, true, true),
            (ViewMode::Source, Input::Find, false, true),
        ]);
    }
}

/// Plan round-7-feedback-decisions WP-5: view-fence text and rows scale with `font_size`.
mod view_font_scale {
    use crate::reading::view::{block_size, reading_scale, scaled};
    use crate::views::{MAX_HEIGHT, ROW_HEIGHT, VIRTUAL_THRESHOLD, px, row_height};
    use crate::widget::virtual_list::{ListWindow, OVERSCAN, VirtualList};

    /// View text keeps its design ratio to the Reading paragraph at every `font_size`.
    #[test]
    fn view_text_tracks_font_size() {
        for fs in [8.0_f32, 16.0, 22.0, 40.0, 64.0] {
            let s = reading_scale(fs);
            let para = scaled(block_size(None), s);
            assert!((para - fs).abs() < 1e-3, "paragraph = font_size at {fs}");
            for design in [12.0_f32, 13.0, 14.0] {
                assert!((px(design, s) - scaled(design, s)).abs() < 1e-3);
                if fs >= 16.0 {
                    assert!((px(design, s) / para - design / 16.0).abs() < 1e-3, "{fs}");
                }
            }
            assert!((row_height(s) - ROW_HEIGHT * fs / 16.0).abs() < 1e-3);
        }
    }

    /// Audit E4: a virtualized view at `font_size` 40 lays rows out at the scaled height and its
    /// visible range covers exactly the window at that height (no clipped / overlapping rows).
    #[test]
    fn virtualized_view_rows_scale_at_large_font() {
        let h = row_height(reading_scale(40.0));
        assert!((h - 65.0).abs() < 1e-3, "{h}");
        let lines: Vec<u32> = (0..200).collect();
        assert!(lines.len() > VIRTUAL_THRESHOLD);
        let window = ListWindow {
            offset: 20.0 * h,
            height: MAX_HEIGHT,
        };
        let r = VirtualList::new(&lines, h, window, |w| w).range();
        assert_eq!(r.start, 20 - OVERSCAN);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let shown = (MAX_HEIGHT / h).ceil() as usize;
        assert_eq!(r.end, 20 + shown + OVERSCAN);
        // At the unscaled height the same window would build far more rows than fit.
        let flat = VirtualList::new(&lines, ROW_HEIGHT, window, |w| w).range();
        assert!(flat.len() > r.len());
    }
}

// ---------------------------------------------------------------------------------------------
// Plan tn-review-baseline-remediation WP-R2: one save-then-continue path, one settings-save
// completion, modal classification (SR1, SR2, SR3). In-flight races use `update` (no pump),
// inject the racing message, then `pump`.
// ---------------------------------------------------------------------------------------------

mod wp_r2 {
    use iced::widget::text_editor::{Action, Edit};

    use super::*;
    use crate::bookmarks::BookmarkMsg;
    use crate::message::{ExplorerMsg, PanesMsg, SourceMsg, SwitcherMsg};
    use crate::panes;

    fn setup(notes: &[&str]) -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
        let (tmp, d) = fake();
        let d = Arc::new(d);
        for p in notes {
            d.create_note(p, "body").expect("create");
        }
        super::wp4::register_create(&d);
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let inner = Arc::clone(&d);
        let c: crate::daemon::Connector =
            Arc::new(move || Ok(Arc::clone(&inner) as Arc<dyn DaemonClient>));
        let mut app = App::new(Some(dc), None).with_connector(c);
        app.queue_rpcs();
        for p in notes {
            let _ = app.send(Message::Explorer(ExplorerMsg::Open((*p).to_owned())));
        }
        (tmp, d, app)
    }

    fn type_x() -> Message {
        Message::Source(SourceMsg::Action(Action::Edit(Edit::Insert('x'))))
    }

    /// Takes every queued RPC completion (to deliver after a racing message).
    fn hold(app: &mut App) -> Vec<Message> {
        match &mut app.rpc_exec {
            crate::app::RpcExec::Queue(q) => q.drain(..).collect(),
            crate::app::RpcExec::Pool => Vec::new(),
        }
    }

    fn some_dialog() -> crate::dialog::Dialog {
        crate::dialog::Dialog::unsaved(
            crate::dialog::UnsavedEdits::default(),
            crate::dialog::OnOk::Quit,
        )
    }

    /// SR1: a bookmark write still in flight when quit starts lets the quit finish.
    #[test]
    fn bookmark_write_in_flight_at_quit_completes_the_quit() {
        let (_tmp, _d, mut app) = setup(&["r2bm/a.md"]);
        let _ = app.update(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::AddActive)));
        let _ = app.update(Message::Tabs(TabsMsg::Quit));
        assert!(!app.exiting, "waits for the bookmark write");
        app.pump();
        assert!(app.exiting, "the landed bookmark write ends the quit");
    }

    /// SR2: an edit typed (and its save requested) while a save-before-close is in flight is
    /// saved by the follow-up, and the close then completes.
    #[test]
    fn edit_typed_during_a_save_first_keeps_its_follow_up_save() {
        let (_tmp, d, mut app) = setup(&["r2fu/a.md", "r2fu/b.md"]);
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("r2fu/a.md".into())));
        let _ = app.send(type_x());
        let _ = app.update(Message::Tabs(TabsMsg::CloseActive));
        let _ = app.update(type_x());
        let _ = app.update(Message::Source(SourceMsg::Save));
        app.pump();
        assert_eq!(d.read_file("r2fu/a.md").expect("file").0, b"xxbody");
        assert!(panes::find(&app.panes, "r2fu/a.md").is_none(), "closed");
        assert!(app.dialog.is_none(), "{:?}", app.status.notice);
    }

    /// SR2: a reconnect resync deferred behind a save-before-delete runs when it lands.
    #[test]
    fn resync_deferred_behind_a_save_first_resolves() {
        use crate::dialog::DialogMsg;
        let (_tmp, _d, mut app) = setup(&["r2rs/a.md"]);
        let _ = app.send(type_x());
        let _ = app.send(Message::Explorer(ExplorerMsg::DeleteActive));
        let _ = app.update(Message::Dialog(DialogMsg::Ok));
        let held = hold(&mut app);
        assert_eq!(held.len(), 1, "the save before the delete");
        let _ = app.send(Message::Status(StatusMsg::Disconnected));
        let _ = app.send(Message::Status(StatusMsg::ReconnectDue));
        assert!(app.doc("r2rs/a.md").is_some_and(|t| t.is_saving()));
        for m in held {
            let _ = app.send(m);
        }
        let t = app.doc_mut("r2rs/a.md").expect("open until confirmed");
        assert!(!t.take_resync_pending(), "the deferred resync ran");
        assert!(
            app.dialog
                .as_ref()
                .and_then(|d| d.delete_confirm())
                .is_some(),
            "the delete confirm follows"
        );
    }

    /// SR3: an async completion (`Created`) arriving while a dialog is open is applied.
    #[test]
    fn created_arriving_under_a_dialog_is_applied() {
        let (_tmp, d, mut app) = setup(&[]);
        let _ = app.send(Message::Switcher(SwitcherMsg::Open));
        let _ = app.send(Message::Switcher(SwitcherMsg::Query("r2cr new".into())));
        let _ = app.update(Message::Switcher(SwitcherMsg::Submit));
        app.dialog = Some(some_dialog());
        app.pump();
        assert!(d.read_file("r2cr new.md").is_ok());
        assert_eq!(
            app.active_tab().map(|t| t.path.as_str()),
            Some("r2cr new.md"),
            "the created note opened"
        );
    }

    /// SR3: Shift released while a dialog is open does not stay held.
    #[test]
    fn shift_released_under_a_dialog_does_not_stick() {
        let (_tmp, d, mut app) = setup(&["r2sh/Works.md"]);
        let _ = app.send(Message::Modifiers(iced::keyboard::Modifiers::SHIFT));
        app.dialog = Some(some_dialog());
        let _ = app.send(Message::Modifiers(iced::keyboard::Modifiers::empty()));
        app.dialog = None;
        let _ = app.send(Message::Switcher(SwitcherMsg::Open));
        let _ = app.send(Message::Switcher(SwitcherMsg::Query("works".into())));
        let _ = app.send(Message::Switcher(SwitcherMsg::Submit));
        assert!(
            d.read_file("works.md").is_err(),
            "plain Enter opens the match"
        );
        assert!(app.status.error_text().is_none(), "{:?}", app.status.notice);
        assert_eq!(
            app.active_tab().map(|t| t.path.as_str()),
            Some("r2sh/Works.md")
        );
    }
}
