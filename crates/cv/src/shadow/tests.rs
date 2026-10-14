//! WP-14 tests: synthetic vaults under temp dirs, a hand-rolled fake companion on 127.0.0.1 that
//! records every request, and an injected fake system probe (I1 — never the live vault, the real
//! companion at :4801 or systemd).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use cv_actions::{ConfigSet, Step};
use cv_protocol::{ActionContext, Mode};
use cv_providers::{ProviderManager, SecretSource};
use cv_state::{
    EnqueueOutcome, ImageDescription, Lane, NewJob, NewShadowWrite, QueueConfig, StateDb,
    StateReader,
};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::parity::{self, CYCLE_GUARD_LINE, ParityOptions};
use super::probe::SystemProbe;
use super::replay::{self, ReplayRequest};
use super::{import, report};
use crate::config::Config;
use crate::steps::{ChildSpawner, StepCtx};

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "cv-wp14-{}-{}",
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

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn age(root: &Path, rel: &str, days: u64) {
    let f = std::fs::File::options()
        .write(true)
        .open(root.join(rel))
        .unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(days * 86_400))
        .unwrap();
}

fn config(root: &Path, mode: Mode) -> Config {
    Config {
        vault: root.join("vault"),
        mode,
        protected_vault: root.join("prot"),
        state_db: root.join("state/state.db"),
        index_db: root.join("cache/index.db"),
        scratch: root.join("scratch"),
        config_dir: root.join("config"),
        socket: root.join("run/d.sock"),
        open_cmd: None,
        clipboard_cmd: vec!["false".into()],
        link_write_roots: Vec::new(),
    }
}

// ---- fake companion -------------------------------------------------------------------------

#[derive(Default)]
struct FakeState {
    store: BTreeMap<String, String>,
    log: Vec<String>,
}

#[derive(Clone)]
struct Fake {
    base: String,
    st: Arc<Mutex<FakeState>>,
}

impl Fake {
    fn log(&self) -> Vec<String> {
        self.st.lock().unwrap().log.clone()
    }
    fn set(&self, path: &str, hash: &str) {
        self.st
            .lock()
            .unwrap()
            .store
            .insert(path.into(), hash.into());
    }
}

fn handle(st: &mut FakeState, path: &str, body: &Value) -> (u16, Value) {
    match path {
        "/health" => (200, json!({"ok": true, "schemaVersion": 7})),
        "/v1/paths" => {
            let paths: Vec<Value> = st
                .store
                .iter()
                .map(|(p, h)| json!({"path": p, "contentHash": h, "chunkCount": 1, "embeddedCount": 0}))
                .collect();
            (200, json!({"ok": true, "paths": paths}))
        }
        "/v1/files/state" => (200, json!({"ok": true, "files": []})),
        "/v1/chunks/upsert" => {
            for c in body["chunks"].as_array().cloned().unwrap_or_default() {
                let p = c["path"].as_str().unwrap_or_default().to_owned();
                let h = c["contentHash"].as_str().unwrap_or_default().to_owned();
                st.store.insert(p, h);
            }
            (200, json!({"ok": true}))
        }
        "/v1/chunks/delete" => (200, json!({"ok": true})),
        _ => (404, json!({})),
    }
}

async fn serve(l: TcpListener, st: Arc<Mutex<FakeState>>) {
    loop {
        let Ok((mut s, _)) = l.accept().await else {
            return;
        };
        let st = st.clone();
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let head_end = loop {
                let Ok(n) = s.read(&mut chunk).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
            let path = head.split(' ').nth(1).unwrap_or("").to_owned();
            let len: usize = head
                .lines()
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.trim().parse().ok())
                .unwrap_or(0);
            while buf.len() < head_end + len {
                let Ok(n) = s.read(&mut chunk).await else {
                    return;
                };
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let body: Value = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
            let (status, out) = {
                let mut g = st.lock().unwrap();
                g.log.push(path.clone());
                handle(&mut g, &path, &body)
            };
            let out = out.to_string();
            let resp = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}",
                out.len()
            );
            let _ = s.write_all(resp.as_bytes()).await;
            let _ = s.shutdown().await;
        });
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// A fake companion; `up = false` binds and drops the port (connection refused).
fn fake(rt: &tokio::runtime::Runtime, up: bool) -> Fake {
    let st = Arc::new(Mutex::new(FakeState::default()));
    let base = rt.block_on(async {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        if up {
            tokio::spawn(serve(l, st.clone()));
        }
        format!("http://{addr}")
    });
    Fake { base, st }
}

