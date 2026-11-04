//! WP-7 `track_youtube` tests: a synthetic temp vault, a temp `state.db` and a local fake Data
//! API (never the network, never the live vault, never env vars).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, ResourceSpec, Step};
use cv_protocol::TrackerOutcome;
use cv_providers::secrets::SecretSource;
use cv_state::FailureReason;
use cv_testkit::{MockHttp, Reply};
use cv_vault::test_support::LiveFixture;
use serde_json::{Value, json};

use super::super::hook;
use super::KIND;
use crate::steps::testkit::{ActionBuilder, StepEnv};
use crate::steps::web::{Overrides, test_overrides};
use crate::steps::{StepError, StepOutcome};
use crate::steps::{execute, validate};

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

type Routes = Vec<(&'static str, u16, String)>;

/// Route substring → (status, body); logs every request target.
fn serve(routes: Routes, log: Arc<Mutex<Vec<String>>>) -> MockHttp {
    MockHttp::start(move |req| {
        log.lock().unwrap().push(req.target.clone());
        routes
            .iter()
            .find(|(p, _, _)| req.target.contains(p))
            .map_or_else(
                || Reply::new(404, Vec::new()),
                |(_, s, b)| Reply::new(*s, b.clone()),
            )
    })
}

struct World {
    env: StepEnv,
    rt: tokio::runtime::Runtime,
    vault: PathBuf,
    base: String,
    log: Arc<Mutex<Vec<String>>>,
    key: Option<&'static str>,
    /// Every mock started for this world (kept alive with it).
    servers: Vec<MockHttp>,
}

impl std::ops::Deref for World {
    type Target = StepEnv;
    fn deref(&self) -> &StepEnv {
        &self.env
    }
}

impl std::ops::DerefMut for World {
    fn deref_mut(&mut self) -> &mut StepEnv {
        &mut self.env
    }
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
    let fx = LiveFixture::new(files.iter().copied());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let http = serve(routes, log.clone());
    let base = http.url();
    let files = BTreeMap::from([
        ("pipelines.toml".to_owned(), Some(pipelines.to_owned())),
        ("sources.toml".to_owned(), Some(sources.to_owned())),
        ("captures.toml".to_owned(), Some(CAPTURES.to_owned())),
    ]);
    let mut config = ConfigSet::new();
    config.reload(&files, &crate::steps::validate);
    World {
        vault: fx.vault.clone(),
        env: StepEnv::new(fx, config, action()),
        rt,
        base,
        log,
        key: Some("TESTKEY"),
        servers: vec![http],
    }
}

fn step(src: &str) -> Step {
    let t: toml::Table = format!("type = \"track_youtube\"\n{src}").parse().unwrap();
    Step::from_table(t).unwrap()
}

fn action() -> Action {
    ActionBuilder::new("track-youtube", vec![], ResourceSpec::default()).build()
}

fn run(w: &World) -> Result<StepOutcome, StepError> {
    let _rt = w.rt.enter();
    let _o = test_overrides(Overrides {
        bases: HashMap::from([("youtube-api".to_owned(), w.base.clone())]),
        secrets: Some(Arc::new(Keys(w.key))),
    });
    execute(&step(""), &mut w.ctx())
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
    let meta = std::fs::read_to_string(w.vault.join("_yt_metadata/chan-a/aaaaaaaaaa1.md")).unwrap();
    assert!(
        meta.contains("channelTitle: \"Chan A\"\npublication: \"Chan A\"\n"),
        "{meta}"
    );
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
fn view_intake_mode_records_items_and_writes_no_daily_line() {
    let w = world(
        &[],
        std_routes(),
        &format!("{ON}\n[intake]\nmode = \"view\"\n"),
        SOURCES,
    );
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "new"), json!(3));
    assert_eq!(n(&o, "lines"), json!(3), "recorded items still count");
    assert!(daily(&w).is_empty(), "no daily note write in view mode");
    for id in [V1, V2, V3] {
        assert_eq!(outcome(&w, id), Some(TrackerOutcome::Line));
    }
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "lines"), json!(0), "dedupe unchanged");
    assert!(daily(&w).is_empty());
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
            note_path: None,
            source_name: None,
            duration_s: None,
            word_count: None,
            access: None,
            complete: None,
            full_word_count: None,
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
fn configured_short_is_ignored_with_a_cancelled_line() {
    let p = "[youtube]\nenabled = true\n[ignore_shorts]\nchannels = [\"UCa\"]\n";
    let w = world(&[], std_routes(), p, SOURCES);
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "ignored"), json!(1));
    assert_eq!(n(&o, "lines"), json!(2));
    assert!(w.state.lock().unwrap().is_ignored(KIND, V2).unwrap());
    assert_eq!(outcome(&w, V2), Some(TrackerOutcome::Ignored));
    // Round-8 WP-1: `"lines"` mode writes the short as a cancelled `- [-]` line.
    assert_eq!(count(&daily(&w), &url(V2)), 1);
    assert_eq!(count(&daily(&w), "- [-] "), 1);
    // By channel name too.
    let p = "[youtube]\nenabled = true\n[ignore_shorts]\nchannels = [\"chan a\"]\n";
    let w = world(&[], std_routes(), p, SOURCES);
    assert_eq!(n(&run(&w).unwrap(), "ignored"), json!(1));
}

