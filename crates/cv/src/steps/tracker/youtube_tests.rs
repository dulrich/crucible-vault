//! WP-7 `track_youtube` tests: a synthetic temp vault, a temp `state.db` and a local fake Data
//! API (never the network, never the live vault, never env vars).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_protocol::ActionContext;
use cv_providers::secrets::SecretSource;
use cv_state::{FailureReason, QueueConfig, StateDb, TrackerOutcome};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::super::hook;
use super::{KIND, execute, validate};
use crate::steps::web::{Overrides, test_overrides};
use crate::steps::{ChildSpawner, StepCtx, StepError, StepOutcome};

struct NoChildren;
impl ChildSpawner for NoChildren {
    fn spawn_child(&self, _: &str, _: Map<String, Value>) -> Result<String, String> {
        Err("no children".into())
    }
}

struct Keys(Option<&'static str>);
impl SecretSource for Keys {
    fn env_var(&self, n: &str) -> Option<String> {
        (n == "YOUTUBE_API_KEY")
            .then(|| self.0.map(str::to_owned))
            .flatten()
    }
    fn secret_file(&self, _: &str) -> Option<String> {
        None
    }
}

struct Tmp(std::path::PathBuf);
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

type Routes = Vec<(&'static str, u16, String)>;

/// Route substring → (status, body); logs every request target.
async fn serve(routes: Routes, log: Arc<Mutex<Vec<String>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let (routes, log) = (routes.clone(), log.clone());
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
                let head = String::from_utf8_lossy(&buf).into_owned();
                let path = head.split(' ').nth(1).unwrap_or("/").to_owned();
                log.lock().unwrap().push(path.clone());
                let (status, body) = routes
                    .iter()
                    .find(|(p, _, _)| path.contains(p))
                    .map_or((404, String::new()), |(_, s, b)| (*s, b.clone()));
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
    log: Arc<Mutex<Vec<String>>>,
    key: Option<&'static str>,
}

const V1: &str = "aaaaaaaaaa1";
const V2: &str = "aaaaaaaaaa2";
const V3: &str = "aaaaaaaaaa3";

/// Uploads newest first (V3, V2, V1).
fn playlist(ids: &[&str]) -> String {
    let items: Vec<String> = ids
        .iter()
        .rev()
        .map(|id| {
            format!(
                r#"{{"snippet":{{"title":"Feed {id}","channelId":"UCa","channelTitle":"Chan A","publishedAt":"2026-10-14T00:00:00Z"}},"contentDetails":{{"videoId":"{id}"}}}}"#
            )
        })
        .collect();
    format!(r#"{{"items":[{}]}}"#, items.join(","))
}

fn videos(spec: &[(&str, &str)]) -> String {
    let items: Vec<String> = spec
        .iter()
        .map(|(id, dur)| {
            format!(
                r#"{{"id":"{id}","snippet":{{"title":"Video {id}","channelId":"UCa","channelTitle":"Chan A","publishedAt":"2026-10-14T00:00:00Z","categoryId":"1","liveBroadcastContent":"none"}},"contentDetails":{{"duration":"{dur}"}},"statistics":{{}}}}"#
            )
        })
        .collect();
    format!(r#"{{"items":[{}]}}"#, items.join(","))
}

fn std_routes() -> Routes {
    vec![
        ("playlistId=UUa", 200, playlist(&[V1, V2, V3])),
        (
            "/videos",
            200,
            videos(&[(V1, "PT10M"), (V2, "PT30S"), (V3, "PT1H")]),
        ),
    ]
}

const CAPTURES: &str = "[periods.day]\nfolder = \"daily\"\nformat = \"YYYY-MM-DD\"\n\n[[capture]]\nname = \"tracker-intake\"\ntarget = \"daily\"\nmode = \"section\"\nsection = \"# Intake\"\nformat = \"- [ ] [{{vars.title}}]({{vars.url}}) — {{vars.source}}\"\n";
const SOURCES: &str = "[[youtube]]\nchannel = \"Chan A\"\nid = \"UCa\"\n";
const ON: &str = "[youtube]\nenabled = true\n";

fn world(files: &[(&str, &str)], routes: Routes, pipelines: &str, sources: &str) -> World {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "cv-wp7-{}-{}",
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
    let log = Arc::new(Mutex::new(Vec::new()));
    let base = rt.block_on(serve(routes, log.clone()));
    let files = BTreeMap::from([
        ("pipelines.toml".to_owned(), Some(pipelines.to_owned())),
        ("sources.toml".to_owned(), Some(sources.to_owned())),
        ("captures.toml".to_owned(), Some(CAPTURES.to_owned())),
    ]);
    let mut config = ConfigSet::new();
    config.reload(&files, &crate::steps::validate);
    World {
        vault: std::fs::canonicalize(&vault).unwrap(),
        _tmp: tmp,
        rt,
        index,
        writer,
        state: Arc::new(Mutex::new(state)),
        config,
        base,
        log,
        key: Some("TESTKEY"),
    }
}

fn step(src: &str) -> Step {
    let t: toml::Table = format!("type = \"track_youtube\"\n{src}").parse().unwrap();
    Step::from_table(t).unwrap()
}

fn action() -> Action {
    Action {
        id: "track-youtube".into(),
        label: "Track YouTube".into(),
        group: None,
        keys: vec![],
        params: vec![],
        needs: vec![],
        lane: LaneChoice::Auto,
        durable: true,
        steps: vec![],
        starters: vec![],
        resources: ResourceSpec::default(),
    }
}

fn run(w: &World) -> Result<StepOutcome, StepError> {
    let _rt = w.rt.enter();
    let _o = test_overrides(Overrides {
        bases: HashMap::from([("youtube-api".to_owned(), w.base.clone())]),
        secrets: Some(Arc::new(Keys(w.key))),
    });
    let action = action();
    let context = ActionContext::default();
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
        state: &w.state,
        writer: Some(w.writer.clone()),
        children: &NoChildren,
        written: Vec::new(),
    };
    execute(&step(""), &mut ctx)
}

fn daily(w: &World) -> String {
    let day = chrono::Local::now().format("%Y-%m-%d");
    std::fs::read_to_string(w.vault.join(format!("daily/{day}.md"))).unwrap_or_default()
}

fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

fn url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

fn outcome(w: &World, id: &str) -> Option<TrackerOutcome> {
    w.state
        .lock()
        .unwrap()
        .tracker_item(KIND, "UCa", id)
        .unwrap()
        .map(|r| r.outcome)
}

fn n(o: &StepOutcome, k: &str) -> Value {
    o.vars[k].clone()
}

#[test]
fn params_validate() {
    let c = ConfigSet::new();
    assert!(validate(&step(""), &c).is_ok());
    assert!(validate(&step("limit_per_source = 10"), &c).is_ok());
    assert!(validate(&step("limit_per_source = 0"), &c).is_err());
    assert!(validate(&step("limit_per_source = 51"), &c).is_err());
    assert!(validate(&step("nope = 1"), &c).is_err());
}

#[test]
fn first_run_seeds_from_metadata_and_crucible_intake_and_writes_nothing() {
    let w = world(
        &[
            (
                "_yt_metadata/chan-a/aaaaaaaaaa1.md",
                "---\nvideoId: x\n---\n",
            ),
            (
                "_crucible/orchestration/youtube/new-videos/2026-10-01.md",
                "- [ ] [t](https://www.youtube.com/watch?v=aaaaaaaaaa2)\n",
            ),
            (
                "_crucible/orchestration/ignored.md",
                "## Videos\n- aaaaaaaaaa3\n",
            ),
        ],
        std_routes(),
        ON,
        SOURCES,
    );
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "new"), json!(0));
    assert_eq!(n(&o, "lines"), json!(0));
    assert!(daily(&w).is_empty());
    assert!(!w.log.lock().unwrap().iter().any(|p| p.contains("/videos")));
}

#[test]
fn new_uploads_land_as_lines_with_metadata_and_rerun_is_a_noop() {
    let w = world(&[], std_routes(), ON, SOURCES);
    let o = run(&w).unwrap();
    // Dark-gap catch-up: an empty seen-set surfaces the whole feed window.
    assert_eq!(n(&o, "sources"), json!(1));
    assert_eq!(n(&o, "new"), json!(3));
    assert_eq!(n(&o, "lines"), json!(3));
    assert_eq!(n(&o, "failed"), json!(0));
    let d = daily(&w);
    assert!(d.contains("# Intake"));
    assert!(d.contains(&format!("- [ ] [Video {V1}]({}) — Chan A", url(V1))));
    // Oldest first.
    assert!(d.find(&url(V1)).unwrap() < d.find(&url(V3)).unwrap());
    assert!(w.vault.join("_yt_metadata/chan-a/aaaaaaaaaa1.md").exists());
    assert_eq!(outcome(&w, V1), Some(TrackerOutcome::Line));
    // 1 playlist + 1 videos batch.
    let day = chrono::Local::now().format("%Y-%m-%d").to_string();
    assert_eq!(
        w.state
            .lock()
            .unwrap()
            .api_usage("youtube-api", &day)
            .unwrap(),
        2
    );

    let o = run(&w).unwrap();
    assert_eq!(n(&o, "new"), json!(0));
    assert_eq!(n(&o, "lines"), json!(0));
    assert_eq!(daily(&w), d);
}

#[test]
fn interrupted_run_replays_each_line_exactly_once() {
    let w = world(&[], std_routes(), ON, SOURCES);
    hook::fail_after(Some(2));
    assert!(run(&w).is_err());
    hook::fail_after(None);
    let d = daily(&w);
    assert_eq!(count(&d, &url(V1)), 1);
    assert_eq!(count(&d, &url(V2)), 1);
    assert_eq!(count(&d, &url(V3)), 0);
    assert_eq!(outcome(&w, V1), Some(TrackerOutcome::Line));
    assert_eq!(outcome(&w, V2), None);

    let o = run(&w).unwrap();
    assert_eq!(n(&o, "lines"), json!(2));
    let d = daily(&w);
    for id in [V1, V2, V3] {
        assert_eq!(count(&d, &url(id)), 1, "{id}");
        assert_eq!(outcome(&w, id), Some(TrackerOutcome::Line));
    }
}

#[test]
fn url_already_in_daily_is_recorded_not_rewritten() {
    let day = chrono::Local::now().format("%Y-%m-%d").to_string();
    let path = format!("daily/{day}.md");
    let pre = format!("# Intake\n- [ ] [old]({}) — Chan A\n", url(V2));
    // A tracker row exists, so the run does not seed from the vault.
    let w = world(&[(path.as_str(), pre.as_str())], std_routes(), ON, SOURCES);
    w.state
        .lock()
        .unwrap()
        .insert_tracker_item(&cv_state::TrackerItem {
            source_kind: KIND.into(),
            source_key: "UCa".into(),
            item_id: "zzzzzzzzzzz".into(),
            title: String::new(),
            url: String::new(),
            first_seen_ms: 0,
            day: day.clone(),
            outcome: TrackerOutcome::Skipped,
            score: None,
            full_body: None,
        })
        .unwrap();
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "lines"), json!(3));
    let d = daily(&w);
    assert_eq!(count(&d, &url(V2)), 1);
    assert_eq!(count(&d, &url(V1)), 1);
    assert_eq!(outcome(&w, V2), Some(TrackerOutcome::Line));
}

