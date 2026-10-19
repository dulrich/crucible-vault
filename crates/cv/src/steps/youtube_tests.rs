//! WP-4 `yt_metadata` / `yt_channel_enrich` tests: a synthetic temp vault, a temp `state.db` and
//! a local fake Data API (never the network, never the live vault, never env vars).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_protocol::ActionContext;
use cv_providers::secrets::SecretSource;
use cv_state::FailureReason;
use cv_testkit::{MockHttp, Reply};
use cv_vault::test_support::LiveFixture;
use serde_json::{Map, Value, json};

use crate::steps::testkit::StepEnv;
use crate::steps::web::{Overrides, create_or_found, test_overrides};
use crate::steps::{DeferCause, StepError, StepOutcome};
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

struct World {
    env: StepEnv,
    rt: tokio::runtime::Runtime,
    vault: PathBuf,
    base: String,
    http: MockHttp,
    key: Option<&'static str>,
}

impl std::ops::Deref for World {
    type Target = StepEnv;
    fn deref(&self) -> &StepEnv {
        &self.env
    }
}

const VIDEO: &str = r#"{"items":[{"id":"dQw4w9WgXcQ","snippet":{"title":"Synthetic","channelId":"UCsynth","channelTitle":"Synth Channel","publishedAt":"2020-01-01T00:00:00Z","categoryId":"10","liveBroadcastContent":"none"},"contentDetails":{"duration":"PT30S"},"statistics":{"viewCount":"5"}}]}"#;
const CHANNEL: &str = r#"{"items":[{"id":"UCsynth","snippet":{"title":"Synth Channel","description":"d","publishedAt":"2019-01-01T00:00:00Z"},"statistics":{"videoCount":"3"}}]}"#;

