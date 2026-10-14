//! In-process daemon tests on a synthetic temp vault (shadow mode, temp `CV_PROTECTED_VAULT`).
//! Never touches the live vault (I1), the real clipboard, or any scheme handler.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use cv::{Client, ClientError, Config, Daemon, DaemonOptions, RunningDaemon};
use cv_protocol::{
    ChangeKind, CreateNoteParams, JobIdParams, Notification, Origin, PathParams, RenameParams,
    Request, SubscribeParams, Topic, UriParams, UriResult, ViewRunParams, WriteNoteParams,
    WriteNoteResult, WriteOp, error_codes,
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
    c.call(Request::Delete(PathParams {
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
        rpc_code(c.call(Request::Delete(PathParams {
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