// ---- fake probe -----------------------------------------------------------------------------

struct FakeProbe {
    restarts: Option<u64>,
    active_since: Option<i64>,
    journal: Option<Vec<String>>,
}

impl SystemProbe for FakeProbe {
    fn main_pid(&self) -> Option<u32> {
        None
    }
    fn n_restarts(&self) -> Option<u64> {
        self.restarts
    }
    fn active_since_ms(&self) -> Option<i64> {
        self.active_since
    }
    fn journal_lines(&self, _since_ms: i64) -> Option<Vec<String>> {
        self.journal.clone()
    }
}

fn quiet_probe() -> FakeProbe {
    FakeProbe {
        restarts: None,
        active_since: None,
        journal: None,
    }
}

fn opts(root: &Path, url: &str, soak_start_ms: Option<i64>) -> ParityOptions {
    ParityOptions {
        search_url: url.to_owned(),
        vault_id: "default".into(),
        soak_start_ms,
        skip: Vec::new(),
        repo_roots: vec![root.join("repos")],
        now_ms: crate::now_ms(),
    }
}

fn enqueue(db: &mut StateDb, job_type: &str) -> String {
    let r = db
        .enqueue(
            NewJob {
                job_type: job_type.into(),
                lane: Lane::User,
                dedupe_key: None,
                params: json!({}),
                resource_keys: vec![format!("k::{job_type}")],
                service: None,
            },
            crate::now_ms(),
        )
        .unwrap();
    match r {
        EnqueueOutcome::Enqueued(id) | EnqueueOutcome::Duplicate(id) => id,
        EnqueueOutcome::Suppressed => panic!("suppressed"),
    }
}

fn shadow_write(db: &mut StateDb, op: &str, path: &str, job: Option<&str>, at_ms: i64) {
    db.append_shadow_write(&NewShadowWrite {
        op: op.into(),
        path: path.into(),
        before_hash: None,
        after_hash: (op != "delete").then(|| "h".to_owned()),
        job_id: job.map(str::to_owned),
        at_ms,
    })
    .unwrap();
}

// ---- report ---------------------------------------------------------------------------------

#[test]
fn report_groups_by_action_op_and_class_without_paths() {
    let t = Tmp::new();
    let p = t.0.join("state.db");
    {
        let mut db = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
        let a = enqueue(&mut db, "action:period.make");
        let b = enqueue(&mut db, "action:capture.daylog");
        shadow_write(&mut db, "create", "daily/2026-10-01.md", Some(&a), 10);
        shadow_write(&mut db, "modify", "daily/2026-10-01.md", Some(&b), 20);
        shadow_write(&mut db, "modify", "core/Zebracorn Secret.md", Some(&b), 30);
        shadow_write(&mut db, "create", "Zebracorn Root.md", None, 40);
        shadow_write(&mut db, "create", "people/zebracorn.md", None, 50);
    }
    let r = StateReader::open(&p).unwrap();
    let rep = report::build(&r, None).unwrap();
    assert_eq!(rep.total, 5);
    assert_eq!(rep.by_action["period.make"], 1);
    assert_eq!(rep.by_action["capture.daylog"], 2);
    assert_eq!(rep.by_action["direct"], 2);
    assert_eq!(rep.by_op["create"], 3);
    assert_eq!(rep.by_class["daily:md"], 2);
    assert_eq!(rep.by_class["core:md"], 1);
    assert_eq!(rep.by_class["other:md"], 2);
    assert_eq!((rep.first_at_ms, rep.last_at_ms), (Some(10), Some(50)));
    assert_eq!(rep.outside_overlay, 0);
    let s = serde_json::to_string(&rep.to_json())
        .unwrap()
        .to_lowercase();
    assert!(!s.contains("zebracorn") && !s.contains("2026-10-01"), "{s}");
    let since = report::build(&r, Some(35)).unwrap();
    assert_eq!(since.total, 2);
}