#[test]
fn configured_short_is_ignored_without_a_line() {
    let p = "[youtube]\nenabled = true\n[ignore_shorts]\nchannels = [\"UCa\"]\n";
    let w = world(&[], std_routes(), p, SOURCES);
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "ignored"), json!(1));
    assert_eq!(n(&o, "lines"), json!(2));
    assert!(w.state.lock().unwrap().is_ignored(KIND, V2).unwrap());
    assert_eq!(outcome(&w, V2), Some(TrackerOutcome::Ignored));
    assert_eq!(count(&daily(&w), &url(V2)), 0);
    // By channel name too.
    let p = "[youtube]\nenabled = true\n[ignore_shorts]\nchannels = [\"chan a\"]\n";
    let w = world(&[], std_routes(), p, SOURCES);
    assert_eq!(n(&run(&w).unwrap(), "ignored"), json!(1));
}

const TWO: &str = "[[youtube]]\nchannel = \"Chan A\"\nid = \"UCa\"\n\n[[youtube]]\nchannel = \"Chan B\"\nid = \"UCb\"\n";

#[test]
fn all_channels_failing_defers_one_failing_counts() {
    let routes = vec![("playlistItems", 503, String::new())];
    let w = world(&[], routes, ON, TWO);
    match run(&w) {
        Err(StepError::Deferred { service, .. }) => {
            assert_eq!(service.as_deref(), Some("youtube-api"));
        }
        other => panic!("{other:?}"),
    }
    let mut routes = vec![("playlistId=UUb", 503, String::new())];
    routes.extend(std_routes());
    let w = world(&[], routes, ON, TWO);
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "sources"), json!(2));
    assert_eq!(n(&o, "failed"), json!(1));
    assert_eq!(n(&o, "lines"), json!(3));
}

