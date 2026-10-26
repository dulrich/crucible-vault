//! In-process daemon tests on a synthetic temp vault (live mode; [`Env::new`] asserts the vault lies
//! under the temp dir). Never touches the live vault, the real clipboard, or any scheme handler.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cv::{Client, ClientError, Config, Daemon, DaemonOptions, RunningDaemon};
use cv_protocol::{
    ChangeKind, CreateNoteParams, DeleteMode, DeleteParams, JobIdParams, NoteSetBoolParams,
    NoteSetBoolResult, Notification, Origin, PathParams, RenameParams, Request, SubscribeParams,
    Topic, UriParams, UriResult, ViewRunParams, WriteAttachmentParams, WriteNoteParams,
    WriteNoteResult, error_codes,
};
use cv_vault::{ContentHash, WatchConfig};
use serde_json::{Value, json};

struct Env {
    tmp: tempfile::TempDir,
    vars: HashMap<String, String>,
}

impl Env {
    /// Synthetic vault `<tmp>/vault/Synth` with two notes.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let vault = tmp.path().join("vault").join("Synth");
        std::fs::create_dir_all(vault.join("notes")).unwrap();
        cv_vault::test_support::assert_temp_root(&vault);
        std::fs::write(vault.join("a.md"), "# A\nsee [[b]]\n").unwrap();
        std::fs::write(vault.join("notes/b.md"), "# B\n").unwrap();
        let r = tmp.path();
        let mut vars = HashMap::new();
        for (k, v) in [
            ("HOME", r.join("home")),
            ("CV_VAULT", vault.clone()),
            ("CV_STATE_DB", r.join("state/state.db")),
            ("CV_INDEX_DB", r.join("cache/index.db")),
            ("CV_CONFIG_DIR", r.join("config")),
            ("CV_SOCKET", r.join("run/d.sock")),
        ] {
            vars.insert(k.to_owned(), v.display().to_string());
        }
        Self { tmp, vars }
    }
    fn set(&mut self, k: &str, v: impl Into<String>) {
        self.vars.insert(k.to_owned(), v.into());
    }
    fn config(&self) -> Config {
        let vars = self.vars.clone();
        Config::resolve(move |k| vars.get(k).cloned()).unwrap()
    }
    fn vault(&self) -> PathBuf {
        PathBuf::from(&self.vars["CV_VAULT"])
    }
    fn socket(&self) -> PathBuf {
        PathBuf::from(&self.vars["CV_SOCKET"])
    }
    /// An executable `/bin/sh` script in the temp dir.
    fn script(&self, name: &str, body: &str) -> PathBuf {
        let p = self.tmp.path().join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }
}

fn opts() -> DaemonOptions {
    DaemonOptions {
        watch: WatchConfig {
            debounce: Duration::from_millis(50),
            echo_ttl: Duration::from_secs(5),
        },
        config_poll: Duration::from_millis(100),
    }
}

async fn start(env: &Env) -> RunningDaemon {
    Daemon::start(env.config(), opts())
        .unwrap()
        .spawn()
        .await
        .unwrap()
}

fn client(env: &Env) -> Client {
    Client::connect(&env.socket(), "test", cv_index::SCHEMA_VERSION).unwrap()
}

fn rpc_code(r: Result<Value, ClientError>) -> i64 {
    match r {
        Err(ClientError::Rpc(e)) => e.code,
        other => panic!("expected an RPC error, got {other:?}"),
    }
}

fn hash_of(p: &Path) -> String {
    ContentHash::of(&std::fs::read(p).unwrap())
        .as_str()
        .to_owned()
}

/// Waits (bounded) for a notification matching `pred`.
fn wait_for(c: &mut Client, pred: impl Fn(&Notification) -> bool) -> Notification {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(n) = c.next_notification(Duration::from_millis(200)).unwrap()
            && pred(&n)
        {
            return n;
        }
    }
    panic!("notification not received");
}