// ---- parity ---------------------------------------------------------------------------------

const MD5: &str = "0123456789abcdef0123456789abcdef";

struct ParityWorld {
    t: Tmp,
    cfg: Config,
}

fn lint_note(wc: usize) -> String {
    format!(
        "---\nword-count: {wc}\nsource: https://www.youtube.com/watch?v=dQw4w9WgXcQ\nyt-video-id: dQw4w9WgXcQ\n---\n# Zebracorn lint\none two three four five\n"
    )
}

fn parity_world() -> ParityWorld {
    let t = Tmp::new();
    let cfg = config(&t.0, Mode::Shadow);
    let v = &cfg.vault;
    write(
        v,
        "notes/zebracorn-match.md",
        "# Zebracorn match\nsee [[zebracorn-drift]]\n",
    );
    write(
        v,
        "notes/zebracorn-drift.md",
        "# Zebracorn drift\nedited during the soak\n",
    );
    write(
        v,
        "notes/zebracorn-facet.md",
        &format!("# Zebracorn facet\n\n![[{MD5}_MD5.png]]\n"),
    );
    write(
        v,
        "notes/zebracorn-unexplained.md",
        "# Zebracorn unexplained\n[[missing-target]]\n",
    );
    write(v, &format!("att/{MD5}_MD5.png"), "png");
    let wc = cv_lint::calculate_word_count(&lint_note(0));
    assert_eq!(cv_lint::calculate_word_count(&lint_note(wc)), wc);
    write(v, "notes/zebracorn-lint-ok.md", &lint_note(wc));
    write(v, "notes/zebracorn-lint-bad.md", &lint_note(wc + 7));
    write(
        v,
        "daily/2026-10-01.md",
        "---\ntags: [daily]\ntitle: \"Zebracorn day\"\n---\n# Day\n",
    );
    write(
        v,
        "daily/2026-10-02.md",
        "---\ntitle: Zebracorn two\n---\n# Two\n",
    );
    write(v, "inbox/zebracorn-clip.md", "clipped zebracorn\n");
    for p in [
        "notes/zebracorn-match.md",
        "notes/zebracorn-facet.md",
        "notes/zebracorn-unexplained.md",
        "notes/zebracorn-lint-ok.md",
        "notes/zebracorn-lint-bad.md",
    ] {
        age(v, p, 3);
    }
    // Overlay (the daemon's would-be content).
    let o = cfg.scratch.join("vault");
    write(
        &o,
        "daily/2026-10-01.md",
        "---\ntitle: Zebracorn day\ntags:\n  - daily\n---\n# Day\n",
    );
    write(
        &o,
        "daily/2026-10-02.md",
        "---\ntitle: Zebracorn two\n---\n# Two!\n",
    );
    write(&o, "inbox/zebracorn-clip.md", "clipped zebracorn\n");
    write(
        &cfg.config_dir,
        "actions.toml",
        "[[action]]\nid = \"period.make\"\nresources = { note = false, keys = [\"note::period::day\"] }\n[[action.steps]]\ntype = \"materialize\"\nperiod = \"day\"\n",
    );
    std::fs::create_dir_all(t.0.join("state")).unwrap();
    {
        let mut db = StateDb::open(&cfg.state_db, 1, QueueConfig::default()).unwrap();
        let stuck = enqueue(&mut db, "action:stuck");
        let claimed = db.claim(crate::now_ms() - 2 * 3_600_000).unwrap().unwrap();
        assert_eq!(claimed.id, stuck);
        let m = enqueue(&mut db, "action:period.make");
        shadow_write(&mut db, "create", "daily/2026-10-01.md", Some(&m), 10);
        shadow_write(&mut db, "create", "daily/2026-10-02.md", Some(&m), 11);
        shadow_write(&mut db, "create", "inbox/zebracorn-clip.md", None, 12);
        shadow_write(&mut db, "create", "../zebracorn-escape.md", None, 13);
        db.put_image_description(&ImageDescription {
            md5: MD5.into(),
            kind: "vision".into(),
            narrative: "a synthetic picture".into(),
            extraction: String::new(),
            description_hash: "abcd1234".into(),
            described_at: "2026-01-01T00:00:00.000Z".into(),
            raw: "{}".into(),
        })
        .unwrap();
    }
    ParityWorld { t, cfg }
}