fn row_of(w: &World, key: &str, id: &str) -> cv_state::TrackerItem {
    w.state
        .lock()
        .unwrap()
        .tracker_item(KIND, key, id)
        .unwrap()
        .unwrap()
}

#[test]
fn recorded_rows_carry_duration_and_source_name_incl_ignored_shorts() {
    let p = "[youtube]\nenabled = true\n[ignore_shorts]\nchannels = [\"UCa\"]\n";
    let w = world(&[], std_routes(), p, SOURCES);
    run(&w).unwrap();
    for (id, secs) in [(V1, 600), (V2, 30), (V3, 3600)] {
        let r = row_of(&w, "UCa", id);
        assert_eq!(r.duration_s, Some(secs), "{id}");
        assert_eq!(r.source_name.as_deref(), Some("Chan A"));
        assert_eq!(r.word_count, None);
    }
    assert_eq!(row_of(&w, "UCa", V2).outcome, TrackerOutcome::Ignored);
}

#[test]
fn name_self_heal_fills_null_rows_of_configured_source_without_fresh_items() {
    let w = world(&[], std_routes(), ON, SOURCES);
    let mk = |key: &str, id: &str, name: Option<&str>| cv_state::TrackerItem {
        source_kind: KIND.into(),
        source_key: key.into(),
        item_id: id.into(),
        title: String::new(),
        url: String::new(),
        first_seen_ms: 0,
        day: "2026-10-17".into(),
        outcome: TrackerOutcome::Line,
        score: None,
        full_body: None,
        note_path: None,
        source_name: name.map(str::to_owned),
        duration_s: None,
        word_count: None,
        access: None,
        complete: None,
        full_word_count: None,
    };
    {
        let mut s = w.state.lock().unwrap();
        // Every playlist id is already recorded: the run has no fresh items.
        for it in [
            mk("UCa", V1, None),
            mk("UCa", V2, None),
            mk("UCa", V3, Some("Old name")),
            mk("UCb", "bbbbbbbbbb1", None),
        ] {
            assert!(s.insert_tracker_item(&it).unwrap());
        }
    }
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "new"), json!(0), "heal never counts as an item");
    assert_eq!(n(&o, "lines"), json!(0));
    assert_eq!(row_of(&w, "UCa", V1).source_name.as_deref(), Some("Chan A"));
    assert_eq!(row_of(&w, "UCa", V2).source_name.as_deref(), Some("Chan A"));
    assert_eq!(
        row_of(&w, "UCa", V3).source_name.as_deref(),
        Some("Old name"),
        "non-null names untouched"
    );
    assert_eq!(
        row_of(&w, "UCb", "bbbbbbbbbb1").source_name,
        None,
        "other sources untouched"
    );
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

// ---------------------------------------------------------------- WP-10 scorer wiring

/// A scorer endpoint that reads the whole POST body (logged) and answers `status`/`body`.
fn serve_scorer(status: u16, body: &'static str, log: Arc<Mutex<Vec<String>>>) -> MockHttp {
    MockHttp::start(move |req| {
        log.lock().unwrap().push(req.body_str());
        Reply::new(status, body)
    })
}

const SCORED_CAPTURES: &str = "[periods.day]\nfolder = \"daily\"\nformat = \"YYYY-MM-DD\"\n\n[[capture]]\nname = \"tracker-intake\"\ntarget = \"daily\"\nmode = \"section\"\nsection = \"# Intake\"\nformat = \"- [ ] [{{vars.title}}]({{vars.url}}) — {{vars.source}} ({{vars.score}})\"\n";