#[test]
fn missing_key_fails_no_api_key() {
    let mut w = world(&[], std_routes(), ON, SOURCES);
    w.key = None;
    match run(&w) {
        Err(StepError::Failed { reason, .. }) => {
            assert_eq!(reason, Some(FailureReason::NoApiKey));
        }
        other => panic!("{other:?}"),
    }
    assert!(w.log.lock().unwrap().is_empty());
}

#[test]
fn skip_ignore_and_non_uc_rows_are_excluded() {
    let s = format!(
        "{SOURCES}\n[[youtube]]\nchannel = \"S\"\nid = \"UCs\"\npriority = \"skip\"\n\n[[youtube]]\nchannel = \"I\"\nid = \"UCi\"\npriority = \"ignore\"\n\n[[youtube]]\nchannel = \"H\"\nid = \"HCh\"\n"
    );
    let w = world(&[], std_routes(), ON, &s);
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "sources"), json!(1));
    let log = w.log.lock().unwrap();
    assert_eq!(
        log.iter().filter(|p| p.contains("playlistItems")).count(),
        1
    );
}

#[test]
fn disabled_pipeline_does_nothing() {
    let w = world(&[], std_routes(), "", SOURCES);
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "disabled"), json!(true));
    assert!(w.log.lock().unwrap().is_empty());
}