/// The local hashes parity computes (same `NotePrep` code), for seeding the fake.
fn local_hashes(cfg: &Config) -> BTreeMap<String, String> {
    let base = cfg.scratch.join("seed").join("index.db");
    std::fs::create_dir_all(base.parent().unwrap()).unwrap();
    drop(cv_index::Index::create(&cfg.vault, &base).unwrap());
    let reader = cv_index::Reader::open_current(&base).unwrap();
    let state = StateReader::open(&cfg.state_db).unwrap();
    let vault = cfg.vault.clone();
    let prep = crate::steps::search::NotePrep::new(
        Box::new(move |p| Ok(std::fs::read(vault.join(p)).ok())),
        &reader,
        Box::new(move |m| {
            state
                .image_descriptions_for(&[m.to_owned()])
                .ok()?
                .into_iter()
                .next()
        }),
    );
    let out = prep.local_hashes().unwrap().into_iter().collect();
    drop(prep);
    drop(reader);
    let _ = std::fs::remove_dir_all(cfg.scratch.join("seed"));
    out
}

fn assert_no_fixture_text(out: &Value) {
    let s = serde_json::to_string(out).unwrap().to_lowercase();
    for needle in [
        "zebracorn",
        "2026-10-0",
        "notes/",
        "inbox",
        "synthetic picture",
        "dqw4w9",
    ] {
        assert!(!s.contains(needle), "{needle} leaked: {s}");
    }
}

#[test]
fn parity_reports_every_criterion_as_aggregates() {
    let w = parity_world();
    let rt = runtime();
    let f = fake(&rt, true);
    let hashes = local_hashes(&w.cfg);
    for (p, h) in &hashes {
        let stale = ["drift", "facet", "unexplained"]
            .iter()
            .any(|k| p.contains(k));
        f.set(p, if stale { "stale" } else { h });
    }
    let now = crate::now_ms();
    let probe = FakeProbe {
        restarts: Some(2),
        active_since: Some(now - 8 * 86_400_000),
        journal: Some(vec![
            "landlock status=Enforced".into(),
            "thread 'x' panicked at src/x.rs:1:1".into(),
            format!("DEBUG {CYCLE_GUARD_LINE}"),
        ]),
    };
    let o = opts(&w.t.0, &f.base, Some(now - 86_400_000));
    let out = parity::run(&w.cfg, &o, &probe).unwrap();
    let c = &out["criteria"];

    let p1 = &c["P1"]["metrics"];
    assert_eq!(p1["md_files"], 9);
    assert_eq!(p1["indexed_files"], 10);
    assert_eq!(p1["links"], 3);
    assert_eq!(p1["unresolved"], 1);
    assert!(p1["drift_pct"]["files"].as_f64().unwrap() < -99.0);
    assert_eq!(c["P1"]["status"], "warn");

    let p2 = &c["P2"]["metrics"];
    assert_eq!(c["P2"]["status"], "manual");
    assert_eq!(p2["word_count"]["compared"], 2);
    assert_eq!(p2["word_count"]["agreed"], 1);
    assert_eq!(p2["yt_video_id"]["compared"], 2);
    assert_eq!(p2["yt_video_id"]["agreed"], 2);
    assert_eq!(p2["post_id"]["compared"], 0);

    let p3 = &c["P3"]["metrics"];
    assert_eq!(p3["indexable"], 9);
    assert_eq!(p3["matched"], 6);
    assert_eq!(p3["mismatched"], 3);
    assert_eq!(p3["note_drift"], 1);
    assert_eq!(p3["facet_drift"], 1);
    assert_eq!(p3["unexplained"], 1);
    assert_eq!(p3["companion_mutations"], "structural");
    assert_eq!(c["P3"]["status"], "fail");

    let p4 = &c["P4"]["metrics"];
    assert_eq!(
        (
            p4["candidates"].clone(),
            p4["compared"].clone(),
            p4["equal"].clone()
        ),
        (json!(2), json!(2), json!(1))
    );
    assert_eq!(c["P4"]["status"], "fail");
    let p5 = &c["P5"]["metrics"];
    assert_eq!(
        (p5["compared"].clone(), p5["equal"].clone()),
        (json!(1), json!(1))
    );
    assert_eq!(c["P5"]["status"], "pass");

    let p6 = &c["P6"]["metrics"];
    assert_eq!(p6["outside_overlay_rows"], 1);
    assert_eq!(p6["landlock_denials"], 0);
    assert_eq!(p6["scratch_outside_vault_and_repos"], true);
    assert_eq!(p6["config_dir_outside_vault_and_repos"], true);
    assert_eq!(c["P6"]["status"], "fail");

    let p7 = &c["P7"]["metrics"];
    assert_eq!(p7["stuck_running_jobs"], 1);
    assert_eq!(p7["n_restarts"], 2);
    assert_eq!(p7["panics"], 1);
    assert_eq!(p7["cycle_guard_trips"], 1);
    assert_eq!(c["P7"]["status"], "fail");

    assert_no_fixture_text(&out);
    let log = f.log();
    assert!(log.iter().all(|p| !p.starts_with("/v1/chunks")), "{log:?}");
    assert!(log.contains(&"/v1/paths".to_owned()));
    // The parity scratch base is gone; the vault is untouched.
    let left: Vec<_> = std::fs::read_dir(&w.cfg.scratch)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("parity-"))
        .collect();
    assert!(left.is_empty());
    assert!(!w.cfg.vault.join(".cv-index").exists());
}

