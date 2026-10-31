//! WP-5 `x_metadata` / `x_discover` / `x_backfill` tests: a synthetic temp vault, a temp
//! `state.db` and a local fake oEmbed endpoint (never the network, never the live vault, never env vars).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_protocol::ActionContext;
use cv_state::LinkRecordInput;
use cv_testkit::{MockHttp, Reply};
use cv_vault::test_support::LiveFixture;
use serde_json::{Map, Value, json};

use crate::steps::testkit::StepEnv;
use crate::steps::web::{Overrides, create_or_found, test_overrides};
use crate::steps::{DeferCause, StepError, StepOutcome};
use crate::steps::{execute, validate};

struct World {
    env: StepEnv,
    rt: tokio::runtime::Runtime,
    vault: PathBuf,
    base: String,
    http: MockHttp,
}

impl std::ops::Deref for World {
    type Target = StepEnv;
    fn deref(&self) -> &StepEnv {
        &self.env
    }
}

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
    // Route substring (of path + query) → (status, body); anything else is an empty 500.
    let http = MockHttp::start(move |req| {
        routes
            .iter()
            .find(|(p, _, _)| req.target.contains(p))
            .map_or_else(
                || Reply::new(500, Vec::new()),
                |(_, s, b)| Reply::new(*s, b.clone()),
            )
    });
    let base = format!("{}/oembed", http.url());
    let files = BTreeMap::from([
        ("pipelines.toml".to_owned(), Some(pipelines.to_owned())),
        ("sources.toml".to_owned(), Some(String::new())),
    ]);
    let mut config = ConfigSet::new();
    config.reload(&files, &crate::steps::validate);
    World {
        vault: fx.vault.clone(),
        env: StepEnv::new(fx, config, action()),
        rt,
        base,
        http,
    }
}

fn step(kind: &str, src: &str) -> Step {
    let t: toml::Table = format!("type = \"{kind}\"\n{src}").parse().unwrap();
    Step::from_table(t).unwrap()
}