/// One V1-only run with `[scorer]` pointed at `url` (`None` = a scorer server answering
/// `status`/`body`); returns the world and the logged scorer request bodies.
fn scored_run(status: u16, body: &'static str, url: Option<&str>) -> (World, Vec<String>) {
    let routes = vec![
        ("playlistId=UUa", 200, playlist(&[V1])),
        ("/videos", 200, videos(&[(V1, "PT10M")])),
    ];
    let mut w = world(&[], routes, ON, SOURCES);
    let log = Arc::new(Mutex::new(Vec::new()));
    let scorer = match url {
        Some(u) => u.to_owned(),
        None => {
            let m = serve_scorer(status, body, log.clone());
            let u = format!("{}/v1/systemone", m.url());
            w.servers.push(m);
            u
        }
    };
    let pipelines = format!("{ON}\n[scorer]\nurl = \"{scorer}\"\nmodel = \"local/scorer\"\n");
    let files = BTreeMap::from([
        ("pipelines.toml".to_owned(), Some(pipelines)),
        ("sources.toml".to_owned(), Some(SOURCES.to_owned())),
        ("captures.toml".to_owned(), Some(SCORED_CAPTURES.to_owned())),
    ]);
    w.config.reload(&files, &crate::steps::validate);
    let o = run(&w).unwrap();
    assert_eq!(
        n(&o, "lines"),
        json!(1),
        "intake never blocks on the scorer"
    );
    let bodies = log.lock().unwrap().clone();
    (w, bodies)
}

fn score_of(w: &World, id: &str) -> Option<f64> {
    w.state
        .lock()
        .unwrap()
        .tracker_item(KIND, "UCa", id)
        .unwrap()
        .and_then(|r| r.score)
}

#[test]
fn stub_scorer_leaves_items_unscored() {
    let w = world(&[], std_routes(), ON, SOURCES);
    run(&w).unwrap();
    assert_eq!(score_of(&w, V1), None);
    assert!(daily(&w).contains(&format!("({}) — Chan A\n", url(V1))));
}

#[test]
fn systemone_score_is_stored_and_rendered() {
    let (w, bodies) = scored_run(200, r#"{"score":3}"#, None);
    assert_eq!(score_of(&w, V1), Some(3.0));
    assert!(daily(&w).contains("— Chan A (3)"), "{}", daily(&w));
    assert_eq!(bodies.len(), 1);
    let req: Value = serde_json::from_str(&bodies[0]).unwrap();
    assert_eq!(req["model"], "local/scorer");
    assert!(
        req["state"]
            .as_str()
            .unwrap()
            .contains(&format!("title: Video {V1}"))
    );
    assert!(req["questions"]["importance"].is_string());
}

#[test]
fn scorer_failures_never_block_intake() {
    for (status, body, url) in [
        (500, "boom", None),
        (200, "not json", None),
        (200, "", Some("http://127.0.0.1:1/v1/systemone")),
    ] {
        let (w, _) = scored_run(status, body, url);
        assert_eq!(outcome(&w, V1), Some(TrackerOutcome::Line));
        assert_eq!(score_of(&w, V1), None);
        assert!(daily(&w).contains("— Chan A ()"), "{}", daily(&w));
    }
}

fn legacy_row(id: &str, outcome: TrackerOutcome) -> cv_state::TrackerItem {
    cv_state::TrackerItem {
        source_kind: KIND.into(),
        source_key: "UCa".into(),
        item_id: id.into(),
        title: String::new(),
        url: String::new(),
        first_seen_ms: 0,
        day: "2026-10-16".into(),
        outcome,
        score: None,
        full_body: None,
        note_path: None,
        source_name: None,
        duration_s: None,
        word_count: None,
        access: None,
        complete: None,
        full_word_count: None,
    }
}

fn videos_calls(w: &World) -> usize {
    w.log
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.contains("/videos"))
        .count()
}

const GONE: &str = "gonegone001";
const SKIP: &str = "skipskip001";