fn poll(mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("condition not reached");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handshake_rules() {
    let env = Env::new();
    let d = start(&env).await;
    let raw = Client::connect_raw(&env.socket()).unwrap();
    let r = raw
        .raw(r#"{"jsonrpc":"2.0","id":1,"method":"status"}"#)
        .unwrap();
    assert_eq!(r["error"]["code"], error_codes::HANDSHAKE_REQUIRED);
    let r = raw
        .raw(r#"{"jsonrpc":"2.0","id":2,"method":"hello","params":{"protocol_version":99,"client":"t"}}"#)
        .unwrap();
    assert_eq!(r["error"]["code"], error_codes::VERSION_MISMATCH);
    let r = raw
        .raw(r#"{"jsonrpc":"2.0","id":3,"method":"status"}"#)
        .unwrap();
    assert_eq!(r["error"]["code"], error_codes::HANDSHAKE_REQUIRED);
    let r = raw.raw("{not json").unwrap();
    assert_eq!(r["error"]["code"], error_codes::PARSE_ERROR);
    assert_eq!(r["id"], Value::Null);
    let r = raw
        .raw(r#"{"jsonrpc":"2.0","id":4,"method":"hello","params":{"protocol_version":1,"client":"t"}}"#)
        .unwrap();
    assert_eq!(r["result"]["index_schema"], cv_index::SCHEMA_VERSION);
    assert!(r["result"].get("mode").is_none());
    let r = raw
        .raw(r#"{"jsonrpc":"2.0","id":5,"method":"frobnicate"}"#)
        .unwrap();
    assert_eq!(r["error"]["code"], error_codes::METHOD_NOT_FOUND);
    assert_eq!(r["id"], 5);
    let r = raw
        .raw(r#"{"jsonrpc":"2.0","id":6,"method":"delete","params":{}}"#)
        .unwrap();
    assert_eq!(r["error"]["code"], error_codes::INVALID_PARAMS);
    // Every known method answers bad params with INVALID_PARAMS, never
    // METHOD_NOT_FOUND — the known-method set is the protocol's own list.
    for (i, method) in cv_protocol::METHOD_NAMES.iter().enumerate() {
        let line = json!({"jsonrpc": "2.0", "id": 100 + i, "method": method, "params": "bogus"});
        let r = raw.raw(&line.to_string()).unwrap();
        assert_eq!(r["error"]["code"], error_codes::INVALID_PARAMS, "{method}");
        assert_eq!(r["id"], 100 + i);
    }

    // Client-side index_schema check: a mismatch is refused, never degraded.
    match Client::connect(&env.socket(), "t", cv_index::SCHEMA_VERSION + 1) {
        Err(ClientError::SchemaMismatch { expected, found }) => {
            assert_eq!(expected, cv_index::SCHEMA_VERSION + 1);
            assert_eq!(found, cv_index::SCHEMA_VERSION);
        }
        other => panic!("expected SchemaMismatch, got {other:?}"),
    }
    let c = client(&env);
    assert_eq!(c.hello.index_db, env.vars["CV_INDEX_DB"]);
    assert!(c.hello.generation >= 1);
    drop((raw, c));
    d.shutdown().await.unwrap();
    assert!(!env.socket().exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn write_note_cas_conflict_does_not_clobber() {
    let env = Env::new();
    let d = start(&env).await;
    let c = client(&env);
    let live_a = env.vault().join("a.md");
    let h0 = hash_of(&live_a);
    // External edit between the client's read and its write.
    std::fs::write(&live_a, "# A\nexternally changed\n").unwrap();
    let h1 = hash_of(&live_a);
    let v = c
        .request(Request::WriteNote(WriteNoteParams {
            path: "a.md".into(),
            content: "client version".into(),
            base_hash: h0.clone(),
        }))
        .unwrap();
    let r: WriteNoteResult = serde_json::from_value(v).unwrap();
    assert_eq!(
        r,
        WriteNoteResult::Conflict {
            current_hash: h1.clone()
        }
    );
    assert_eq!(hash_of(&live_a), h1, "losing write must not land");
    // A correct base hash wins.
    let v = c
        .request(Request::WriteNote(WriteNoteParams {
            path: "a.md".into(),
            content: "client version".into(),
            base_hash: h1.clone(),
        }))
        .unwrap();
    assert!(matches!(
        serde_json::from_value(v).unwrap(),
        WriteNoteResult::Written { .. }
    ));
    assert_eq!(std::fs::read_to_string(&live_a).unwrap(), "client version");
    drop(c);
    d.shutdown().await.unwrap();
}

/// Plan round-8-intake-views-anchor WP-3: `note_set_bool` edits one frontmatter key through the
/// writer, creating key / block as needed; equal values write nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn note_set_bool_edits_frontmatter() {
    let env = Env::new();
    std::fs::write(
        env.vault().join("r.md"),
        "---\nread: false\ntitle: R\n---\nbody\n",
    )
    .unwrap();
    std::fs::write(env.vault().join("k.md"), "---\ntitle: K\n---\nbody\n").unwrap();
    let d = start(&env).await;
    let mut c = client(&env);
    let set = |c: &mut Client, path: &str, value: bool| {
        c.request(Request::NoteSetBool(NoteSetBoolParams {
            path: path.into(),
            key: "read".into(),
            value,
        }))
    };
    let on_disk = |p: &str| std::fs::read_to_string(env.vault().join(p)).unwrap();
    // Existing `false` → `true`; the returned hash is the written file's.
    let r: NoteSetBoolResult = serde_json::from_value(set(&mut c, "r.md", true).unwrap()).unwrap();
    assert_eq!(on_disk("r.md"), "---\nread: true\ntitle: R\n---\nbody\n");
    assert_eq!(r.hash, hash_of(&env.vault().join("r.md")));
    // Missing key → appended `true`.
    set(&mut c, "k.md", true).unwrap();
    assert_eq!(on_disk("k.md"), "---\ntitle: K\nread: true\n---\nbody\n");
    // No frontmatter → a new block.
    set(&mut c, "notes/b.md", true).unwrap();
    assert!(on_disk("notes/b.md").starts_with("---\nread: true\n---\n"));
    assert!(on_disk("notes/b.md").ends_with("# B\n"));
    // Unchanged value → no write, current hash returned.
    let before = std::fs::metadata(env.vault().join("r.md"))
        .unwrap()
        .modified()
        .unwrap();
    let r2: NoteSetBoolResult = serde_json::from_value(set(&mut c, "r.md", true).unwrap()).unwrap();
    assert_eq!(r2.hash, r.hash);
    assert_eq!(
        std::fs::metadata(env.vault().join("r.md"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
    // Missing note → NOT_FOUND.
    assert_eq!(
        rpc_code(set(&mut c, "nope.md", true)),
        error_codes::NOT_FOUND
    );
    drop(c);
    d.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mutations_land_in_the_vault() {
    let env = Env::new();
    let d = start(&env).await;
    let c = client(&env);
    let v = c
        .request(Request::CreateNote(CreateNoteParams {
            path: "a.md".into(),
            content: "dup".into(),
        }))
        .unwrap();
    assert_eq!(v["path"], "a 1.md");
    let h = hash_of(&env.vault().join("a 1.md"));
    c.request(Request::WriteNote(WriteNoteParams {
        path: "a 1.md".into(),
        content: "dup2".into(),
        base_hash: h,
    }))
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(env.vault().join("a 1.md")).unwrap(),
        "dup2"
    );
    c.request(Request::Rename(RenameParams {
        from: "notes/b.md".into(),
        to: "notes/c.md".into(),
    }))
    .unwrap();
    assert!(!env.vault().join("notes/b.md").exists());
    assert!(env.vault().join("notes/c.md").exists());
    c.request(Request::Delete(DeleteParams {
        mode: DeleteMode::Trash,
        path: "a 1.md".into(),
    }))
    .unwrap();
    assert!(!env.vault().join("a 1.md").exists());
    assert!(env.vault().join(".trash/a 1.md").exists());
    assert_eq!(
        rpc_code(c.request(Request::Delete(DeleteParams {
            mode: DeleteMode::Trash,
            path: "nope.md".into()
        }))),
        error_codes::NOT_FOUND
    );
    // plan editor-ui-round-3 WP-3: the preview / delete_note RPCs are wired.
    let pv = c
        .request(Request::DeletePreview(PathParams {
            path: "notes/c.md".into(),
        }))
        .unwrap();
    assert!(pv["items"].is_array(), "{pv}");
    let dn = c
        .request(Request::DeleteNote(cv_protocol::DeleteNoteParams {
            path: "notes/c.md".into(),
            attachments: vec!["missing.png".into()],
            mode: DeleteMode::Permanent,
        }))
        .unwrap();
    assert_eq!(dn["removed"], json!(["notes/c.md"]), "{dn}");
    assert_eq!(dn["skipped"], json!(["missing.png"]), "{dn}");
    assert!(!env.vault().join("notes/c.md").exists());
    drop(c);
    d.shutdown().await.unwrap();
}

/// Plan tidiness-trash-orphans WP-1: a daemon delete lands a `trash_entries` row; a
/// `vault.toml` `location = "system"` reload sends the next delete to the (temp) XDG trash.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trash_moves_reach_the_ledger_and_follow_vault_toml() {
    let mut env = Env::new();
    let data_home = env.tmp.path().join("data");
    env.set("XDG_DATA_HOME", data_home.display().to_string());
    let d = start(&env).await;
    let c = client(&env);
    let rows = || lock_rows(&d);
    c.request(Request::Delete(DeleteParams {
        mode: DeleteMode::Trash,
        path: "a.md".into(),
    }))
    .unwrap();
    poll(|| rows().len() == 1);
    let r = &rows()[0];
    assert_eq!(
        (
            r.location.as_str(),
            r.trash_path.as_str(),
            r.original_path.as_str()
        ),
        ("vault", ".trash/a.md", "a.md")
    );
    assert_eq!(r.reason, "deleted");
    let mut sub = client(&env);
    sub.request(Request::Subscribe(SubscribeParams {
        topics: vec![Topic::ConfigReloaded],
    }))
    .unwrap();
    let cfg = PathBuf::from(&env.vars["CV_CONFIG_DIR"]);
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join("vault.toml"), "[trash]\nlocation = \"system\"\n").unwrap();
    wait_for(&mut sub, |n| {
        matches!(n, Notification::ConfigReloaded { .. })
    });
    c.request(Request::Delete(DeleteParams {
        mode: DeleteMode::Trash,
        path: "notes/b.md".into(),
    }))
    .unwrap();
    poll(|| rows().len() == 2);
    let r = &rows()[1];
    assert_eq!(
        (r.location.as_str(), r.trash_path.as_str()),
        ("system", "b.md")
    );
    assert!(data_home.join("Trash/files/b.md").exists());
    assert!(data_home.join("Trash/info/b.md.trashinfo").exists());
    drop((c, sub));
    d.shutdown().await.unwrap();
}

fn lock_rows(d: &RunningDaemon) -> Vec<cv_state::NewTrashEntry> {
    d.shared()
        .state()
        .lock()
        .unwrap()
        .trash_entries()
        .unwrap()
        .into_iter()
        .map(|e| e.entry)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscriptions_filter_topics() {
    let env = Env::new();
    let d = start(&env).await;
    let mut sub = client(&env);
    sub.request(Request::Subscribe(SubscribeParams {
        topics: vec![Topic::FileChanged, Topic::IndexGeneration],
    }))
    .unwrap();
    let other = client(&env);
    let mut inproc = d.subscribe();

    std::fs::write(env.vault().join("ext.md"), "# external\n").unwrap();
    let n = wait_for(
        &mut sub,
        |n| matches!(n, Notification::FileChanged { path, .. } if path == "ext.md"),
    );
    let Notification::FileChanged {
        kind, hash, origin, ..
    } = n
    else {
        unreachable!()
    };
    assert_eq!(kind, ChangeKind::Created);
    assert_eq!(origin, Origin::External);
    assert_eq!(hash, Some(hash_of(&env.vault().join("ext.md"))));
    // The in-process hook (WP-9's runner) sees it too.
    let got = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(Notification::FileChanged { path, .. }) = inproc.recv().await
                && path == "ext.md"
            {
                return;
            }
        }
    })
    .await;
    assert!(got.is_ok());

    let n = d.shared().rebuild_index().await.unwrap();
    wait_for(&mut sub, |x| *x == Notification::IndexGeneration { n });
    assert!(
        other
            .next_notification(Duration::from_millis(300))
            .unwrap()
            .is_none()
    );
    // The status reflects the new generation.
    let v = other.request(Request::Status).unwrap();
    assert_eq!(v["generation"], n);
    std::fs::remove_file(env.vault().join("ext.md")).unwrap();
    drop((sub, other));
    d.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a1_registry_and_stub_methods() {
    let env = Env::new();
    let d = start(&env).await;
    let mut c = client(&env);
    assert_eq!(
        c.request(Request::ViewsList).unwrap(),
        json!({"view_ids": []})
    );
    assert_eq!(
        rpc_code(c.request(Request::ViewRun(ViewRunParams {
            view_id: "x".into(),
            this_path: None,
            sort: None,
            select: None,
        }))),
        error_codes::NOT_FOUND
    );
    assert_eq!(c.request(Request::ActionsList).unwrap(), json!([]));
    assert_eq!(
        rpc_code(c.request(Request::Job(JobIdParams {
            job_id: "nope".into()
        }))),
        error_codes::NOT_FOUND
    );
    assert_eq!(
        rpc_code(c.request(Request::Cancel(JobIdParams {
            job_id: "nope".into()
        }))),
        error_codes::NOT_FOUND
    );
    let s = c.request(Request::Status).unwrap();
    assert!(s.get("mode").is_none(), "{s}");
    // A second daemon on the same socket refuses to start serving.
    let mut e2 = Env::new();
    e2.set("CV_SOCKET", env.vars["CV_SOCKET"].clone());
    let second = Daemon::start(e2.config(), opts()).unwrap().spawn().await;
    assert!(second.unwrap_err().to_string().contains("already running"));
    // Config dir change → config_reloaded.
    c.request(Request::Subscribe(SubscribeParams {
        topics: vec![Topic::ConfigReloaded],
    }))
    .unwrap();
    std::fs::write(
        Path::new(&env.vars["CV_CONFIG_DIR"]).join("actions.toml"),
        "",
    )
    .unwrap();
    wait_for(&mut c, |n| matches!(n, Notification::ConfigReloaded { .. }));
    drop(c);
    d.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_clients() {
    let env = Env::new();
    let d = start(&env).await;
    let sock = env.socket();
    let hs: Vec<_> = (0..2)
        .map(|i| {
            let sock = sock.clone();
            std::thread::spawn(move || {
                let c = Client::connect(&sock, "t", cv_index::SCHEMA_VERSION).unwrap();
                for _ in 0..5 {
                    c.request(Request::Status).unwrap();
                }
                c.request(Request::CreateNote(CreateNoteParams {
                    path: format!("conc{i}.md"),
                    content: "x".into(),
                }))
                .unwrap()
            })
        })
        .collect();
    let mut both = Vec::new();
    // Hold both connections open at once before either finishes.
    let a = client(&env);
    let b = client(&env);
    for h in hs {
        both.push(h.join().unwrap()["path"].as_str().unwrap().to_owned());
    }
    both.sort();
    assert_eq!(both, vec!["conc0.md", "conc1.md"]);
    drop((a, b));
    d.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uri_intake_over_rpc_and_cli() {
    let mut env = Env::new();
    let clip = env.script("clip.sh", "printf 'clip body \\303\\251'");
    env.set("CV_CLIPBOARD_CMD", clip.display().to_string());
    let out = env.tmp.path().join("opened.txt");
    let open = env.script(
        "open.sh",
        &format!("printf '%s' \"$1\" > '{}'", out.display()),
    );
    env.set("CV_OPEN_CMD", open.display().to_string());
    let d = start(&env).await;
    let cfg = env.config();
    let mut c = client(&env);

    let v = c
        .request(Request::Uri(UriParams {
            url: "obsidian://new?vault=Synth&file=..%2F..%2Fescape&content=hi".into(),
        }))
        .unwrap();
    assert_eq!(
        serde_json::from_value::<UriResult>(v).unwrap(),
        UriResult::Created {
            path: "escape.md".into()
        }
    );
    assert_eq!(
        std::fs::read_to_string(env.vault().join("escape.md")).unwrap(),
        "hi"
    );
    assert_eq!(
        rpc_code(c.request(Request::Uri(UriParams {
            url: "obsidian://new?vault=Other&file=x".into()
        }))),
        error_codes::INVALID_PARAMS
    );
    assert_eq!(
        rpc_code(c.request(Request::Uri(UriParams {
            url: "obsidian://new?file=x&clipboard".into()
        }))),
        error_codes::INVALID_PARAMS
    );
    assert_eq!(
        rpc_code(c.request(Request::Uri(UriParams {
            url: "obsidian://search?q=x".into()
        }))),
        error_codes::INVALID_PARAMS
    );

    // Clipboard resolved in the client via the stub command.
    let r = cv::cli::uri_command(&cfg, "obsidian://new?vault=Synth&file=Clip&clipboard").unwrap();
    assert_eq!(
        r,
        UriResult::Created {
            path: "Clip.md".into()
        }
    );
    assert_eq!(
        std::fs::read_to_string(env.vault().join("Clip.md")).unwrap(),
        "clip body é"
    );
    let r = cv::cli::uri_command(&cfg, "obsidian://new?file=Clip&content=x").unwrap();
    assert_eq!(
        r,
        UriResult::Created {
            path: "Clip 1.md".into()
        }
    );

    // open with no subscriber runs CV_OPEN_CMD with the absolute path as one argument.
    let r = cv::cli::uri_command(&cfg, "obsidian://open?vault=Synth&file=notes%2Fb").unwrap();
    assert_eq!(
        r,
        UriResult::OpenRequested {
            path: "notes/b.md".into()
        }
    );
    poll(|| std::fs::read_to_string(&out).is_ok_and(|s| !s.is_empty()));
    let want = std::fs::canonicalize(env.vault())
        .unwrap()
        .join("notes/b.md");
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        want.display().to_string()
    );

    // With a subscriber, open_requested is broadcast instead.
    std::fs::remove_file(&out).unwrap();
    c.request(Request::Subscribe(SubscribeParams {
        topics: vec![Topic::OpenRequested],
    }))
    .unwrap();
    cv::cli::uri_command(&cfg, "obsidian://open?file=a").unwrap();
    wait_for(&mut c, |n| {
        *n == Notification::OpenRequested {
            path: "a.md".into(),
        }
    });
    assert!(!out.exists());
    drop(c);
    d.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spool_then_drain_and_clipboard_failure() {
    let mut env = Env::new();
    env.set("CV_CLIPBOARD_CMD", "/bin/false");
    let cfg = env.config();
    let spool = cfg.spool_dir();
    // Clipboard failure: explicit error, nothing spooled.
    assert!(cv::cli::uri_command(&cfg, "obsidian://new?file=x&clipboard").is_err());
    let mut env2 = Env::new();
    env2.set("CV_CLIPBOARD_CMD", "/nonexistent/clip-cmd");
    assert!(cv::cli::uri_command(&env2.config(), "obsidian://new?file=x&clipboard").is_err());
    assert!(cv::spool::pending(&spool).unwrap().is_empty());
    assert!(
        cv::spool::pending(&env2.config().spool_dir())
            .unwrap()
            .is_empty()
    );

    // No daemon: spooled, exit-0 semantics.
    let r = cv::cli::uri_command(
        &cfg,
        "obsidian://new?vault=Synth&file=Spooled%20Clip&content=%C3%A9",
    )
    .unwrap();
    assert_eq!(r, UriResult::Spooled);
    let r = cv::cli::uri_command(&cfg, "obsidian://new?vault=Wrong&file=y").unwrap();
    assert_eq!(r, UriResult::Spooled);
    assert_eq!(cv::spool::pending(&spool).unwrap().len(), 2);

    let d = start(&env).await;
    assert_eq!(
        std::fs::read_to_string(env.vault().join("Spooled Clip.md")).unwrap(),
        "é"
    );
    assert!(cv::spool::pending(&spool).unwrap().is_empty());
    let failed: Vec<_> = std::fs::read_dir(spool.join("failed")).unwrap().collect();
    assert_eq!(
        failed.len(),
        1,
        "the refused URL is quarantined, not deleted"
    );
    d.shutdown().await.unwrap();
}

#[test]
fn binary_cli_contract() {
    let mut env = Env::new();
    let missing = env.tmp.path().join("missing-vault").display().to_string();
    env.set("CV_VAULT", missing);
    let bin = env!("CARGO_BIN_EXE_cv");
    let run = |args: &[&str]| {
        let mut cmd = std::process::Command::new(bin);
        cmd.args(args).env_clear();
        for (k, v) in &env.vars {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    };
    let o = run(&["daemon"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("opening the vault"));
    assert_eq!(run(&["bogus"]).status.code(), Some(2));
    assert_eq!(run(&["--help"]).status.code(), Some(0));
    // Status with no daemon falls back to state.db (created by the failed start above).
    let o = run(&["status"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(String::from_utf8_lossy(&o.stdout).contains("daemon: not running"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn write_attachment_is_content_addressed_and_idempotent() {
    use base64::Engine as _;
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let env = Env::new();
    let d = start(&env).await;
    let c = client(&env);
    let bytes = b"\x89PNG fake image bytes";
    let req = |ext: &str, data: String| {
        Request::WriteAttachment(WriteAttachmentParams {
            note_path: "notes/b.md".into(),
            ext: ext.into(),
            data_base64: data,
        })
    };
    let v = c.request(req("png", b64(bytes))).unwrap();
    let path = v["path"].as_str().unwrap().to_owned();
    use md5::Digest as _;
    let md5 = hex::encode(md5::Md5::digest(bytes));
    assert_eq!(path, format!("notes/_attachments/b/{md5}_MD5.png"));
    let on_disk = env.vault().join(&path);
    assert_eq!(std::fs::read(&on_disk).unwrap(), bytes);
    // Tamper with the stored file: a second identical call must reuse it, not write again.
    std::fs::write(&on_disk, b"marker").unwrap();
    let v2 = c.request(req("png", b64(bytes))).unwrap();
    assert_eq!(v2["path"].as_str().unwrap(), path);
    assert_eq!(std::fs::read(&on_disk).unwrap(), b"marker");
    // Refusals.
    assert_eq!(
        rpc_code(c.request(req("png", "!!not base64!!".into()))),
        error_codes::INVALID_PARAMS
    );
    for bad in ["", "PNG", "p.g", "toolongext"] {
        assert_eq!(
            rpc_code(c.request(req(bad, b64(bytes)))),
            error_codes::INVALID_PARAMS
        );
    }
    let big = vec![0u8; 25 * 1024 * 1024 + 1];
    assert_eq!(
        rpc_code(c.request(req("png", b64(&big)))),
        error_codes::INVALID_PARAMS
    );
    drop(c);
    d.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gitignore_change_rebuilds_index() {
    let env = Env::new();
    std::fs::create_dir_all(env.vault().join("drop")).unwrap();
    std::fs::write(env.vault().join("drop/x.md"), "# x\n").unwrap();
    let d = start(&env).await;
    let mut sub = client(&env);
    sub.request(Request::Subscribe(SubscribeParams {
        topics: vec![Topic::IndexGeneration],
    }))
    .unwrap();
    let base = PathBuf::from(&env.vars["CV_INDEX_DB"]);
    let has = |n: u64, p: &str| {
        cv_index::Reader::open(&base, n)
            .unwrap()
            .query()
            .file(p)
            .unwrap()
            .is_some()
    };
    std::fs::write(env.vault().join(".gitignore"), "drop/\n").unwrap();
    let Notification::IndexGeneration { n } = wait_for(&mut sub, |x| {
        matches!(x, Notification::IndexGeneration { .. })
    }) else {
        unreachable!()
    };
    assert!(!has(n, "drop/x.md") && has(n, "a.md"));
    std::fs::remove_file(env.vault().join(".gitignore")).unwrap();
    let Notification::IndexGeneration { n } = wait_for(
        &mut sub,
        |x| matches!(x, Notification::IndexGeneration { n: m } if *m > n),
    ) else {
        unreachable!()
    };
    assert!(has(n, "drop/x.md"));
    drop(sub);
    d.shutdown().await.unwrap();
}

/// plan editor-ui-round-3 WP-2: incremental note changes (daemon writes and external edits)
/// invalidate views, coalesced into one `views_invalidated` per short window.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn incremental_changes_invalidate_views_coalesced() {
    let env = Env::new();
    let views = PathBuf::from(&env.vars["CV_CONFIG_DIR"]).join("views");
    std::fs::create_dir_all(&views).unwrap();
    std::fs::write(
        views.join("t.toml"),
        "[[views]]\ntype='tasks'\nname='All'\n",
    )
    .unwrap();
    let d = start(&env).await;
    let c = client(&env);
    let mut sub = client(&env);
    sub.request(Request::Subscribe(SubscribeParams {
        topics: vec![Topic::ViewsInvalidated],
    }))
    .unwrap();

    // Two daemon-origin writes back to back → one coalesced invalidation of every view.
    let mut base = hash_of(&env.vault().join("a.md"));
    for body in ["# A\n- [ ] one\n", "# A\n- [ ] one\n- [ ] two\n"] {
        let v = c
            .request(Request::WriteNote(WriteNoteParams {
                path: "a.md".into(),
                content: body.into(),
                base_hash: base.clone(),
            }))
            .unwrap();
        assert!(matches!(
            serde_json::from_value(v).unwrap(),
            WriteNoteResult::Written { .. }
        ));
        base = ContentHash::of(body.as_bytes()).as_str().to_owned();
    }
    let Notification::ViewsInvalidated { view_ids } = wait_for(&mut sub, |n| {
        matches!(n, Notification::ViewsInvalidated { .. })
    }) else {
        unreachable!()
    };
    assert_eq!(view_ids, vec!["t#All".to_owned()]);
    // Under load the watcher's late echo of the writes can land after the 150 ms window and
    // open one more batch; that is at most one extra re-run, never one per event.
    let mut extra = 0;
    while let Some(n) = sub.next_notification(Duration::from_millis(600)).unwrap() {
        assert!(matches!(n, Notification::ViewsInvalidated { .. }), "{n:?}");
        extra += 1;
    }
    assert!(extra <= 1, "coalesced: got {} invalidations", extra + 1);

    // An external edit invalidates too.
    std::fs::write(env.vault().join("notes/b.md"), "# B\n- [ ] ext\n").unwrap();
    wait_for(&mut sub, |n| {
        matches!(n, Notification::ViewsInvalidated { .. })
    });
    drop((c, sub));
    d.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lookup_models_round_trip_falls_back_to_declared() {
    let env = Env::new();
    let cfg = PathBuf::from(&env.vars["CV_CONFIG_DIR"]);
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("providers.toml"),
        "[[provider]]\nid = \"local\"\nkind = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:1/v1\"\nmodels = [{ id = \"gemma-4\" }]\n",
    )
    .unwrap();
    let d = start(&env).await;
    let c = client(&env);
    let v = c
        .request(Request::LookupModels(cv_protocol::LookupModelsParams {
            provider: "local".into(),
        }))
        .unwrap();
    assert_eq!(v, json!({"models": [{"id": "gemma-4"}], "live": false}));
    assert_eq!(
        rpc_code(
            c.request(Request::LookupModels(cv_protocol::LookupModelsParams {
                provider: "missing".into(),
            }))
        ),
        error_codes::NOT_FOUND
    );
    assert_eq!(
        c.request(Request::LookupGeocode(cv_protocol::LookupGeocodeParams {
            query: " ".into(),
        }))
        .unwrap(),
        json!([])
    );
    drop(c);
    d.shutdown().await.unwrap();
}

/// Plan memory-instrumentation-and-reduction WP-M1: `status.memory` carries the daemon snapshot
/// and the latest `memory_report`, and `cv status` renders the memory block.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn memory_report_reaches_status() {
    use cv_protocol::{GaugeValue, MemoryReport, methods as m};
    let env = Env::new();
    let d = start(&env).await;
    let c = client(&env);
    let s = c.call::<m::Status>(()).unwrap();
    let mem = s.memory.expect("memory");
    assert!(mem.daemon.rss.unwrap_or(0) > 0);
    assert!(mem.editor.is_none());
    let names: Vec<&str> = mem.daemon.gauges.iter().map(|g| g.name.as_str()).collect();
    assert!(names.contains(&"resolver_entries"), "{names:?}");
    c.call::<m::MemoryReport>(MemoryReport {
        uptime_secs: 7,
        rss: Some(64 << 20),
        gauges: vec![GaugeValue {
            name: "image_cache_bytes".into(),
            value: Some(1 << 20),
            error: None,
        }],
        ..MemoryReport::default()
    })
    .unwrap();
    let e = c
        .call::<m::Status>(())
        .unwrap()
        .memory
        .expect("memory")
        .editor
        .expect("editor");
    assert_eq!(e.report.uptime_secs, 7);
    let out = cv::cli::status_command(&env.config()).unwrap();
    println!("{out}");
    assert!(out.contains("memory:\n  daemon: rss+swap "), "{out}");
    assert!(out.contains("  editor: rss+swap 64.0 MiB"), "{out}");
    drop(c);
    d.shutdown().await.unwrap();
}
