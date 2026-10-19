//! WP-9 `daily_brief` + `ignore_video` tests: a synthetic temp vault, a temp `state.db` and a
//! local fake Frankfurter/Open-Meteo (never the network, never the live vault, never env vars).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_protocol::ActionContext;
use cv_state::{QueueConfig, StateDb};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::steps::web::{Overrides, test_overrides};
use crate::steps::{ChildSpawner, DeferCause, StepCtx, StepError, StepOutcome};

struct NoChildren;
impl ChildSpawner for NoChildren {
    fn spawn_child(&self, _: &str, _: Map<String, Value>) -> Result<String, String> {
        Err("no children".into())
    }
}

struct Tmp(std::path::PathBuf);
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

type Routes = Vec<(&'static str, u16, &'static str)>;

/// Route prefix (path + query) → (status, body); counts every request.
async fn serve(routes: Routes, hits: Arc<AtomicUsize>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let (routes, hits) = (routes.clone(), hits.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                hits.fetch_add(1, Ordering::SeqCst);
                let head = String::from_utf8_lossy(&buf).into_owned();
                let path = head.split(' ').nth(1).unwrap_or("/").to_owned();
                let (status, body) = routes
                    .iter()
                    .find(|(p, _, _)| path.starts_with(p))
                    .map_or((404, ""), |(_, s, b)| (*s, *b));
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

struct World {
    _tmp: Tmp,
    rt: tokio::runtime::Runtime,
    vault: std::path::PathBuf,
    index: std::path::PathBuf,
    writer: cv_vault::WriterHandle,
    state: Arc<Mutex<StateDb>>,
    config: ConfigSet,
    base: String,
    hits: Arc<AtomicUsize>,
}

const FX_OK: &str = r#"{"amount":1.0,"base":"USD","date":"2026-10-14","rates":{"MXN":18.2345}}"#;
const WX_OK: &str =
    r#"{"daily":{"temperature_2m_max":[24.6],"temperature_2m_min":[12.4],"weather_code":[61]}}"#;

const BRIEF_CFG: &str = r#"
[daily_brief]
enabled = true
[[daily_brief.fx]]
base = "USD"
quote = "MXN"
[[daily_brief.fx]]
base = "EUR"
quote = "USD"
label = "Euro"
[[daily_brief.weather]]
label = "Place A"
lat = 1.5
lon = 2.0
"#;

fn world(files: &[(&str, &str)], routes: Routes, pipelines: &str) -> World {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "cv-wp9-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    let tmp = Tmp(root.clone());
    let vault = root.join("vault");
    let protected = root.join("protected");
    std::fs::create_dir_all(&protected).unwrap();
    std::fs::create_dir_all(&vault).unwrap();
    for (rel, text) in files {
        let p = vault.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let fs = cv_vault::VaultFs::live(&vault, &protected, &cv_vault::NoCutover).unwrap();
    let index = root.join("index.db");
    let w = cv_vault::VaultWriter::open(fs, &index).unwrap();
    let (writer, _join) = cv_vault::spawn_writer(w).unwrap();
    let state = StateDb::open(&root.join("state.db"), 1, QueueConfig::default()).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let base = rt.block_on(serve(routes, hits.clone()));
    let cfg = BTreeMap::from([
        ("pipelines.toml".to_owned(), Some(pipelines.to_owned())),
        (
            "captures.toml".to_owned(),
            Some("[periods.day]\nfolder = \"daily\"\nformat = \"YYYY-MM-DD\"\n".to_owned()),
        ),
        (
            "sources.toml".to_owned(),
            Some("[[youtube]]\nchannel = \"Tracked Name\"\nid = \"UCtracked\"\n".to_owned()),
        ),
    ]);
    let mut config = ConfigSet::new();
    config.reload(&cfg, &crate::steps::validate);
    World {
        vault: std::fs::canonicalize(&vault).unwrap(),
        _tmp: tmp,
        rt,
        index,
        writer,
        state: Arc::new(Mutex::new(state)),
        config,
        base,
        hits,
    }
}

fn step(kind: &str, src: &str) -> Step {
    let t: toml::Table = format!("type = \"{kind}\"\n{src}").parse().unwrap();
    Step::from_table(t).unwrap()
}

fn action() -> Action {
    Action {
        id: "wp9".into(),
        label: "WP9".into(),
        group: None,
        keys: vec![],
        params: vec![],
        needs: vec![],
        lane: LaneChoice::Auto,
        durable: true,
        steps: vec![],
        starters: vec![],
        resources: ResourceSpec {
            note: true,
            keys: vec![],
        },
    }
}

fn run(w: &World, s: &Step, active: Option<&str>) -> Result<StepOutcome, StepError> {
    let _rt = w.rt.enter();
    let _o = test_overrides(Overrides {
        bases: HashMap::from([
            ("frankfurter".to_owned(), format!("{}/fx", w.base)),
            ("open-meteo".to_owned(), format!("{}/wx", w.base)),
        ]),
        secrets: None,
    });
    let action = action();
    let context = ActionContext {
        active_path: active.map(str::to_owned),
        ..ActionContext::default()
    };
    let (params, vars) = (Map::new(), Map::new());
    let mut ctx = StepCtx {
        job_id: "job-1",
        action: &action,
        step_index: 0,
        params: &params,
        context: &context,
        vars: &vars,
        config: &w.config,
        vault_root: &w.vault,
        index_db: &w.index,
        search: None,
        state: &w.state,
        writer: Some(w.writer.clone()),
        children: &NoChildren,
        written: Vec::new(),
    };
    crate::steps::execute(s, &mut ctx)
}

fn today_path() -> String {
    format!("daily/{}.md", chrono::Local::now().format("%Y-%m-%d"))
}

fn read(w: &World, rel: &str) -> String {
    std::fs::read_to_string(w.vault.join(rel)).unwrap()
}

fn brief(w: &World) -> Result<StepOutcome, StepError> {
    run(w, &step("daily_brief", ""), None)
}

// ---- ignore_video -------------------------------------------------------------------------

const SHORTS_CFG: &str = "[ignore_shorts]\nchannels = [\"UCshorts\", \"tracked name\"]\n";

fn meta_note(id: &str, channel: &str, secs: i64) -> String {
    format!(
        "---\nvideoId: {id}\nchannelId: {channel}\nchannelTitle: \"Some Title\"\nduration_seconds: {secs}\n---\n# T\n"
    )
}

fn ignored(w: &World, id: &str) -> bool {
    w.state.lock().unwrap().is_ignored("youtube", id).unwrap()
}

#[test]
fn ignore_auto_short_on_configured_channel_and_idempotent() {
    let n1 = meta_note("short1", "UCshorts", 30);
    let n2 = meta_note("short2", "UCtracked", 59);
    let w = world(
        &[
            ("_yt_metadata/c/short1.md", &n1),
            ("_yt_metadata/t/short2.md", &n2),
        ],
        vec![],
        SHORTS_CFG,
    );
    let s = step("ignore_video", "");
    for _ in 0..2 {
        let o = run(&w, &s, Some("_yt_metadata/c/short1.md")).unwrap();
        assert_eq!(o.vars["ignored"], json!(true));
        assert_eq!(o.vars["reason"], json!("short"));
        assert_eq!(o.vars["video_id"], json!("short1"));
    }
    assert!(ignored(&w, "short1"));
    // Matched by the sources.toml display name of a tracked channel id.
    let o = run(&w, &s, Some("_yt_metadata/t/short2.md")).unwrap();
    assert_eq!(o.vars["ignored"], json!(true));
    // The note is untouched.
    assert_eq!(read(&w, "_yt_metadata/c/short1.md"), n1);
}

#[test]
fn ignore_auto_noop_for_long_or_unconfigured_and_manual_ignores() {
    let long = meta_note("long1", "UCshorts", 600);
    let other = meta_note("other1", "UCother", 20);
    let w = world(
        &[
            ("_yt_metadata/c/long1.md", &long),
            ("_yt_metadata/o/other1.md", &other),
        ],
        vec![],
        SHORTS_CFG,
    );
    let auto = step("ignore_video", "mode = \"auto\"");
    for p in ["_yt_metadata/c/long1.md", "_yt_metadata/o/other1.md"] {
        let o = run(&w, &auto, Some(p)).unwrap();
        assert_eq!(o.vars["ignored"], json!(false));
        assert_eq!(o.vars["reason"], json!(""));
    }
    assert!(!ignored(&w, "long1") && !ignored(&w, "other1"));
    let manual = step("ignore_video", "mode = \"manual\"");
    let o = run(&w, &manual, Some("_yt_metadata/c/long1.md")).unwrap();
    assert_eq!(
        (&o.vars["ignored"], &o.vars["reason"]),
        (&json!(true), &json!("manual"))
    );
    assert!(ignored(&w, "long1"));
    assert!(crate::steps::validate(&step("ignore_video", "mode = \"x\""), &w.config).is_err());
    assert!(crate::steps::validate(&step("ignore_video", "bogus = 1"), &w.config).is_err());
}

// ---- daily_brief --------------------------------------------------------------------------

#[test]
fn brief_disabled_is_noop() {
    let w = world(&[], vec![], "");
    let o = brief(&w).unwrap();
    assert_eq!(o.vars["disabled"], json!(true));
    assert_eq!(w.hits.load(Ordering::SeqCst), 0);
    assert!(!w.vault.join(today_path()).exists());
}

#[test]
fn brief_nonempty_section_skips_without_fetch() {
    let p = today_path();
    let note = "# Day\n\n# Daily Brief\n- already here\n";
    let w = world(&[(&p, note)], vec![("/fx", 200, FX_OK)], BRIEF_CFG);
    let o = brief(&w).unwrap();
    assert_eq!(o.vars["skipped"], json!(true));
    assert_eq!(w.hits.load(Ordering::SeqCst), 0);
    assert_eq!(read(&w, &p), note);
}

#[test]
fn brief_partial_failure_writes_lines_and_inline_note() {
    let p = today_path();
    let w = world(
        &[(&p, "# Day\n\n# Daily Brief\n\n# Notes\nkeep\n")],
        vec![
            ("/fx/latest?from=USD", 200, FX_OK),
            ("/fx/latest?from=EUR", 500, ""),
            ("/wx/forecast", 200, WX_OK),
        ],
        BRIEF_CFG,
    );
    let o = brief(&w).unwrap();
    assert_eq!(
        (
            &o.vars["fx"],
            &o.vars["weather"],
            &o.vars["failed"],
            &o.vars["skipped"]
        ),
        (&json!(1), &json!(1), &json!(1), &json!(false))
    );
    let text = read(&w, &p);
    assert!(
        text.starts_with("# Day\n\n# Daily Brief\n\n**FX Rates** _(updated "),
        "{text}"
    );
    assert!(text.contains("\n- USD/MXN: 18.23 _(as of 2026-10-14)_\n- Euro: *lookup failed ("));
    assert!(
        text.ends_with(
            "\n\n**Weather**\n- Place A: ↑25°C / ↓12°C, light rain\n\n\n# Notes\nkeep\n"
        ),
        "{text}"
    );
    let usage = w.state.lock().unwrap();
    let day = crate::steps::web::local_day();
    assert_eq!(usage.api_usage("frankfurter", &day).unwrap(), 2);
    assert_eq!(usage.api_usage("open-meteo", &day).unwrap(), 1);
}

#[test]
fn brief_total_failure_defers_without_write() {
    let p = today_path();
    let w = world(&[(&p, "# Day\n")], vec![("/", 503, "")], BRIEF_CFG);
    let e = brief(&w).unwrap_err();
    assert!(
        matches!(
            e,
            StepError::Deferred {
                cause: DeferCause::Transient(_),
                ..
            }
        ),
        "{e:?}"
    );
    assert_eq!(read(&w, &p), "# Day\n");
}

#[test]
fn brief_absent_section_appended_once_then_skips() {
    let w = world(
        &[],
        vec![("/fx", 200, FX_OK), ("/wx/forecast", 200, WX_OK)],
        "[daily_brief]\nenabled = true\n[[daily_brief.fx]]\nbase = \"USD\"\nquote = \"MXN\"\n",
    );
    let o = brief(&w).unwrap();
    let p = today_path();
    assert_eq!(o.output_paths, vec![p.clone()]);
    let text = read(&w, &p);
    assert!(
        text.starts_with("# Daily Brief\n\n\n**FX Rates** _(updated "),
        "{text}"
    );
    assert!(
        text.ends_with(")_\n- USD/MXN: 18.23 _(as of 2026-10-14)_\n"),
        "{text}"
    );
    let hits = w.hits.load(Ordering::SeqCst);
    let o = brief(&w).unwrap();
    assert_eq!(o.vars["skipped"], json!(true));
    assert_eq!(w.hits.load(Ordering::SeqCst), hits);
    assert_eq!(read(&w, &p), text);
}

/// SR7: a concurrent edit between the brief's read and its CAS write is kept and the brief is
/// re-applied on top of it.
#[test]
fn brief_concurrent_edit_is_kept_and_brief_reapplied() {
    let p = today_path();
    let w = world(
        &[(&p, "# Day\n\n# Daily Brief\n\n# Notes\nkeep\n")],
        vec![("/fx", 200, FX_OK), ("/wx/forecast", 200, WX_OK)],
        BRIEF_CFG,
    );
    crate::steps::race::inject(&w.vault, &p, 1, |t| format!("{t}racer\n"));
    let o = brief(&w).unwrap();
    assert_eq!(o.vars["skipped"], json!(false));
    let text = read(&w, &p);
    assert!(text.contains("# Daily Brief\n\n**FX Rates**"), "{text}");
    assert!(text.ends_with("# Notes\nkeep\nracer\n"), "{text}");
}

/// SR7: a section filled by a concurrent edit is not appended to a second time.
#[test]
fn brief_section_filled_meanwhile_skips() {
    let p = today_path();
    let w = world(
        &[(&p, "# Day\n\n# Daily Brief\n")],
        vec![("/fx", 200, FX_OK), ("/wx/forecast", 200, WX_OK)],
        BRIEF_CFG,
    );
    crate::steps::race::inject(&w.vault, &p, 1, |t| format!("{t}filled\n"));
    let o = brief(&w).unwrap();
    assert_eq!(o.vars["skipped"], json!(true));
    assert_eq!(read(&w, &p), "# Day\n\n# Daily Brief\nfilled\n");
}
