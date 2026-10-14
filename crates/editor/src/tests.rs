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
    assert!(app.files.contains(&p));

    let _ = app.update(Message::Tabs(TabsMsg::Open(p.clone())));
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.active_tab().map(|t| t.source.text()), Some(expected));
    let _ = app.update(Message::Tabs(TabsMsg::Open(p.clone())));
    assert_eq!(app.tabs.len(), 1, "re-open focuses the tab");
    let _ = app.update(Message::Tabs(TabsMsg::Open("missing.md".into())));
    assert_eq!(app.tabs.len(), 1);
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
    assert_eq!(restored.tabs.len(), 1);
    assert_eq!(restored.ui.recent.first(), Some(&p));
}
