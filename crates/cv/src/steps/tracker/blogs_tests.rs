//! WP-8 `track_blogs` tests: a synthetic temp vault, a temp `state.db` and a local feed server
//! (never the network, never the live vault, never env vars).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_protocol::ActionContext;
use cv_state::{QueueConfig, StateDb, TrackerOutcome};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::super::hook;
use super::{KIND, execute, validate};
use crate::steps::{ChildSpawner, StepCtx, StepError, StepOutcome};

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

/// Path → (status, body); swappable between runs.
type Routes = Arc<Mutex<Vec<(&'static str, u16, String)>>>;

async fn serve(routes: Routes) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let routes = routes.clone();
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
                let (status, body) = routes
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(p, _, _)| path == *p)
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
    routes: Routes,
}

const CAPTURES: &str = "[periods.day]\nfolder = \"daily\"\nformat = \"YYYY-MM-DD\"\n";
const KINDS: &str =
    "[kinds.blog]\nlabel = \"Blog\"\ntags = [\"blog\"]\nfolder = \"{period.day}\"\n";

/// One RSS item: (slug, full body?).
fn rss(items: &[(&str, bool)]) -> String {
    let body: String = items
        .iter()
        .map(|(s, full)| {
            let content = if *full {
                format!("<content:encoded><![CDATA[<p>Body of {s} here</p>]]></content:encoded>")
            } else {
                String::new()
            };
            format!(
                "<item><title>Post {s}</title><link>https://blog.example/p/{s}</link><dc:creator>Ann</dc:creator><pubDate>Tue, 14 Oct 2026 10:00:00 +0000</pubDate><description>About {s}</description>{content}</item>"
            )
        })
        .collect();
    format!("<rss><channel><title>B</title>{body}</channel></rss>")
}

fn post_url(s: &str) -> String {
    format!("https://blog.example/p/{s}")
}

/// `sources` rows as (name, path, extra toml).
fn world(files: &[(&str, &str)], pipelines: &str, rows: &[(&str, &str, &str)]) -> World {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "cv-wp8-{}-{}",
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
    let routes: Routes = Arc::new(Mutex::new(Vec::new()));
    let base = rt.block_on(serve(routes.clone()));
    let sources: String = rows
        .iter()
        .map(|(name, path, extra)| {
            let link = if path.starts_with('/') {
                format!("{base}{path}")
            } else {
                (*path).to_owned()
            };
            format!("[[blog]]\nname = \"{name}\"\nlink = \"{link}\"\n{extra}\n")
        })
        .collect();
    let files = BTreeMap::from([
        ("pipelines.toml".to_owned(), Some(pipelines.to_owned())),
        ("sources.toml".to_owned(), Some(sources)),
        ("captures.toml".to_owned(), Some(CAPTURES.to_owned())),
        ("kinds.toml".to_owned(), Some(KINDS.to_owned())),
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
        routes,
    }
}

fn set(w: &World, path: &'static str, status: u16, body: String) {
    let mut r = w.routes.lock().unwrap();
    r.retain(|(p, _, _)| *p != path);
    r.push((path, status, body));
}

fn step(src: &str) -> Step {
    let t: toml::Table = format!("type = \"track_blogs\"\n{src}").parse().unwrap();
    Step::from_table(t).unwrap()
}

fn action() -> Action {
    Action {
        id: "track-blogs".into(),
        label: "Track blogs".into(),
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

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn daily(w: &World) -> String {
    std::fs::read_to_string(w.vault.join(format!("daily/{}.md", today()))).unwrap_or_default()
}

fn note(w: &World, slug: &str) -> Option<String> {
    std::fs::read_to_string(w.vault.join(format!("daily/{}/Post {slug}.md", today()))).ok()
}

fn outcome(w: &World, key: &str, s: &str) -> Option<TrackerOutcome> {
    w.state
        .lock()
        .unwrap()
        .tracker_item(KIND, key, &post_url(s))
        .unwrap()
        .map(|r| r.outcome)
}

fn n(o: &StepOutcome, k: &str) -> Value {
    o.vars[k].clone()
}

const ON: &str = "[blogs]\nenabled = true\nautomatable_window = 2\n";

#[test]
fn params_validate() {
    let c = ConfigSet::new();
    assert!(validate(&step(""), &c).is_ok());
    assert!(validate(&step("nope = 1"), &c).is_err());
}

#[test]
fn snippet_posts_become_lines_and_rerun_is_a_noop() {
    let w = world(&[], ON, &[("Snip", "/a", "body = \"snippet\"")]);
    set(&w, "/a", 200, rss(&[("b", true), ("a", true)]));
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "sources"), json!(1));
    assert_eq!(n(&o, "new"), json!(2));
    assert_eq!(n(&o, "lines"), json!(2));
    assert_eq!(n(&o, "notes"), json!(0));
    let d = daily(&w);
    assert!(d.contains(&format!("- [ ] [Post a]({}) — Snip", post_url("a"))));
    assert!(d.find(&post_url("a")).unwrap() < d.find(&post_url("b")).unwrap());
    let key = format!("{}/a", w.base);
    assert_eq!(outcome(&w, &key, "a"), Some(TrackerOutcome::Line));
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "new"), json!(0));
    assert_eq!(daily(&w), d);
}