#[test]
fn duration_backfill_fills_null_rows_marks_omitted_and_never_requeries() {
    let w = world(&[], std_routes(), ON, SOURCES);
    {
        let mut s = w.state.lock().unwrap();
        for it in [
            legacy_row(V1, TrackerOutcome::Line),
            legacy_row(V2, TrackerOutcome::Line),
            legacy_row(V3, TrackerOutcome::Note),
            legacy_row(GONE, TrackerOutcome::Line),
            legacy_row(SKIP, TrackerOutcome::Skipped),
        ] {
            assert!(s.insert_tracker_item(&it).unwrap());
        }
    }
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "new"), json!(0), "backfill never counts as an item");
    assert_eq!(n(&o, "lines"), json!(0));
    assert_eq!(n(&o, "failed"), json!(0));
    assert_eq!(row_of(&w, "UCa", V1).duration_s, Some(600));
    assert_eq!(row_of(&w, "UCa", V2).duration_s, Some(30));
    assert_eq!(row_of(&w, "UCa", V3).duration_s, Some(3600));
    assert_eq!(
        row_of(&w, "UCa", GONE).duration_s,
        Some(0),
        "omitted id marked"
    );
    assert_eq!(
        row_of(&w, "UCa", SKIP).duration_s,
        None,
        "skipped rows untouched"
    );
    for id in [V1, V2, V3, GONE] {
        assert_eq!(row_of(&w, "UCa", id).word_count, None);
    }
    let calls = videos_calls(&w);
    assert_eq!(calls, 1, "one batch");
    run(&w).unwrap();
    assert_eq!(videos_calls(&w), calls, "nothing re-queried next run");
}

#[test]
fn failed_duration_backfill_leaves_rows_null_and_a_later_run_fills_them() {
    let routes = vec![
        ("playlistId=UUa", 200, playlist(&[V1, V2, V3])),
        ("/videos", 503, String::new()),
    ];
    let mut w = world(&[], routes, ON, SOURCES);
    {
        let mut s = w.state.lock().unwrap();
        for id in [V1, V2, V3] {
            assert!(
                s.insert_tracker_item(&legacy_row(id, TrackerOutcome::Line))
                    .unwrap()
            );
        }
    }
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "failed"), json!(0), "a backfill failure never fails");
    assert_eq!(n(&o, "new"), json!(0));
    assert_eq!(n(&o, "lines"), json!(0));
    assert_eq!(row_of(&w, "UCa", V1).duration_s, None, "retryable");
    let m = serve(std_routes(), w.log.clone());
    w.base = m.url();
    w.servers.push(m);
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "new"), json!(0));
    assert_eq!(row_of(&w, "UCa", V1).duration_s, Some(600));
    assert_eq!(row_of(&w, "UCa", V3).duration_s, Some(3600));
}

// Round-8 WP-1: ignored shorts are cancelled intakes; the retroactive pass.

const SHORTS_UCA: &str =
    "[youtube]\nenabled = true\n[ignore_shorts]\nchannels = [\"UCa\"]\nmax_seconds = 60\n";

fn status_of(w: &World, key: &str, id: &str) -> Option<char> {
    w.state
        .lock()
        .unwrap()
        .tracker_item_status(KIND, key, id)
        .unwrap()
        .unwrap()
}

#[test]
fn view_mode_ignored_short_is_a_cancelled_intake_in_the_daily_intake_rows() {
    let p = format!("{SHORTS_UCA}[intake]\nmode = \"view\"\n");
    let w = world(&[], std_routes(), &p, SOURCES);
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "ignored"), json!(1));
    assert!(daily(&w).is_empty(), "view mode writes no text");
    assert_eq!(outcome(&w, V2), Some(TrackerOutcome::Ignored));
    assert_eq!(status_of(&w, "UCa", V2), Some('-'));
    assert_eq!(status_of(&w, "UCa", V1), None);
    assert!(w.state.lock().unwrap().is_ignored(KIND, V2).unwrap());
    // The daily-intake filter `outcome == "line" || "note" || "ignored"` over the day's rows.
    let day = chrono::Local::now().format("%Y-%m-%d").to_string();
    let rows = w
        .state
        .lock()
        .unwrap()
        .tracker_items_for_source(KIND, "UCa", 100)
        .unwrap();
    let shown: Vec<_> = rows
        .iter()
        .filter(|r| r.day == day)
        .filter(|r| {
            matches!(
                r.outcome,
                TrackerOutcome::Line | TrackerOutcome::Note | TrackerOutcome::Ignored
            )
        })
        .map(|r| r.item_id.as_str())
        .collect();
    assert!(shown.contains(&V2), "{shown:?}");
}