#[test]
fn parity_companion_down_is_unknown_and_skip_works() {
    let w = parity_world();
    let rt = runtime();
    let f = fake(&rt, false);
    let mut o = opts(&w.t.0, &f.base, None);
    o.skip = vec!["P7".into()];
    let out = parity::run(&w.cfg, &o, &quiet_probe()).unwrap();
    let c = &out["criteria"];
    assert_eq!(c["P3"]["status"], "unknown");
    assert_eq!(c["P7"]["status"], "skipped");
    assert_eq!(c["P6"]["metrics"]["landlock_denials"], Value::Null);
    assert_no_fixture_text(&out);
}

#[test]
fn parity_refuses_scratch_inside_a_repo_root() {
    let w = parity_world();
    let mut o = opts(&w.t.0, "http://127.0.0.1:9", None);
    o.repo_roots = vec![w.t.0.clone()];
    assert!(parity::run(&w.cfg, &o, &quiet_probe()).is_err());
}

// ---- search refactor: parity's hash == the feed step's ---------------------------------------

struct NoKids;
impl ChildSpawner for NoKids {
    fn spawn_child(&self, _: &str, _: Map<String, Value>) -> Result<String, String> {
        Err("no children".into())
    }
}

struct NoSecrets;
impl SecretSource for NoSecrets {
    fn env_var(&self, _: &str) -> Option<String> {
        None
    }
    fn secret_file(&self, _: &str) -> Option<String> {
        None
    }
}