fn action() -> Action {
    Action {
        id: "x-metadata".into(),
        label: "X".into(),
        group: None,
        keys: vec![],
        params: vec![],
        needs: vec![],
        lane: LaneChoice::Auto,
        durable: true,
        hold: false,
        fast: false,
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
        bases: HashMap::from([("x-oembed".to_owned(), w.base.clone())]),
        secrets: None,
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

fn read(w: &World, rel: &str) -> String {
    std::fs::read_to_string(w.vault.join(rel)).unwrap()
}

fn oembed(handle: &str) -> String {
    format!(
        r#"{{"url":"https://x.com/{handle}/status/1","author_name":"Synth","author_url":"https://x.com/{handle}","html":"<blockquote><p>synthetic post</p></blockquote><script src=\"w.js\"></script>","type":"rich","version":"1.0"}}"#
    )
}

const PIPES: &str = "[x]\nenabled = true\ninbox = \"clips\"\n";
const NOTE: &str = "---\ntitle: n\nsource: https://x.com/synth/status/111\n---\nBody\n";

fn meta(w: &World, note: &str) -> Result<StepOutcome, StepError> {
    run(w, &step("x_metadata", ""), Some(note), &Map::new())
}

#[test]
fn params_validate() {
    let c = ConfigSet::new();
    assert!(validate(&step("x_metadata", "status_id = \"{vars.id}\""), &c).is_ok());
    assert!(validate(&step("x_metadata", "status_id = \"{bogus}\""), &c).is_err());
    assert!(validate(&step("x_metadata", "limit = 1"), &c).is_err());
    assert!(validate(&step("x_discover", "limit = 5"), &c).is_ok());
    assert!(validate(&step("x_backfill", "limit = 0"), &c).is_err());
}

#[test]
fn creates_links_counts_usage_then_rerun_is_a_noop() {
    let w = world(
        &[("n/a.md", NOTE)],
        vec![("status%2F111", 200, oembed("Synth"))],
        PIPES,
    );
    let o = meta(&w, "n/a.md").unwrap();
    let path = "_x_metadata/synth/111.md";
    assert_eq!(o.vars["status_id"], json!("111"));
    assert_eq!(o.vars["metadata_path"], json!(path));
    assert_eq!(o.vars["created"], json!(true));
    assert_eq!(o.vars["linked"], json!(true));
    let m = read(&w, path);
    assert!(
        m.starts_with(
            "---\nstatus-id: \"111\"\nurl: https://x.com/synth/status/111\nauthor: \"Synth\"\n"
        ),
        "{m}"
    );
    assert!(m.contains("synthetic post") && !m.contains("script"));
    let n = read(&w, "n/a.md");
    assert!(n.contains("[[_x_metadata/synth/111]]"), "{n}");
    let day = crate::steps::web::local_day();
    assert_eq!(
        w.state.lock().unwrap().api_usage("x-oembed", &day).unwrap(),
        1
    );
    // Already linked + exists → no fetch, no write.
    let o = meta(&w, "n/a.md").unwrap();
    assert_eq!(o.vars["linked"], json!(false));
    assert!(o.output_paths.is_empty());
    assert_eq!(read(&w, "n/a.md"), n);
    assert_eq!(w.http.hits(), 1);
}

#[test]
fn not_found_and_forbidden_tombstone_without_the_breaker() {
    for st in [404u16, 403] {
        let w = world(
            &[("n/a.md", NOTE), ("n/b.md", NOTE)],
            vec![("status%2F111", st, String::new())],
            PIPES,
        );
        let o = meta(&w, "n/a.md").unwrap();
        let path = "_x_metadata/_unavailable/111.md";
        assert_eq!(o.vars["tombstone"], json!(true), "{st}");
        assert_eq!(o.vars["metadata_path"], json!(path));
        assert!(
            read(&w, path)
                .contains("state: unavailable\nunavailable-reason: \"deleted-or-private\"")
        );
        // Another note citing the same status finds the tombstone: no refetch.
        let o = meta(&w, "n/b.md").unwrap();
        assert_eq!(o.vars["tombstone"], json!(true));
        assert_eq!(o.vars["linked"], json!(true));
        assert_eq!(w.http.hits(), 1);
    }
}

#[test]
fn rate_limit_and_5xx_defer_transient_on_x_oembed() {
    for st in [429u16, 503] {
        let w = world(
            &[("n/a.md", NOTE)],
            vec![("status%2F111", st, String::new())],
            PIPES,
        );
        match meta(&w, "n/a.md") {
            Err(StepError::Deferred {
                service,
                cause: DeferCause::Transient(_),
                ..
            }) => assert_eq!(service.as_deref(), Some("x-oembed")),
            other => panic!("{st}: {other:?}"),
        }
        assert!(!w.vault.join("_x_metadata").exists());
    }
}

#[test]
fn find_or_create_twice_yields_one_note() {
    let w = world(&[], vec![], PIPES);
    let _rt = w.rt.enter();
    let mut ctx = w.ctx();
    let p = "_x_metadata/a/1.md";
    assert!(create_or_found(&mut ctx, p, b"one".to_vec()).unwrap());
    assert!(!create_or_found(&mut ctx, p, b"two".to_vec()).unwrap());
    assert_eq!(read(&w, p), "one");
}

#[test]
fn discover_ignores_outside_inbox_and_dedupes() {
    let body = "---\nsource: https://x.com/synth/status/111\n---\nsee https://twitter.com/synth/status/111?s=20 and [x](https://x.com/other/status/222) and https://x.com/synth/status/111\n";
    let w = world(
        &[("clips/c.md", body), ("elsewhere/c.md", body)],
        vec![
            ("status%2F111", 200, oembed("synth")),
            ("status%2F222", 404, String::new()),
        ],
        PIPES,
    );
    let d = step("x_discover", "");
    let o = run(&w, &d, Some("elsewhere/c.md"), &Map::new()).unwrap();
    assert_eq!(o.vars["found"], json!(0));
    assert_eq!(w.http.hits(), 0);
    let o = run(&w, &d, Some("clips/c.md"), &Map::new()).unwrap();
    assert_eq!(o.vars["found"], json!(2));
    assert_eq!(o.vars["ensured"], json!(2));
    assert_eq!(w.http.hits(), 2);
    let note = read(&w, "clips/c.md");
    assert!(
        note.contains("[[_x_metadata/synth/111]]")
            && note.contains("[[_x_metadata/_unavailable/222]]"),
        "{note}"
    );
    // Unset inbox → no-op.
    let w2 = world(&[("clips/c.md", body)], vec![], "");
    let o = run(&w2, &d, Some("clips/c.md"), &Map::new()).unwrap();
    assert_eq!(o.vars["found"], json!(0));
}

/// Round 7 WP-1: with `origin = "any"` x-discover re-fires on its own stamp write (after the
/// cycle window); the second pass over the processed note must write nothing and fetch nothing.
#[test]
fn discover_second_pass_over_processed_note_writes_nothing() {
    let body =
        "---\nsource: https://x.com/synth/status/111\n---\nsee https://x.com/other/status/222\n";
    let w = world(
        &[("clips/c.md", body)],
        vec![
            ("status%2F111", 200, oembed("synth")),
            ("status%2F222", 404, String::new()),
        ],
        PIPES,
    );
    let d = step("x_discover", "");
    let first = run(&w, &d, Some("clips/c.md"), &Map::new()).unwrap();
    assert!(first.output_paths.contains(&"clips/c.md".to_owned()));
    let note = read(&w, "clips/c.md");
    let hits = w.http.hits();
    let again = run(&w, &d, Some("clips/c.md"), &Map::new()).unwrap();
    assert_eq!(again.vars["found"], json!(2));
    assert!(again.output_paths.is_empty(), "{:?}", again.output_paths);
    assert_eq!(read(&w, "clips/c.md"), note);
    assert_eq!(w.http.hits(), hits);
}

#[test]
fn backfill_skips_statuses_with_a_note_and_bounds_by_limit() {
    let w = world(
        &[
            ("src/a.md", "---\ntitle: a\n---\n"),
            ("_x_metadata/old/100.md", "---\nstatus-id: \"100\"\n---\n"),
        ],
        vec![
            ("status%2F2", 200, oembed("synth")),
            ("status%2F3", 200, oembed("synth")),
        ],
        PIPES,
    );
    {
        let mut s = w.state.lock().unwrap();
        for (url, id) in [
            ("https://x.com/old/status/100", "100"),
            ("https://x.com/synth/status/2", "2"),
            ("https://x.com/synth/status/3", "3"),
        ] {
            s.upsert_link_record(&LinkRecordInput {
                canonical_url: url.into(),
                url: url.into(),
                source_notes: vec!["[[src/a]]".into()],
                x_status_id: Some(id.into()),
                ..LinkRecordInput::default()
            })
            .unwrap();
        }
    }
    let b = step("x_backfill", "limit = 1");
    let o = run(&w, &b, None, &Map::new()).unwrap();
    assert_eq!(o.vars["pending"], json!(2));
    assert_eq!(o.vars["ensured"], json!(1));
    assert_eq!(o.vars["remaining"], json!(1));
    assert!(read(&w, "src/a.md").contains("[[_x_metadata/synth/2]]"));
    let o = run(&w, &b, None, &Map::new()).unwrap();
    assert_eq!(o.vars["pending"], json!(1));
    assert_eq!(o.vars["remaining"], json!(0));
    assert_eq!(w.http.hits(), 2);
}