#[test]
fn lines_mode_ignored_short_writes_a_cancelled_line() {
    let w = world(&[], std_routes(), SHORTS_UCA, SOURCES);
    run(&w).unwrap();
    let d = daily(&w);
    let line = d.lines().find(|l| l.contains(&url(V2))).expect("V2 line");
    assert!(line.starts_with("- [-] "), "{line}");
    let v1 = d.lines().find(|l| l.contains(&url(V1))).expect("V1 line");
    assert!(v1.starts_with("- [ ] "), "{v1}");
    assert_eq!(status_of(&w, "UCa", V2), Some('-'));
    assert_eq!(outcome(&w, V2), Some(TrackerOutcome::Ignored));
}

#[test]
fn retroactive_pass_cancels_only_eligible_rows_once() {
    // UCz stays a configured (skipped) source: the orphans pass must not cancel its row.
    let sources =
        format!("{SOURCES}\n[[youtube]]\nchannel = \"Z\"\nid = \"UCz\"\npriority = \"skip\"\n");
    let w = world(&[], std_routes(), SHORTS_UCA, &sources);
    let mk = |key: &str, id: &str, dur: Option<i64>| cv_state::TrackerItem {
        source_key: key.into(),
        duration_s: dur,
        ..legacy_row(id, TrackerOutcome::Line)
    };
    {
        let mut s = w.state.lock().unwrap();
        for it in [
            mk("UCa", "eligible001", Some(30)),
            mk("UCa", "zero0000001", Some(0)),
            mk("UCa", "unknown0001", None), // the fake API omits it: the backfill marks it 0
            mk("UCa", "done0000001", Some(30)),
            mk("UCa", "space000001", Some(30)),
            mk("UCa", "reopened001", Some(30)),
            mk("UCz", "otherchan01", Some(30)),
            mk("UCa", "longvideo01", Some(3000)),
        ] {
            assert!(s.insert_tracker_item(&it).unwrap());
        }
        s.set_tracker_item_status(KIND, "UCa", "done0000001", Some('x'))
            .unwrap();
        s.set_tracker_item_status(KIND, "UCa", "space000001", Some(' '))
            .unwrap();
        s.insert_ignored_id(KIND, "reopened001", "manual", 1)
            .unwrap();
    }
    run(&w).unwrap();
    assert_eq!(
        row_of(&w, "UCa", "eligible001").outcome,
        TrackerOutcome::Ignored
    );
    assert_eq!(status_of(&w, "UCa", "eligible001"), Some('-'));
    assert!(
        w.state
            .lock()
            .unwrap()
            .is_ignored(KIND, "eligible001")
            .unwrap()
    );
    for (key, id, st) in [
        ("UCa", "zero0000001", None),
        ("UCa", "unknown0001", None),
        ("UCa", "done0000001", Some('x')),
        ("UCa", "space000001", Some(' ')),
        ("UCa", "reopened001", None),
        ("UCz", "otherchan01", None),
        ("UCa", "longvideo01", None),
    ] {
        assert_eq!(row_of(&w, key, id).outcome, TrackerOutcome::Line, "{id}");
        assert_eq!(status_of(&w, key, id), st, "{id}");
    }
    assert_eq!(row_of(&w, "UCa", "unknown0001").duration_s, Some(0));
    // A user reopen sticks: the ignored_ids row keeps the row ineligible; re-runs are no-ops.
    w.state
        .lock()
        .unwrap()
        .set_tracker_item_status(KIND, "UCa", "eligible001", None)
        .unwrap();
    let left = w
        .state
        .lock()
        .unwrap()
        .tracker_items_cancel_candidates(KIND, "UCa", 60, 500)
        .unwrap();
    assert!(left.is_empty(), "listed channel: none left {left:?}");
    run(&w).unwrap();
    assert_eq!(status_of(&w, "UCa", "eligible001"), None, "reopen sticks");
    assert_eq!(
        row_of(&w, "UCa", "eligible001").outcome,
        TrackerOutcome::Ignored
    );
}

#[test]
fn metadata_note_publication_prefers_the_rows_short() {
    let sources = "[[youtube]]\nchannel = \"Chan A\"\nid = \"UCa\"\nshort = \"CA\"\n";
    let w = world(&[], std_routes(), ON, sources);
    run(&w).unwrap();
    let meta = std::fs::read_to_string(w.vault.join("_yt_metadata/chan-a/aaaaaaaaaa1.md")).unwrap();
    assert!(meta.contains("publication: \"CA\"\n"), "{meta}");
}