#[test]
fn full_source_post_becomes_a_linted_filed_blog_note() {
    let w = world(
        &[],
        ON,
        &[("Full", "/a", "body = \"full\"\ntags = [\"research\"]")],
    );
    set(&w, "/a", 200, rss(&[("x", true)]));
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "notes"), json!(1));
    assert_eq!(n(&o, "lines"), json!(0));
    assert!(daily(&w).is_empty());
    let t = note(&w, "x").expect("note");
    let keys: Vec<&str> = t
        .lines()
        .skip(1)
        .take_while(|l| *l != "---")
        .filter(|l| !l.starts_with(' '))
        .map(|l| l.split(':').next().unwrap())
        .collect();
    // Lint appends its modified stamp (as on a filed blog note); the clipper keys keep order.
    assert_eq!(keys.last(), Some(&"updated"));
    assert_eq!(
        keys[..keys.len() - 1],
        [
            "title",
            "source",
            "post-id",
            "author",
            "published",
            "created",
            "description",
            "word-count",
            "tags",
            "read"
        ]
    );
    assert!(t.contains("tags:\n  - blog\n  - research\n"), "{t}");
    assert!(t.contains("read: false"));
    assert!(t.contains("author:\n  - \"[[Ann]]\""));
    // Lint recomputed the word count from the Markdown body ("Body of x here").
    assert!(t.contains("word-count: 4"), "{t}");
    let key = format!("{}/a", w.base);
    assert_eq!(outcome(&w, &key, "x"), Some(TrackerOutcome::Note));
    // Round-5 WP-11: the row carries the captured note's vault-relative path.
    let row = w
        .state
        .lock()
        .unwrap()
        .tracker_item(KIND, &key, &post_url("x"))
        .unwrap()
        .unwrap();
    assert_eq!(
        row.note_path.as_deref(),
        Some(format!("daily/{}/Post x.md", today()).as_str())
    );
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "new"), json!(0));
}

#[test]
fn auto_source_becomes_automatable_after_window_full_posts_across_restarts() {
    let w = world(&[], ON, &[("Auto", "/a", "")]);
    set(&w, "/a", 200, rss(&[("p2", true), ("p1", true)]));
    // Window 2: p1 and p2 have no prior evidence window → lines.
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "lines"), json!(2));
    // A fresh StepCtx (new run) sees the persisted evidence: p3 becomes a note.
    set(
        &w,
        "/a",
        200,
        rss(&[("p3", true), ("p2", true), ("p1", true)]),
    );
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "notes"), json!(1));
    assert!(note(&w, "p3").is_some());
    // A snippet-only post breaks the window: line, and the next full post is a line too.
    set(
        &w,
        "/a",
        200,
        rss(&[("p5", true), ("p4", false), ("p3", true)]),
    );
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "lines"), json!(2));
    assert_eq!(n(&o, "notes"), json!(0));
}

#[test]
fn interrupted_note_write_replays_as_record_only() {
    let w = world(&[], ON, &[("Full", "/a", "body = \"full\"")]);
    set(&w, "/a", 200, rss(&[("x", true)]));
    hook::fail_after(Some(1));
    assert!(run(&w).is_err());
    hook::fail_after(None);
    let before = note(&w, "x").expect("note written before the crash");
    let key = format!("{}/a", w.base);
    assert_eq!(outcome(&w, &key, "x"), None);
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "notes"), json!(0));
    assert_eq!(outcome(&w, &key, "x"), Some(TrackerOutcome::Note));
    assert_eq!(note(&w, "x").unwrap(), before);
    let day = w.vault.join(format!("daily/{}", today()));
    assert_eq!(std::fs::read_dir(day).unwrap().count(), 1);
}