#[test]
fn parity_local_hash_equals_the_feed_steps_hash() {
    let w = parity_world();
    let rt = runtime();
    let f = fake(&rt, true);
    // The live feed step upserts every note (reconcile) into the fake companion.
    let fs = cv_vault::VaultFs::live(&w.cfg.vault, &w.cfg.protected_vault, &cv_vault::NoCutover)
        .unwrap();
    let index = w.t.0.join("feed-index").join("index.db");
    std::fs::create_dir_all(index.parent().unwrap()).unwrap();
    let vw = cv_vault::VaultWriter::open(fs, &index).unwrap();
    let (writer, _join) = cv_vault::spawn_writer(vw).unwrap();
    let state = Arc::new(Mutex::new(
        StateDb::open(&w.cfg.state_db, 1, QueueConfig::default()).unwrap(),
    ));
    let mut set = ConfigSet::new();
    set.reload(
        &BTreeMap::from([(
            "actions.toml".to_owned(),
            Some("[[action]]\nid = \"s\"\n[[action.steps]]\ntype = \"search_upsert\"\n".to_owned()),
        )]),
        &crate::steps::validate,
    );
    let step: toml::Table = toml::from_str(&format!("companion_url = \"{}\"", f.base)).unwrap();
    let step = Step::SearchUpsert(step);
    let (params, vars, context) = (Map::new(), Map::new(), ActionContext::default());
    let mut ctx = StepCtx {
        job_id: "job-1",
        action: set.action("s").unwrap(),
        step_index: 0,
        params: &params,
        context: &context,
        vars: &vars,
        config: &set,
        vault_root: &w.cfg.vault,
        index_db: &index,
        state: &state,
        writer: Some(writer.clone()),
        children: &NoKids,
        written: Vec::new(),
    };
    let mgr = ProviderManager::new(Arc::new(NoSecrets)).unwrap();
    crate::steps::search::run(&step, &mut ctx, rt.handle(), &mgr).unwrap();
    drop(writer);
    drop(state);
    assert!(f.log().iter().any(|p| p == "/v1/chunks/upsert"));
    let upserted = f.st.lock().unwrap().store.len();
    assert_eq!(upserted, 9);
    let out = parity::run(&w.cfg, &opts(&w.t.0, &f.base, None), &quiet_probe()).unwrap();
    let p3 = &out["criteria"]["P3"];
    assert_eq!(p3["metrics"]["matched"], 9, "{p3}");
    assert_eq!(p3["metrics"]["match_rate"], 1.0);
    assert_eq!(p3["status"], "pass");
}

// ---- import-descriptions --------------------------------------------------------------------

#[test]
fn import_refuses_while_a_daemon_socket_answers() {
    let t = Tmp::new();
    let cfg = config(&t.0, Mode::Shadow);
    let src = t.0.join("descriptions");
    write(
        &src,
        &format!("{MD5}.json"),
        &format!(
            r#"{{"md5":"{MD5}","narrative":"n","extraction":"","kind":"vision","describedAt":"t","schemaVersion":1,"descriptionHash":"1234abcd"}}"#
        ),
    );
    std::fs::create_dir_all(cfg.socket.parent().unwrap()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&cfg.socket).unwrap();
    let err = import::run(&cfg, &src).unwrap_err().to_string();
    assert!(err.contains("stop it first"), "{err}");
    assert!(!cfg.state_db.exists());
    drop(listener);
    std::fs::remove_file(&cfg.socket).unwrap();
    let ok = import::run(&cfg, &src).unwrap();
    assert_eq!(ok["imported"], 1);
    let r = StateReader::open(&cfg.state_db).unwrap();
    assert_eq!(r.image_descriptions_for(&[MD5.into()]).unwrap().len(), 1);
}

// ---- replay ---------------------------------------------------------------------------------

const REPLAY_ACTIONS: &str = r#"
[[action]]
id = "make-note"
label = "Make note"
resources = { note = false, keys = ["{param.path}"] }
starters = [
  { type = "event", kinds = ["created", "modified"], origin = "any", selector = { folder = "notes" } },
]
[[action.params]]
name = "path"
label = "Path"
kind = "path"
required = true
[[action.steps]]
type = "materialize"
path = "{param.path}"
template = ""

[[action]]
id = "echo-on-create"
label = "Would fire on any create"
starters = [
  { type = "event", kinds = ["created", "modified"], origin = "any", selector = { folder = "notes" } },
]
[[action.steps]]
type = "lint"

[[action]]
id = "stamp"
label = "Stamp the active note"
needs = ["active_path"]
[[action.steps]]
type = "set_props"
props = { stamped = "yes" }

