//! In-process daemon tests on a synthetic temp vault (shadow mode, temp `CV_PROTECTED_VAULT`).
//! Never touches the live vault (I1), the real clipboard, or any scheme handler.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use cv::{Client, ClientError, Config, Daemon, DaemonOptions, RunningDaemon};
use cv_protocol::{
    ChangeKind, CreateNoteParams, DeleteMode, DeleteParams, JobIdParams, Notification, Origin,
    PathParams, RenameParams, Request, SubscribeParams, Topic, UriParams, UriResult, ViewRunParams,
    WriteAttachmentParams, WriteNoteParams, WriteNoteResult, WriteOp, error_codes,
};
use cv_vault::{ContentHash, WatchConfig};
use serde_json::{Value, json};

static N: AtomicU32 = AtomicU32::new(0);

/// A temp dir removed on drop.
struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "cv-it-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Env {
    tmp: Tmp,
    vars: HashMap<String, String>,
}

impl Env {
    /// Synthetic vault `<tmp>/prot/Synth` (protected root `<tmp>/prot`) with two notes.
    fn new() -> Self {
        let tmp = Tmp::new();
        let vault = tmp.0.join("prot").join("Synth");
        std::fs::create_dir_all(vault.join("notes")).unwrap();
        std::fs::write(vault.join("a.md"), "# A\nsee [[b]]\n").unwrap();
        std::fs::write(vault.join("notes/b.md"), "# B\n").unwrap();
        let r = &tmp.0;
        let mut vars = HashMap::new();
        for (k, v) in [
            ("HOME", r.join("home")),
            ("CV_VAULT", vault.clone()),
            ("CV_PROTECTED_VAULT", r.join("prot")),
            ("CV_STATE_DB", r.join("state/state.db")),
            ("CV_INDEX_DB", r.join("cache/index.db")),
            ("CV_SCRATCH", r.join("scratch")),
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
    fn overlay(&self) -> PathBuf {
        PathBuf::from(&self.vars["CV_SCRATCH"]).join("vault")
    }
    fn socket(&self) -> PathBuf {
        PathBuf::from(&self.vars["CV_SOCKET"])
    }
    /// An executable `/bin/sh` script in the temp dir.
    fn script(&self, name: &str, body: &str) -> PathBuf {
        let p = self.tmp.0.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }
}

fn opts() -> DaemonOptions {
    DaemonOptions {
        landlock: false,
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

fn snapshot(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push((p.display().to_string(), hash_of(&p)));
            }
        }
    }
    out.sort();
    out
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
    let mut raw = Client::connect_raw(&env.socket()).unwrap();
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
    assert_eq!(r["result"]["mode"], "shadow");
    let r = raw
        .raw(r#"{"jsonrpc":"2.0","id":5,"method":"frobnicate"}"#)
        .unwrap();
    assert_eq!(r["error"]["code"], error_codes::METHOD_NOT_FOUND);
    assert_eq!(r["id"], 5);
    let r = raw
        .raw(r#"{"jsonrpc":"2.0","id":6,"method":"delete","params":{}}"#)
        .unwrap();
    assert_eq!(r["error"]["code"], error_codes::INVALID_PARAMS);

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
    let mut c = client(&env);
    let live_a = env.vault().join("a.md");
    let h0 = hash_of(&live_a);
    // External edit between the client's read and its write.
    std::fs::write(&live_a, "# A\nexternally changed\n").unwrap();
    let h1 = hash_of(&live_a);
    let v = c
        .call(Request::WriteNote(WriteNoteParams {
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
    assert!(
        !env.overlay().join("a.md").exists(),
        "losing write must not land"
    );
    assert_eq!(hash_of(&live_a), h1);
    // A correct base hash wins, in the overlay only.
    let v = c
        .call(Request::WriteNote(WriteNoteParams {
            path: "a.md".into(),
            content: "client version".into(),
            base_hash: h1.clone(),
        }))
        .unwrap();
    assert!(matches!(
        serde_json::from_value(v).unwrap(),
        WriteNoteResult::Written { .. }
    ));
    assert_eq!(
        std::fs::read_to_string(env.overlay().join("a.md")).unwrap(),
        "client version"
    );
    assert_eq!(hash_of(&live_a), h1);
    drop(c);
    d.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shadow_mutations_never_touch_live() {
    let env = Env::new();
    let before = snapshot(&env.vault());
    let d = start(&env).await;
    let mut sub = client(&env);
    sub.call(Request::Subscribe(SubscribeParams {
        topics: vec![Topic::ShadowWrite],
    }))
    .unwrap();
    let mut c = client(&env);
    let v = c
        .call(Request::CreateNote(CreateNoteParams {
            path: "a.md".into(),
            content: "dup".into(),
        }))
        .unwrap();
    assert_eq!(v["path"], "a 1.md");
    wait_for(
        &mut sub,
        |n| matches!(n, Notification::ShadowWrite { op: WriteOp::Create, path } if path == "a 1.md"),
    );
    let h = hash_of(&env.overlay().join("a 1.md"));
    c.call(Request::WriteNote(WriteNoteParams {
        path: "a 1.md".into(),
        content: "dup2".into(),
        base_hash: h,
    }))
    .unwrap();
    wait_for(&mut sub, |n| {
        matches!(
            n,
            Notification::ShadowWrite {
                op: WriteOp::Write,
                ..
            }
        )
    });
    c.call(Request::Rename(RenameParams {
        from: "notes/b.md".into(),
        to: "notes/c.md".into(),
    }))
    .unwrap();
    wait_for(&mut sub, |n| {
        matches!(
            n,
            Notification::ShadowWrite {
                op: WriteOp::Rename,
                ..
            }
        )
    });
    c.call(Request::Delete(DeleteParams {
        mode: DeleteMode::Trash,
        path: "a 1.md".into(),
    }))
    .unwrap();
    wait_for(&mut sub, |n| {
        matches!(
            n,
            Notification::ShadowWrite {
                op: WriteOp::Delete,
                ..
            }
        )
    });
    assert_eq!(
        rpc_code(c.call(Request::Delete(DeleteParams {
            mode: DeleteMode::Trash,
            path: "nope.md".into()
        }))),
        error_codes::NOT_FOUND
    );

    assert_eq!(snapshot(&env.vault()), before, "live vault changed");
    assert!(env.overlay().join("notes/c.md").exists());
    let r = cv_state::StateReader::open(Path::new(&env.vars["CV_STATE_DB"])).unwrap();
    let ops: Vec<String> = r
        .shadow_writes(0, 100)
        .unwrap()
        .into_iter()
        .map(|w| w.op)
        .collect();
    for op in ["create", "modify", "rename_to", "rename_from", "delete"] {
        assert!(ops.iter().any(|o| o == op), "{op} missing from {ops:?}");
    }
    // plan editor-ui-round-3 WP-3: the preview / delete_note RPCs are wired (shadow-safe).
    let pv = c
        .call(Request::DeletePreview(PathParams {
            path: "notes/c.md".into(),
        }))
        .unwrap();
    assert!(pv["items"].is_array(), "{pv}");
    let dn = c
        .call(Request::DeleteNote(cv_protocol::DeleteNoteParams {
            path: "notes/c.md".into(),
            attachments: vec!["missing.png".into()],
            mode: DeleteMode::Permanent,
        }))
        .unwrap();
    assert_eq!(dn["removed"], json!(["notes/c.md"]), "{dn}");
    assert_eq!(dn["skipped"], json!(["missing.png"]), "{dn}");
    assert_eq!(snapshot(&env.vault()), before, "live vault changed");
    drop((c, sub));
    d.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscriptions_filter_topics() {
    let env = Env::new();
    let d = start(&env).await;
    let mut sub = client(&env);
    sub.call(Request::Subscribe(SubscribeParams {
        topics: vec![Topic::FileChanged, Topic::IndexGeneration],
    }))
    .unwrap();
    let mut other = client(&env);
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
    let v = other.call(Request::Status).unwrap();
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
    assert_eq!(c.call(Request::ViewsList).unwrap(), json!({"view_ids": []}));
    assert_eq!(
        rpc_code(c.call(Request::ViewRun(ViewRunParams {
            view_id: "x".into(),
            this_path: None
        }))),
        error_codes::NOT_FOUND
    );
    assert_eq!(c.call(Request::ActionsList).unwrap(), json!([]));
    assert_eq!(
        rpc_code(c.call(Request::Job(JobIdParams {
            job_id: "nope".into()
        }))),
        error_codes::NOT_FOUND
    );
    assert_eq!(
        rpc_code(c.call(Request::Cancel(JobIdParams {
            job_id: "nope".into()
        }))),
        error_codes::NOT_FOUND
    );
    let s = c.call(Request::Status).unwrap();
    assert_eq!(s["mode"], "shadow");
    // A second daemon on the same socket refuses to start serving.
    let mut e2 = Env::new();
    e2.set("CV_SOCKET", env.vars["CV_SOCKET"].clone());
    let second = Daemon::start(e2.config(), opts()).unwrap().spawn().await;
    assert!(second.unwrap_err().to_string().contains("already running"));
    // Config dir change → config_reloaded.
    c.call(Request::Subscribe(SubscribeParams {
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
                let mut c = Client::connect(&sock, "t", cv_index::SCHEMA_VERSION).unwrap();
                for _ in 0..5 {
                    c.call(Request::Status).unwrap();
                }
                c.call(Request::CreateNote(CreateNoteParams {
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
    let out = env.tmp.0.join("opened.txt");
    let open = env.script(
        "open.sh",
        &format!("printf '%s' \"$1\" > '{}'", out.display()),
    );
    env.set("CV_OPEN_CMD", open.display().to_string());
    let d = start(&env).await;
    let cfg = env.config();
    let mut c = client(&env);

    let v = c
        .call(Request::Uri(UriParams {
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
        std::fs::read_to_string(env.overlay().join("escape.md")).unwrap(),
        "hi"
    );
    assert_eq!(
        rpc_code(c.call(Request::Uri(UriParams {
            url: "obsidian://new?vault=Other&file=x".into()
        }))),
        error_codes::INVALID_PARAMS
    );
    assert_eq!(
        rpc_code(c.call(Request::Uri(UriParams {
            url: "obsidian://new?file=x&clipboard".into()
        }))),
        error_codes::INVALID_PARAMS
    );
    assert_eq!(
        rpc_code(c.call(Request::Uri(UriParams {
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
        std::fs::read_to_string(env.overlay().join("Clip.md")).unwrap(),
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
    c.call(Request::Subscribe(SubscribeParams {
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
        std::fs::read_to_string(env.overlay().join("Spooled Clip.md")).unwrap(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_mode_refused_near_protected_root() {
    for which in ["equal", "under", "above"] {
        let mut env = Env::new();
        let prot = PathBuf::from(&env.vars["CV_PROTECTED_VAULT"]);
        let vault = match which {
            "equal" => prot.clone(),
            "under" => env.vault(),
            _ => env.tmp.0.clone(),
        };
        env.set("CV_MODE", "live");
        env.set("CV_VAULT", vault.display().to_string());
        let err = Daemon::start(env.config(), opts()).unwrap_err();
        assert!(
            format!("{err:#}").contains("live mode refused"),
            "{which}: {err:#}"
        );
        assert!(!env.socket().exists());
    }
}

#[test]
fn binary_cli_contract() {
    let env = {
        let mut e = Env::new();
        e.set("CV_MODE", "live");
        e
    };
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
    assert!(String::from_utf8_lossy(&o.stderr).contains("live mode refused"));
    assert_eq!(run(&["bogus"]).status.code(), Some(2));
    assert_eq!(run(&["shadow", "bogus"]).status.code(), Some(2));
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

// ---- cut-over marker (plan cut-over-now WP-1) -------------------------------

fn state_db(env: &Env) -> cv_state::StateDb {
    let p = PathBuf::from(&env.vars["CV_STATE_DB"]);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    cv_state::StateDb::open(&p, 1, cv_state::QueueConfig::default()).unwrap()
}

fn prot(env: &Env) -> String {
    env.vars["CV_PROTECTED_VAULT"].clone()
}

fn mark_args(rest: &[&str]) -> Vec<String> {
    rest.iter().map(|s| (*s).to_owned()).collect()
}

fn seed_job(db: &mut cv_state::StateDb, key: &str) -> String {
    let job = cv_state::NewJob {
        job_type: "no-such-type".into(),
        lane: cv_state::Lane::Background,
        dedupe_key: Some(key.into()),
        params: json!({}),
        resource_keys: vec![],
        service: None,
    };
    match db.enqueue(job, 1).unwrap() {
        cv_state::EnqueueOutcome::Enqueued(id) => id,
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_mode_starts_with_marker() {
    let mut env = Env::new();
    env.set("CV_MODE", "live");
    {
        let mut db = state_db(&env);
        let canon = std::fs::canonicalize(prot(&env)).unwrap();
        db.set_cutover(&canon.display().to_string(), "test", 1)
            .unwrap();
    }
    let d = start(&env).await;
    let st = client(&env).call(Request::Status).unwrap();
    assert_eq!(st["mode"], "live", "{st}");
    d.shutdown().await.unwrap();
}

#[test]
fn cutover_mark_refusals() {
    let env = Env::new();
    let c = env.config();
    let p = prot(&env);
    // No --yes.
    let e = cv::cutover::mark(&c, &mark_args(&["--vault", &p])).unwrap_err();
    assert!(format!("{e:#}").contains("refusing without --yes"), "{e:#}");
    // Vault mismatch.
    let e = cv::cutover::mark(&c, &mark_args(&["--vault", &env.vars["CV_VAULT"], "--yes"]))
        .unwrap_err();
    assert!(
        format!("{e:#}").contains("not the protected vault"),
        "{e:#}"
    );
    // Unfinished job without --cancel-unfinished.
    let id = seed_job(&mut state_db(&env), "u");
    let e = cv::cutover::mark(&c, &mark_args(&["--vault", &p, "--yes"])).unwrap_err();
    let msg = format!("{e:#}");
    assert!(msg.contains("unfinished") && msg.contains(&id), "{msg}");
    assert!(msg.contains("no-such-type"), "{msg}");
    // Listener that accepts and drops every stream, never handshaking (audit E1).
    std::fs::create_dir_all(env.socket().parent().unwrap()).unwrap();
    let l = std::os::unix::net::UnixListener::bind(env.socket()).unwrap();
    let t = std::thread::spawn(move || {
        if let Ok((s, _)) = l.accept() {
            drop(s);
        }
    });
    let e = cv::cutover::mark(
        &c,
        &mark_args(&["--vault", &p, "--yes", "--cancel-unfinished"]),
    )
    .unwrap_err();
    assert!(format!("{e:#}").contains("accepting connections"), "{e:#}");
    t.join().unwrap();
    let db = state_db(&env);
    assert_eq!(db.cutover().unwrap(), None, "no refusal marks");
    assert_eq!(db.unfinished_jobs().unwrap().len(), 1, "no refusal cancels");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cutover_cancel_unfinished_then_live_runs_nothing() {
    let mut env = Env::new();
    env.set("CV_MODE", "live");
    let (queued, deferred, running) = {
        let mut db = state_db(&env);
        let deferred = seed_job(&mut db, "d");
        assert_eq!(db.claim(2).unwrap().unwrap().id, deferred);
        db.settle(
            &deferred,
            &cv_state::JobResult::Deferred {
                retry_after_ms: 1_000_000_000_000,
                service_unhealthy: None,
            },
            3,
        )
        .unwrap();
        let running = seed_job(&mut db, "r");
        assert_eq!(db.claim(4).unwrap().unwrap().id, running);
        db.put_checkpoint(&running, 0, "{}", 4).unwrap();
        let queued = seed_job(&mut db, "q");
        (queued, deferred, running)
    };
    let p = prot(&env);
    let out = cv::cutover::mark(
        &env.config(),
        &mark_args(&["--vault", &p, "--yes", "--cancel-unfinished"]),
    )
    .unwrap();
    for id in [&queued, &deferred, &running] {
        assert!(out.contains(&format!("cancelled {id}")), "{out}");
    }
    assert!(out.contains("marker set"), "{out}");
    let d = start(&env).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    d.shutdown().await.unwrap();
    let r = cv_state::StateReader::open(Path::new(&env.vars["CV_STATE_DB"])).unwrap();
    for id in [&queued, &deferred, &running] {
        let j = r.job(id).unwrap().expect("row kept");
        assert_eq!(j.status, cv_state::JobStatus::Cancelled, "{id}");
    }
}

#[test]
fn binary_cutover_contract() {
    let env = Env::new();
    let bin = env!("CARGO_BIN_EXE_cv");
    let run = |args: &[&str]| {
        let mut cmd = std::process::Command::new(bin);
        cmd.args(args).env_clear();
        for (k, v) in &env.vars {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    };
    let db = PathBuf::from(&env.vars["CV_STATE_DB"]);
    let o = run(&["cutover", "status"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stdout).contains("no state.db"));
    assert!(!db.exists(), "status never creates state.db");
    let p = prot(&env);
    let o = run(&["cutover", "mark", "--vault", &p]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("--yes"));
    assert!(!db.exists());
    let o = run(&["cutover", "mark", "--vault", &p, "--yes"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(String::from_utf8_lossy(&o.stdout).contains("marker set"));
    let o = run(&["cutover", "mark", "--vault", &p, "--yes"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stdout).contains("marker already set"));
    let o = run(&["cutover", "status"]);
    let s = String::from_utf8_lossy(&o.stdout);
    assert!(
        s.contains("marker: present") && s.contains("queued 0, running 0"),
        "{s}"
    );
    assert_eq!(run(&["cutover", "bogus"]).status.code(), Some(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn write_attachment_is_content_addressed_and_idempotent() {
    use base64::Engine as _;
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let env = Env::new();
    let d = start(&env).await;
    let mut c = client(&env);
    let bytes = b"\x89PNG fake image bytes";
    let req = |ext: &str, data: String| {
        Request::WriteAttachment(WriteAttachmentParams {
            note_path: "notes/b.md".into(),
            ext: ext.into(),
            data_base64: data,
        })
    };
    let v = c.call(req("png", b64(bytes))).unwrap();
    let path = v["path"].as_str().unwrap().to_owned();
    use md5::Digest as _;
    let md5 = hex::encode(md5::Md5::digest(bytes));
    assert_eq!(path, format!("notes/_attachments/b/{md5}_MD5.png"));
    // Shadow mode: the write lands in the overlay, never live.
    let on_disk = env.overlay().join(&path);
    assert_eq!(std::fs::read(&on_disk).unwrap(), bytes);
    assert!(!env.vault().join(&path).exists());
    // Tamper with the stored file: a second identical call must reuse it, not write again.
    std::fs::write(&on_disk, b"marker").unwrap();
    let v2 = c.call(req("png", b64(bytes))).unwrap();
    assert_eq!(v2["path"].as_str().unwrap(), path);
    assert_eq!(std::fs::read(&on_disk).unwrap(), b"marker");
    // Refusals.
    assert_eq!(
        rpc_code(c.call(req("png", "!!not base64!!".into()))),
        error_codes::INVALID_PARAMS
    );
    for bad in ["", "PNG", "p.g", "toolongext"] {
        assert_eq!(
            rpc_code(c.call(req(bad, b64(bytes)))),
            error_codes::INVALID_PARAMS
        );
    }
    let big = vec![0u8; 25 * 1024 * 1024 + 1];
    assert_eq!(
        rpc_code(c.call(req("png", b64(&big)))),
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
    sub.call(Request::Subscribe(SubscribeParams {
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
    let mut c = client(&env);
    let mut sub = client(&env);
    sub.call(Request::Subscribe(SubscribeParams {
        topics: vec![Topic::ViewsInvalidated],
    }))
    .unwrap();

    // Two daemon-origin writes back to back → one coalesced invalidation of every view.
    let mut base = hash_of(&env.vault().join("a.md"));
    for body in ["# A\n- [ ] one\n", "# A\n- [ ] one\n- [ ] two\n"] {
        let v = c
            .call(Request::WriteNote(WriteNoteParams {
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
    let mut c = client(&env);
    let v = c
        .call(Request::LookupModels(cv_protocol::LookupModelsParams {
            provider: "local".into(),
        }))
        .unwrap();
    assert_eq!(v, json!({"models": [{"id": "gemma-4"}], "live": false}));
    assert_eq!(
        rpc_code(
            c.call(Request::LookupModels(cv_protocol::LookupModelsParams {
                provider: "missing".into(),
            }))
        ),
        error_codes::NOT_FOUND
    );
    assert_eq!(
        c.call(Request::LookupGeocode(cv_protocol::LookupGeocodeParams {
            query: " ".into(),
        }))
        .unwrap(),
        json!([])
    );
    drop(c);
    d.shutdown().await.unwrap();
}