#[test]
fn seeding_from_existing_blog_notes_produces_nothing() {
    let files = [
        (
            "_blog_metadata/b/post-a.md",
            "---\nsource: https://blog.example/p/a\npost-id: \"https://blog.example/p/a\"\n---\n",
        ),
        (
            "_crucible/orchestration/blogs/new-posts/2026-10-01.md",
            "- [ ] [B](https://blog.example/p/b?utm_source=rss)\n",
        ),
    ];
    let w = world(&files, ON, &[("Full", "/a", "body = \"full\"")]);
    set(&w, "/a", 200, rss(&[("b", true), ("a", true)]));
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "new"), json!(0));
    assert_eq!(n(&o, "notes"), json!(0));
    assert!(daily(&w).is_empty());
    assert!(note(&w, "a").is_none() && note(&w, "b").is_none());
}

#[test]
fn one_feed_failing_counts_all_failing_defers() {
    let rows = [("A", "/a", ""), ("B", "/b", "")];
    let w = world(&[], ON, &rows);
    set(&w, "/a", 200, rss(&[("a", false)]));
    set(&w, "/b", 503, String::new());
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "sources"), json!(2));
    assert_eq!(n(&o, "failed"), json!(1));
    assert_eq!(n(&o, "lines"), json!(1));

    let w = world(&[], ON, &rows);
    set(&w, "/a", 503, String::new());
    set(&w, "/b", 502, String::new());
    match run(&w) {
        Err(StepError::Deferred { service, .. }) => {
            assert_eq!(service.as_deref(), Some(super::SERVICE));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn invalid_and_excluded_rows_do_not_fail_the_run() {
    let rows = [
        ("A", "/a", ""),
        ("Bad", "ftp://nope", ""),
        ("Scrape", "/s", "method = \"scrape\""),
        ("Skip", "/k", "priority = \"skip\""),
    ];
    let w = world(&[], ON, &rows);
    set(&w, "/a", 200, rss(&[("a", false)]));
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "invalid"), json!(2));
    assert_eq!(n(&o, "sources"), json!(1));
    assert_eq!(n(&o, "lines"), json!(1));
}

#[test]
fn disabled_pipeline_does_nothing() {
    let w = world(&[], "", &[("A", "/a", "")]);
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "disabled"), json!(true));
    assert!(daily(&w).is_empty());
}

/// One full-body RSS item titled `title` whose body embeds the image at `img`.
fn rss_with_image(title: &str, img: &str) -> String {
    format!(
        "<rss><channel><title>B</title><item><title>{title}</title><link>https://blog.example/p/i</link><dc:creator>Ann</dc:creator><pubDate>Tue, 14 Oct 2026 10:00:00 +0000</pubDate><description>About</description><content:encoded><![CDATA[<p>Look</p><p><img src=\"{img}\" alt=\"pic\"></p>]]></content:encoded></item></channel></rss>"
    )
}

const SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"></svg>";

#[test]
fn full_body_capture_is_title_named_tag_normalized_and_localized_inline() {
    let w = world(
        &[],
        ON,
        &[(
            "Full",
            "/a",
            "body = \"full\"\ntags = [\"#a #b\", \"a, c\"]",
        )],
    );
    let img = format!("{}/img.svg", w.base);
    set(&w, "/a", 200, rss_with_image("Why: Rust? #1", &img));
    set(&w, "/img.svg", 200, SVG.to_owned());
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "notes"), json!(1));
    assert_eq!(n(&o, "localize_failed"), json!(0));
    let path = w.vault.join(format!("daily/{}/Why Rust 1.md", today()));
    let t = std::fs::read_to_string(&path).expect("title-named note");
    assert!(t.contains("tags:\n  - blog\n  - a\n  - b\n  - c\n"), "{t}");
    assert!(!t.contains(&img), "{t}");
    assert!(t.contains("_MD5.svg"), "{t}");
}

#[test]
fn image_404_keeps_the_remote_ref_and_still_captures() {
    let w = world(&[], ON, &[("Full", "/a", "body = \"full\"")]);
    let img = format!("{}/gone.png", w.base);
    set(&w, "/a", 200, rss_with_image("Gone", &img));
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "notes"), json!(1));
    let t =
        std::fs::read_to_string(w.vault.join(format!("daily/{}/Gone.md", today()))).expect("note");
    assert!(t.contains(&img), "{t}");
    let key = format!("{}/a", w.base);
    assert_eq!(outcome(&w, &key, "i"), Some(TrackerOutcome::Note));
}