fn world(
    files: &[(&str, &str)],
    routes: Vec<(&'static str, u16, String)>,
    pipelines: &str,
) -> World {
    let fx = LiveFixture::new(files.iter().copied());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    // Route prefix → (status, body); anything else is an empty 500.
    let http = MockHttp::start(move |req| {
        routes
            .iter()
            .find(|(p, _, _)| req.target.starts_with(p))
            .map_or_else(
                || Reply::new(500, Vec::new()),
                |(_, s, b)| Reply::new(*s, b.clone()),
            )
    });
    let base = http.url();
    let files = BTreeMap::from([
        ("pipelines.toml".to_owned(), Some(pipelines.to_owned())),
        (
            "sources.toml".to_owned(),
            Some("[[youtube]]\nchannel = \"Tracked Name\"\nid = \"UCtracked\"\n".to_owned()),
        ),
    ]);
    let mut config = ConfigSet::new();
    config.reload(&files, &crate::steps::validate);
    World {
        vault: fx.vault.clone(),
        env: StepEnv::new(fx, config, action()),
        rt,
        base,
        http,
        key: Some("TESTKEY"),
    }
}

fn step(kind: &str, src: &str) -> Step {
    let t: toml::Table = format!("type = \"{kind}\"\n{src}").parse().unwrap();
    Step::from_table(t).unwrap()
}

fn action() -> Action {
    Action {
        id: "yt-metadata".into(),
        label: "YT".into(),
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

fn run(
    w: &World,
    s: &Step,
    active: Option<&str>,
    vars: &Map<String, Value>,
) -> Result<StepOutcome, StepError> {
    let _rt = w.rt.enter();
    let _o = test_overrides(Overrides {
        bases: HashMap::from([("youtube-api".to_owned(), w.base.clone())]),
        secrets: Some(Arc::new(Keys(w.key))),
    });
    let context = ActionContext {
        active_path: active.map(str::to_owned),
        ..ActionContext::default()
    };
    let mut ctx = w.ctx();
    ctx.context = &context;
    ctx.vars = vars;
    execute(s, &mut ctx)
}

fn meta(w: &World, note: &str) -> Result<StepOutcome, StepError> {
    run(w, &step("yt_metadata", ""), Some(note), &Map::new())
}

fn read(w: &World, rel: &str) -> String {
    std::fs::read_to_string(w.vault.join(rel)).unwrap()
}

const NOTE: &str = "---\ntitle: n\nyt-video-id: dQw4w9WgXcQ\nsource: x\n---\nBody\n";

#[test]
fn params_validate() {
    let c = ConfigSet::new();
    assert!(validate(&step("yt_metadata", ""), &c).is_ok());
    assert!(validate(&step("yt_metadata", "video_id = \"{vars.id}\""), &c).is_ok());
    assert!(validate(&step("yt_metadata", "video_id = \"{bogus}\""), &c).is_err());
    assert!(validate(&step("yt_metadata", "nope = 1"), &c).is_err());
    assert!(
        validate(
            &step(
                "yt_channel_enrich",
                "force = true\nmax_age_days = 7\nchannel_id = \"{vars.channel_id}\""
            ),
            &c
        )
        .is_ok()
    );
    assert!(validate(&step("yt_channel_enrich", "max_age_days = 0"), &c).is_err());
}

#[test]
fn creates_links_after_video_id_counts_usage_then_rerun_is_a_noop() {
    let w = world(
        &[("n/a.md", NOTE)],
        vec![("/videos", 200, VIDEO.into())],
        "",
    );
    let o = meta(&w, "n/a.md").unwrap();
    let path = "_yt_metadata/synth-channel/dQw4w9WgXcQ.md";
    assert_eq!(o.vars["metadata_path"], json!(path));
    assert_eq!(o.vars["created"], json!(true));
    assert_eq!(o.vars["linked"], json!(true));
    assert_eq!(o.vars["channel_id"], json!("UCsynth"));
    let m = read(&w, path);
    assert!(
        m.starts_with("---\nvideoId: dQw4w9WgXcQ\ntitle: \"Synthetic\"\n"),
        "{m}"
    );
    assert!(m.contains("duration_seconds: 30\n") && m.contains("\n---\n# Synthetic\n"));
    assert_eq!(
        read(&w, "n/a.md"),
        "---\ntitle: n\nyt-video-id: dQw4w9WgXcQ\nyt-metadata:\n  - \"[[_yt_metadata/synth-channel/dQw4w9WgXcQ]]\"\nsource: x\n---\nBody\n"
    );
    let day = crate::steps::web::local_day();
    assert_eq!(
        w.state
            .lock()
            .unwrap()
            .api_usage("youtube-api", &day)
            .unwrap(),
        1
    );
    // Re-run: already linked to a live note → no API call, no write.
    let before = read(&w, "n/a.md");
    let o = meta(&w, "n/a.md").unwrap();
    assert_eq!(o.vars["linked"], json!(false));
    assert!(o.output_paths.is_empty());
    assert_eq!(w.http.hits(), 1);
    assert_eq!(read(&w, "n/a.md"), before);
}

#[test]
fn second_note_finds_the_same_note_and_exists_counts_as_found() {
    let b = "---\nsource: https://youtu.be/dQw4w9WgXcQ\nyt-metadata: \"[[other/x]]\"\n---\n";
    let w = world(
        &[("n/a.md", NOTE), ("n/b.md", b)],
        vec![("/videos", 200, VIDEO.into())],
        "",
    );
    meta(&w, "n/a.md").unwrap();
    let o = meta(&w, "n/b.md").unwrap();
    assert_eq!(o.vars["created"], json!(false));
    assert_eq!(w.http.hits(), 1, "found, not refetched");
    // Scalar entry [0] is kept first (append-only).
    assert!(read(&w, "n/b.md").contains(
        "yt-metadata:\n  - \"[[other/x]]\"\n  - \"[[_yt_metadata/synth-channel/dQw4w9WgXcQ]]\"\n"
    ));
    // The concurrent-create invariant: the writer's Exists → found, one note.
    let _rt = w.rt.enter();
    let mut ctx = w.ctx();
    let p = "_yt_metadata/synth-channel/dQw4w9WgXcQ.md";
    assert!(!create_or_found(&mut ctx, p, b"other".to_vec()).unwrap());
    assert!(ctx.written.is_empty());
    assert!(read(&w, p).starts_with("---\nvideoId:"));
}

#[test]
fn empty_items_and_404_tombstone_settle_done() {
    for (status, body) in [(200u16, r#"{"items":[]}"#), (404, "")] {
        let w = world(
            &[("n/a.md", NOTE)],
            vec![("/videos", status, body.into())],
            "",
        );
        let o = meta(&w, "n/a.md").unwrap();
        let p = "_yt_metadata/_unavailable/dQw4w9WgXcQ.md";
        assert_eq!(o.vars["tombstone"], json!(true), "{status}");
        assert_eq!(o.vars["created"], json!(false));
        assert!(read(&w, p).contains("state: unavailable\n"));
        assert!(read(&w, "n/a.md").contains("[[_yt_metadata/_unavailable/dQw4w9WgXcQ]]"));
        // A later run finds the tombstone; no refetch.
        let n = w.http.hits();
        std::fs::write(w.vault.join("n/c.md"), NOTE).unwrap();
        let o = meta(&w, "n/c.md").unwrap();
        assert_eq!(o.vars["tombstone"], json!(true));
        assert_eq!(w.http.hits(), n);
    }
}

#[test]
fn missing_key_fails_no_api_key_and_quota_defers() {
    let mut w = world(
        &[("n/a.md", NOTE)],
        vec![(
            "/videos",
            403,
            r#"{"error":{"errors":[{"reason":"quotaExceeded"}]}}"#.into(),
        )],
        "",
    );
    w.key = None;
    match meta(&w, "n/a.md").unwrap_err() {
        StepError::Failed { reason, .. } => assert_eq!(reason, Some(FailureReason::NoApiKey)),
        e => panic!("{e:?}"),
    }
    assert_eq!(w.http.hits(), 0);
    w.key = Some("K");
    match meta(&w, "n/a.md").unwrap_err() {
        StepError::Deferred { cause, service, .. } => {
            assert_eq!(cause, DeferCause::Quota);
            assert_eq!(service.as_deref(), Some("youtube-api"));
        }
        e => panic!("{e:?}"),
    }
}

#[test]
fn tracked_channel_uses_sources_name_folder() {
    let body = VIDEO.replace("UCsynth", "UCtracked");
    let w = world(&[("n/a.md", NOTE)], vec![("/videos", 200, body)], "");
    let o = meta(&w, "n/a.md").unwrap();
    assert_eq!(
        o.vars["metadata_path"],
        json!("_yt_metadata/tracked-name/dQw4w9WgXcQ.md")
    );
}

#[test]
fn channel_enrich_gates_skips_fresh_and_refreshes_stale() {
    let on = "[youtube]\nchannel_enrich = true\n";
    let created = Map::from_iter([
        ("created".to_owned(), json!(true)),
        ("channel_id".to_owned(), json!("UCsynth")),
    ]);
    let not_created = Map::from_iter([
        ("created".to_owned(), json!(false)),
        ("channel_id".to_owned(), json!("UCsynth")),
    ]);
    let s = step("yt_channel_enrich", "");
    // Off by default; on but not created → no-op.
    let w = world(&[], vec![("/channels", 200, CHANNEL.into())], "");
    assert_eq!(
        run(&w, &s, None, &created).unwrap().vars["enriched"],
        json!(false)
    );
    let w = world(&[], vec![("/channels", 200, CHANNEL.into())], on);
    assert_eq!(
        run(&w, &s, None, &not_created).unwrap().vars["enriched"],
        json!(false)
    );
    assert_eq!(w.http.hits(), 0);
    // Created → writes about.md.
    let o = run(&w, &s, None, &created).unwrap();
    assert_eq!(o.vars["enriched"], json!(true));
    let about = "_yt_metadata/synth-channel/about.md";
    assert!(read(&w, about).starts_with("---\nchannelId: UCsynth\ntitle: \"Synth Channel\"\n"));
    // Fresh → skipped, no call.
    let o = run(&w, &s, None, &created).unwrap();
    assert_eq!(o.vars["enriched"], json!(false));
    assert_eq!(w.http.hits(), 1);
    // Stale (found by channelId under another folder) → refreshed in place.
    std::fs::remove_file(w.vault.join(about)).unwrap();
    std::fs::create_dir_all(w.vault.join("_yt_metadata/old")).unwrap();
    std::fs::write(
        w.vault.join("_yt_metadata/old/about.md"),
        "---\nchannelId: UCsynth\nfetched_at: 2000-01-01T00:00:00.000Z\n---\n",
    )
    .unwrap();
    let o = run(&w, &s, None, &created).unwrap();
    assert_eq!(o.vars["about_path"], json!("_yt_metadata/old/about.md"));
    assert!(read(&w, "_yt_metadata/old/about.md").contains("videoCount: 3\n"));
}

/// SR7: the source note is linked through `modify` after the API fetch — a concurrent edit
/// between its read and the CAS write is kept and the link re-applied on top.
#[test]
fn concurrent_edit_of_the_source_note_is_kept_and_link_reapplied() {
    let w = world(
        &[("n/a.md", NOTE)],
        vec![("/videos", 200, VIDEO.into())],
        "",
    );
    crate::steps::race::inject(&w.vault, "n/a.md", 1, |t| format!("{t}racer\n"));
    let o = meta(&w, "n/a.md").unwrap();
    assert_eq!(o.vars["linked"], json!(true));
    assert_eq!(
        read(&w, "n/a.md"),
        "---\ntitle: n\nyt-video-id: dQw4w9WgXcQ\nyt-metadata:\n  - \"[[_yt_metadata/synth-channel/dQw4w9WgXcQ]]\"\nsource: x\n---\nBody\nracer\n"
    );
}

/// SR7: a race on every attempt defers (`Wait`), never fails, never clobbers.
#[test]
fn persistent_race_on_the_source_note_defers() {
    let w = world(
        &[("n/a.md", NOTE)],
        vec![("/videos", 200, VIDEO.into())],
        "",
    );
    crate::steps::race::inject(&w.vault, "n/a.md", crate::steps::MODIFY_ATTEMPTS, |t| {
        format!("{t}x\n")
    });
    assert_eq!(meta(&w, "n/a.md"), Err(StepError::conflict()));
    assert_eq!(read(&w, "n/a.md"), format!("{NOTE}x\nx\nx\n"));
}

#[test]
fn non_utf8_note_is_read_lossily_like_x() {
    // WP-R11 (SM7): youtube used to fail "note is not UTF-8"; already linked → done, no fetch.
    let live = "_yt_metadata/synth-channel/dQw4w9WgXcQ.md";
    let w = world(&[(live, "---\nvideoId: dQw4w9WgXcQ\n---\n")], vec![], "");
    let mut bytes = format!(
        "---\nyt-video-id: dQw4w9WgXcQ\nyt-metadata: \"[[{}]]\"\n---\n",
        &live[..live.len() - 3]
    )
    .into_bytes();
    bytes.push(0xff);
    std::fs::create_dir_all(w.vault.join("n")).unwrap();
    std::fs::write(w.vault.join("n/a.md"), &bytes).unwrap();
    let o = meta(&w, "n/a.md").unwrap();
    assert_eq!(
        (o.vars["metadata_path"].clone(), o.vars["linked"].clone()),
        (json!(live), json!(false))
    );
    assert_eq!(std::fs::read(w.vault.join("n/a.md")).unwrap(), bytes);
}