[[action]]
id = "feed"
label = "Feed"
[[action.steps]]
type = "search_upsert"
"#;

fn replay_world() -> (Tmp, Config) {
    let t = Tmp::new();
    let cfg = config(&t.0, Mode::Shadow);
    write(&cfg.vault, "a.md", "# A\n");
    write(&t.0.join("corpus"), "notes/existing.md", "# Existing\n");
    write(&t.0.join("prot/c"), "x.md", "x");
    write(&t.0.join("repos/c"), "x.md", "x");
    write(&cfg.config_dir, "actions.toml", REPLAY_ACTIONS);
    (t, cfg)
}

fn req(t: &Tmp, corpus: &str, action: &str) -> ReplayRequest {
    let mut params = Map::new();
    params.insert("path".into(), Value::String("notes/new-one".into()));
    ReplayRequest {
        action_id: action.into(),
        corpus: t.0.join(corpus),
        params,
        active_path: None,
        repo_roots: vec![t.0.join("repos")],
    }
}

#[test]
fn replay_refuses_protected_repo_vault_and_search_actions() {
    let (t, cfg) = replay_world();
    let rt = runtime();
    for (corpus, action, needle) in [
        ("prot/c", "make-note", "live root"),
        ("repos/c", "make-note", "repo root"),
        ("vault", "make-note", "CV_VAULT"),
        ("corpus", "feed", "search companion"),
    ] {
        let err = rt
            .block_on(replay::run(&cfg, &req(&t, corpus, action)))
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains(needle), "{corpus}/{action}: {msg}");
    }
    assert!(!cfg.scratch.exists() || std::fs::read_dir(&cfg.scratch).unwrap().count() == 0);
}

#[test]
fn replay_runs_one_action_against_the_corpus() {
    let (t, cfg) = replay_world();
    let rt = runtime();
    let out = rt
        .block_on(replay::run(&cfg, &req(&t, "corpus", "make-note")))
        .unwrap();
    assert_eq!(out["status"], "done", "{out}");
    let writes = out["writes"].as_array().unwrap();
    assert!(
        writes
            .iter()
            .any(|w| w["op"] == "create" && w["path"] == "notes/new-one.md"),
        "{out}"
    );
    assert!(t.0.join("corpus/notes/new-one.md").exists());
    // Only the replayed run: the event starters were stripped.
    let dir = PathBuf::from(out["replay_dir"].as_str().unwrap());
    let r = StateReader::open(&dir.join("state/state.db")).unwrap();
    let jobs = r.list(&cv_state::ListQuery::default()).unwrap();
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0].job_type, "action:make-note");
    // The live vault is untouched.
    assert_eq!(std::fs::read_dir(&cfg.vault).unwrap().count(), 1);
}

#[test]
fn replay_refuses_a_link_write_into_a_repo_outside_the_corpus() {
    let (t, cfg) = replay_world();
    // A stand-in "repo" outside the corpus, linked from it (as a `cp -a` copy preserves).
    let repo = t.0.join("standin-repo");
    write(&repo, "INITIATIVE.md", "# Init\n");
    std::os::unix::fs::symlink(
        repo.join("INITIATIVE.md"),
        t.0.join("corpus/notes/INITIATIVE.md"),
    )
    .unwrap();
    let mut r = req(&t, "corpus", "stamp");
    r.active_path = Some("notes/INITIATIVE.md".into());
    let out = runtime().block_on(replay::run(&cfg, &r)).unwrap();
    assert_ne!(out["status"], "done", "{out}");
    assert_eq!(
        std::fs::read_to_string(repo.join("INITIATIVE.md")).unwrap(),
        "# Init\n"
    );
    assert!(
        t.0.join("corpus/notes/INITIATIVE.md")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    // The same action on a regular corpus note goes through.
    let mut ok = req(&t, "corpus", "stamp");
    ok.active_path = Some("notes/existing.md".into());
    let out = runtime().block_on(replay::run(&cfg, &ok)).unwrap();
    assert_eq!(out["status"], "done", "{out}");
}
