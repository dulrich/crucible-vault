//! `localize` step tests (was `wp11_tests.rs`): synthetic vaults in temp dirs (via [`super::testkit`])
//! and a `cv_testkit::MockHttp` on 127.0.0.1 (no real
//! network).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cv_actions::{ConfigSet, Step};
use cv_protocol::ActionContext;
use cv_testkit::{MockHttp, Reply};
use cv_vault::test_support::LiveFixture;
use serde_json::Value;

use super::testkit::{StepEnv, config_of};
use super::{DeferCause, StepError, StepOutcome, localize};

/// A test vault + writer thread + state db.
struct Env(StepEnv);

impl std::ops::Deref for Env {
    type Target = StepEnv;
    fn deref(&self) -> &StepEnv {
        &self.0
    }
}

impl Env {
    fn vault(&self) -> PathBuf {
        self.fx.vault.clone()
    }
    fn file(&self, rel: &str) -> Option<Vec<u8>> {
        self.fx.read(rel)
    }
    fn text(&self, rel: &str) -> String {
        self.fx.text(rel)
    }
    fn step(&self, toml_src: &str) -> Step {
        let t: toml::Table = toml::from_str(toml_src).unwrap();
        let mut full = t.clone();
        full.insert("type".into(), toml::Value::String("localize".into()));
        let s = Step::from_table(full).unwrap();
        crate::steps::validate(&s, &self.config).unwrap();
        s
    }

    fn run(&self, step: &Step, note: &str) -> Result<StepOutcome, StepError> {
        let context = ActionContext {
            active_path: Some(note.to_owned()),
            ..ActionContext::default()
        };
        let mut ctx = self.ctx();
        ctx.context = &context;
        crate::steps::execute(step, &mut ctx)
    }
}

fn config() -> ConfigSet {
    config_of(&[(
        "actions.toml",
        "[[action]]\nid = \"loc\"\n[[action.steps]]\ntype = \"localize\"\n",
    )])
}

/// `files` are written into the vault before the writer (and its index) opens.
fn env(files: &[(&str, &[u8])]) -> Env {
    let config = config();
    let action = config.armed("loc").unwrap().clone();
    Env(StepEnv::new(
        LiveFixture::new(files.iter().copied()),
        config,
        action,
    ))
}

fn png(seed: u8) -> Vec<u8> {
    let img = image::RgbImage::from_fn(16, 16, |x, y| {
        image::Rgb([seed, (x * 13) as u8, (y * 7) as u8])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

type Hook = Arc<dyn Fn(&str) + Send + Sync>;

/// `routes` maps a request target to (content type, body); `/throttle*` is 429, anything else
/// 404. `hook` runs before each response (e.g. to edit the note mid-download).
fn serve(routes: HashMap<String, (String, Vec<u8>)>, hook: Option<Hook>) -> MockHttp {
    MockHttp::start(move |req| {
        if let Some(h) = &hook {
            h(&req.target);
        }
        match routes.get(&req.target) {
            _ if req.target.starts_with("/throttle") => Reply::new(429, Vec::new()),
            Some((ct, body)) => Reply::new(200, body.clone()).header("Content-Type", ct),
            None => Reply::new(404, Vec::new()),
        }
    })
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// Runs `f` on a blocking thread of `rt` (as the runner does), after starting the responder.
fn with_server<T: Send + 'static>(
    routes: HashMap<String, (String, Vec<u8>)>,
    hook: Option<Hook>,
    f: impl FnOnce(u16, Arc<MockHttp>) -> T + Send + 'static,
) -> T {
    let rt = rt();
    let s = Arc::new(serve(routes, hook));
    let port = s.addr().port();
    rt.block_on(async move {
        tokio::task::spawn_blocking(move || f(port, s))
            .await
            .unwrap()
    })
}

fn route(path: &str, ct: &str, body: Vec<u8>) -> (String, (String, Vec<u8>)) {
    (path.to_owned(), (ct.to_owned(), body))
}

fn var(o: &StepOutcome, k: &str) -> u64 {
    o.vars.get(k).and_then(Value::as_u64).unwrap_or(u64::MAX)
}

#[test]
fn downloads_names_by_md5_rewrites_with_empty_alt_and_dedupes() {
    let img = png(1);
    let md5 = super::util::md5_hex(&img);
    let routes = HashMap::from([
        route("/a/1.png", "image/png", img.clone()),
        route("/b/other", "image/png", img.clone()),
    ]);
    let (env, out, out2) = with_server(routes, None, move |port, hits| {
        let note = format!(
            "# Post\nA ![1](http://127.0.0.1:{port}/a/1.png) and B ![x](http://127.0.0.1:{port}/b/other)\n"
        );
        let env = env(&[("notes/My Post.md", note.as_bytes())]);
        let step = env.step("");
        let out = env.run(&step, "notes/My Post.md").unwrap();
        assert_eq!(hits.hits(), 2);
        // Re-run converges: nothing downloaded, nothing written.
        let before = env.file("notes/My Post.md");
        let out2 = env.run(&step, "notes/My Post.md").unwrap();
        assert_eq!(hits.hits(), 2);
        assert_eq!(env.file("notes/My Post.md"), before);
        (env, out, out2)
    });
    let target = format!("notes/_attachments/my-post/{md5}_MD5.png");
    assert_eq!(env.file(&target), Some(img));
    assert_eq!(
        env.text("notes/My Post.md"),
        format!("# Post\nA ![]({t}) and B ![]({t})\n", t = target)
    );
    assert_eq!(var(&out, "downloaded"), 1);
    assert_eq!(var(&out, "reused"), 1);
    assert_eq!(var(&out, "failed"), 0);
    assert_eq!(
        out.output_paths,
        [target.clone(), "notes/My Post.md".to_owned()]
    );
    assert_eq!(out2.vars.get("changed"), Some(&Value::Bool(false)));
    assert_eq!(var(&out2, "downloaded"), 0);
    assert!(out2.output_paths.is_empty());
}

#[test]
fn converts_before_hashing_when_enabled() {
    // A noisy BMP compresses well as lossless WebP → the converted bytes name the file.
    let bmp = {
        let img = image::RgbImage::from_pixel(32, 32, image::Rgb([9, 9, 9]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, image::ImageFormat::Bmp)
            .unwrap();
        out.into_inner()
    };
    let (converted, ext) =
        localize::convert_image(bmp.clone(), "bmp", localize::ImageFormat::Webp, None);
    assert_eq!(ext, "webp");
    let md5 = super::util::md5_hex(&converted);
    let routes = HashMap::from([route("/p.bmp", "image/bmp", bmp)]);
    let env = with_server(routes, None, move |port, _| {
        let note = format!("![](http://127.0.0.1:{port}/p.bmp)\n");
        let env = env(&[("n.md", note.as_bytes())]);
        let step = env.step("convert_images = true");
        env.run(&step, "n.md").unwrap();
        env
    });
    let target = format!("_attachments/n/{md5}_MD5.webp");
    assert_eq!(env.file(&target), Some(converted));
    assert_eq!(env.text("n.md"), format!("![]({target})\n"));
}

#[test]
fn strips_data_image_placeholders_and_already_localized_note_is_untouched() {
    let img = png(2);
    let md5 = super::util::md5_hex(&img);
    let local = format!("_attachments/n/{md5}_MD5.png");
    let note = format!("Lazy ![](data:image/gif;base64,R0lGODlhAQABAAAAACw=) ![]({local})\n");
    let env = env(&[("n.md", note.as_bytes()), (&local, &img)]);
    let step = env.step("");
    let out = env.run(&step, "n.md").unwrap();
    assert_eq!(var(&out, "stripped"), 1);
    assert_eq!(env.text("n.md"), format!("Lazy ![]({local})\n"));
    let before = env.file("n.md");
    let out2 = env.run(&step, "n.md").unwrap();
    assert_eq!(out2.vars.get("changed"), Some(&Value::Bool(false)));
    assert_eq!(var(&out2, "stripped"), 0);
    assert_eq!(env.file("n.md"), before);
}

/// SR7: the note is read for the write only after the download, so an edit made while
/// downloading is kept and the ref rewrite applies on top of it (no CAS across the network).
#[test]
fn concurrent_edit_during_download_is_kept_and_refs_reapplied() {
    let img = png(3);
    let vault_slot: Arc<Mutex<Option<(PathBuf, String)>>> = Arc::new(Mutex::new(None));
    let slot2 = Arc::clone(&vault_slot);
    let hook: Hook = Arc::new(move |_| {
        if let Some((v, note)) = slot2.lock().unwrap().as_ref() {
            std::fs::write(v.join("n.md"), format!("user edit\n{note}")).unwrap();
        }
    });
    let routes = HashMap::from([route("/x.png", "image/png", img.clone())]);
    let (env, res) = with_server(routes, Some(hook), move |port, _| {
        let note = format!("![](http://127.0.0.1:{port}/x.png)\n");
        let env = env(&[("n.md", note.as_bytes())]);
        *vault_slot.lock().unwrap() = Some((env.vault(), note));
        let step = env.step("");
        let res = env.run(&step, "n.md");
        (env, res)
    });
    let out = res.unwrap();
    assert_eq!(out.vars.get("changed"), Some(&Value::Bool(true)));
    let m = super::util::md5_hex(&img);
    assert_eq!(
        env.text("n.md"),
        format!("user edit\n![](_attachments/n/{m}_MD5.png)\n")
    );
}

/// SR7: a CAS race between `modify`'s read and write re-applies the rewrite on the edited
/// text; a race on every attempt defers (`Wait`) and never clobbers.
#[test]
fn write_race_reapplies_then_persistent_race_defers() {
    let img = png(6);
    let routes = HashMap::from([route("/y.png", "image/png", img.clone())]);
    let (env, first, second) = with_server(routes, None, move |port, _| {
        let note = format!("![](http://127.0.0.1:{port}/y.png)\n");
        let env = env(&[("n.md", note.as_bytes()), ("m.md", note.as_bytes())]);
        super::race::inject(&env.vault(), "n.md", 1, |t| format!("{t}racer\n"));
        let step = env.step("");
        let first = env.run(&step, "n.md");
        super::race::inject(&env.vault(), "m.md", super::MODIFY_ATTEMPTS, |t| {
            format!("{t}x\n")
        });
        let second = env.run(&step, "m.md");
        (env, first, second)
    });
    let m = super::util::md5_hex(&img);
    assert!(first.is_ok(), "{first:?}");
    assert_eq!(
        env.text("n.md"),
        format!("![](_attachments/n/{m}_MD5.png)\nracer\n")
    );
    assert_eq!(second, Err(StepError::conflict()));
    assert!(env.text("m.md").ends_with("x\nx\nx\n"));
    assert!(env.text("m.md").contains("http://127.0.0.1"));
}

#[test]
fn one_failed_download_among_several_is_counted_and_left() {
    let (a, b) = (png(4), png(5));
    let routes = HashMap::from([
        route("/a.png", "image/png", a.clone()),
        route("/b.png", "image/png", b.clone()),
    ]);
    let (env, out, port) = with_server(routes, None, move |port, _| {
        let note = format!(
            "![](http://127.0.0.1:{port}/a.png)\n![](http://127.0.0.1:{port}/missing.png)\n![](http://127.0.0.1:{port}/b.png)\n"
        );
        let env = env(&[("n.md", note.as_bytes())]);
        let step = env.step("");
        let out = env.run(&step, "n.md").unwrap();
        (env, out, port)
    });
    assert_eq!(var(&out, "downloaded"), 2);
    assert_eq!(var(&out, "failed"), 1);
    let (ma, mb) = (super::util::md5_hex(&a), super::util::md5_hex(&b));
    assert_eq!(
        env.text("n.md"),
        format!(
            "![](_attachments/n/{ma}_MD5.png)\n![](http://127.0.0.1:{port}/missing.png)\n![](_attachments/n/{mb}_MD5.png)\n"
        )
    );
}

#[test]
fn all_unreachable_succeeds_and_leaves_the_refs() {
    let port = MockHttp::unused_port(); // nothing listens: connection refused
    let note = format!("![](http://127.0.0.1:{port}/a.png) ![](http://127.0.0.1:{port}/b.png)\n");
    let (env, res) = with_server(HashMap::new(), None, move |_, _| {
        let env = env(&[("n.md", note.as_bytes())]);
        let step = env.step("timeout_secs = 5");
        let res = env.run(&step, "n.md");
        (env, res)
    });
    // Plan round-11 Decision 2: success, so the chain goes on to describe and auto-file.
    let out = res.unwrap();
    assert!(!out.stop);
    assert_eq!(out.vars.get("changed"), Some(&Value::Bool(false)));
    assert!(env.text("n.md").contains("http://127.0.0.1"));
}

#[test]
fn folder_backfill_leaves_an_all_unreachable_note_unrecorded() {
    let port = MockHttp::unused_port();
    let note = format!("![](http://127.0.0.1:{port}/a.png)\n");
    let (env, out) = with_server(HashMap::new(), None, move |_, _| {
        let env = env(&[("Clippings/n.md", note.as_bytes())]);
        let step = env.step("scope = \"folder\"\nfolder = \"Clippings\"\ntimeout_secs = 5");
        let out = env.run(&step, "").unwrap();
        (env, out)
    });
    assert_eq!(var(&out, "failed"), 0);
    assert_eq!(var(&out, "ineligible"), 0);
    assert!(ignored(&env).is_empty(), "not in the exclusion set");
}

#[test]
fn no_runtime_fails_cleanly() {
    let env = env(&[("n.md", b"![](http://127.0.0.1:9/a.png)\n")]);
    let step = env.step("");
    assert_eq!(
        env.run(&step, "n.md"),
        Err(StepError::failed("no async runtime"))
    );
}

#[test]
fn repair_repoints_broken_ref_and_counts_landed_write() {
    let img = png(7);
    let md5 = super::util::md5_hex(&img);
    let kept = format!("_resources/keep/{md5}_MD5.png");
    let note = format!(
        "![](_resources/Clippings/gone/{md5}_MD5.png)\n[[old/doc_MD5.pdf|the doc]]\n![[nowhere.png]]\n"
    );
    let env = env(&[
        ("n.md", note.as_bytes()),
        (&kept, &img),
        ("_resources/k/doc_MD5.pdf", b"%PDF"),
    ]);
    let step = env.step("mode = \"repair\"");
    let out = env.run(&step, "n.md").unwrap();
    assert_eq!(var(&out, "repaired"), 2);
    assert_eq!(var(&out, "unrepairable"), 1);
    assert_eq!(
        env.text("n.md"),
        format!("![]({kept})\n[[_resources/k/doc_MD5.pdf|the doc]]\n![[nowhere.png]]\n")
    );
    // Re-run: refs resolve → nothing to repair, no write.
    let out2 = env.run(&step, "n.md").unwrap();
    assert_eq!(var(&out2, "repaired"), 0);
    assert_eq!(out2.vars.get("changed"), Some(&Value::Bool(false)));
}

#[test]
fn local_attachment_is_rehomed_and_unshared_source_retired() {
    let img = png(8);
    let md5 = super::util::md5_hex(&img);
    let env = env(&[
        ("n.md", b"![[pics/photo.png]] ![[pics/shared.png]]\n"),
        ("other.md", b"![[pics/shared.png]]\n"),
        ("pics/photo.png", &img),
        ("pics/shared.png", &img),
    ]);
    let step = env.step("");
    let out = env.run(&step, "n.md").unwrap();
    let target = format!("_attachments/n/{md5}_MD5.png");
    assert_eq!(env.text("n.md"), format!("![[{target}]] ![[{target}]]\n"));
    assert_eq!(env.file(&target), Some(img));
    assert!(
        env.file("pics/photo.png").is_none(),
        "unshared source retired"
    );
    assert!(env.file("pics/shared.png").is_some(), "shared source kept");
    assert_eq!(var(&out, "reused"), 1);
}

// --- plan usability-sprint-captures-links WP-2: folder-scope backfill ---------------------------

fn ignored(env: &Env) -> Vec<String> {
    env.state
        .lock()
        .unwrap()
        .ignored_ids(localize::BACKFILL_KIND)
        .unwrap()
}

#[test]
fn folder_backfill_is_bounded_resumes_and_records_failures() {
    let img = png(7);
    let routes = HashMap::from([route("/ok.png", "image/png", img.clone())]);
    let (env, outs) = with_server(routes, None, move |port, _| {
        let ok = format!("![](http://127.0.0.1:{port}/ok.png)\n");
        let bad = format!("![](http://127.0.0.1:{port}/gone.png)\n");
        let fenced = format!("```\n![](http://127.0.0.1:{port}/ok.png)\n```\n");
        let env = env(&[
            ("Clippings/a.md", ok.as_bytes()),
            ("Clippings/b.md", bad.as_bytes()),
            ("Clippings/sub/c.md", ok.as_bytes()),
            ("Clippings/d.md", b"no images\n"),
            ("Clippings/e.md", fenced.as_bytes()),
            ("other/x.md", ok.as_bytes()),
        ]);
        let step = env.step("scope = \"folder\"\nfolder = \"Clippings\"\nlimit = 2");
        let outs: Vec<StepOutcome> = (0..3).map(|_| env.run(&step, "").unwrap()).collect();
        (env, outs)
    });
    let v = |i: usize, k: &str| var(&outs[i], k);
    // Run 1: a (localized) + b (404 → recorded); c left for later.
    assert_eq!(
        (
            v(0, "localized"),
            v(0, "failed"),
            v(0, "unattempted_remaining")
        ),
        (1, 1, 1)
    );
    assert_eq!(v(0, "scanned"), 5);
    assert_eq!(ignored(&env), ["Clippings/b.md"]);
    // Run 2: b is not re-selected; c is localized; nothing remains.
    assert_eq!(
        (
            v(1, "localized"),
            v(1, "failed"),
            v(1, "unattempted_remaining")
        ),
        (1, 0, 0)
    );
    // Run 3: converged.
    assert_eq!(
        (
            v(2, "localized"),
            v(2, "failed"),
            v(2, "unattempted_remaining")
        ),
        (0, 0, 0)
    );
    let md5 = super::util::md5_hex(&img);
    assert_eq!(
        env.text("Clippings/sub/c.md"),
        format!("![](Clippings/sub/_attachments/c/{md5}_MD5.png)\n")
    );
    assert!(env.text("Clippings/b.md").contains("gone.png"));
    assert!(env.text("Clippings/e.md").contains("127.0.0.1"));
    assert!(env.text("other/x.md").contains("127.0.0.1"));
}

#[test]
fn folder_backfill_throttle_is_not_a_permanent_failure() {
    let img = png(9);
    let routes = HashMap::from([route("/ok.png", "image/png", img)]);
    let (env, outs) = with_server(routes, None, move |port, _| {
        let ok = format!("![](http://127.0.0.1:{port}/ok.png)\n");
        let limited = format!("![](http://127.0.0.1:{port}/throttle.png)\n");
        let env = env(&[
            ("daily/a.md", ok.as_bytes()),
            ("daily/b.md", limited.as_bytes()),
            ("daily/c.md", ok.as_bytes()),
        ]);
        let step = env.step("scope = \"folder\"\nfolder = \"daily\"\nlimit = 10");
        let outs: Vec<StepOutcome> = (0..2).map(|_| env.run(&step, "").unwrap()).collect();
        (env, outs)
    });
    let v = |i: usize, k: &str| var(&outs[i], k);
    // Run 1: a localized; b throttled → not recorded, batch ends; b and c stay unattempted.
    assert_eq!(
        (
            v(0, "localized"),
            v(0, "failed"),
            v(0, "throttled"),
            v(0, "unattempted_remaining")
        ),
        (1, 0, 1, 2)
    );
    assert!(
        ignored(&env).is_empty(),
        "a throttled note is never recorded"
    );
    // Run 2: b is re-selected (still throttled) — progress waits on the host, nothing is lost.
    assert_eq!((v(1, "throttled"), v(1, "unattempted_remaining")), (1, 2));
    assert!(env.text("daily/b.md").contains("throttle.png"));
}

/// SR7: a folder-scope note whose write keeps losing CAS races is never recorded as ignored —
/// it stays eligible, and a later run localizes it.
#[test]
fn folder_backfill_conflict_is_not_recorded() {
    let img = png(10);
    let routes = HashMap::from([route("/ok.png", "image/png", img)]);
    let (env, outs) = with_server(routes, None, move |port, _| {
        let ok = format!("![](http://127.0.0.1:{port}/ok.png)\n");
        let env = env(&[("daily/a.md", ok.as_bytes()), ("daily/b.md", ok.as_bytes())]);
        super::race::inject(&env.vault(), "daily/a.md", super::MODIFY_ATTEMPTS, |t| {
            format!("{t}edit\n")
        });
        let step = env.step("scope = \"folder\"\nfolder = \"daily\"\nlimit = 10");
        let outs: Vec<StepOutcome> = (0..2).map(|_| env.run(&step, "").unwrap()).collect();
        (env, outs)
    });
    assert!(
        ignored(&env).is_empty(),
        "a conflicted note is never recorded"
    );
    assert_eq!(
        (var(&outs[0], "localized"), var(&outs[0], "failed")),
        (1, 0)
    );
    assert_eq!(
        (var(&outs[1], "localized"), var(&outs[1], "failed")),
        (1, 0)
    );
    assert!(env.text("daily/a.md").contains("_MD5.png"));
    assert!(env.text("daily/a.md").ends_with("edit\nedit\nedit\n"));
}

/// A note-scope download the host throttles (429) is a transient failure on
/// the `localize` breaker key — the step defers instead of succeeding with refs left.
#[test]
fn note_scope_throttle_defers_on_the_breaker_key() {
    let (env, res) = with_server(HashMap::new(), None, move |port, _| {
        let note = format!("![](http://127.0.0.1:{port}/throttle.png)\n");
        let env = env(&[("n.md", note.as_bytes())]);
        let step = env.step("");
        let res = env.run(&step, "n.md");
        (env, res)
    });
    assert_eq!(
        res,
        Err(StepError::Deferred {
            retry_after_ms: super::TRANSIENT_BASE_MS,
            service: Some(localize::SERVICE.into()),
            cause: DeferCause::Transient(cv_state::ServiceFailureKind::RateLimited),
        })
    );
    assert!(env.text("n.md").contains("throttle.png"));
}

#[test]
fn embed_pages_are_not_attachments() {
    for u in [
        "https://www.youtube.com/watch?v=abc",
        "https://youtube.com/shorts/abc",
        "https://m.youtube.com/watch?v=abc",
        "https://www.youtube.com/embed/abc",
        "https://youtu.be/abc",
        "https://x.com/someone/status/123",
        "https://twitter.com/i/status/123",
    ] {
        assert!(cv_text::attachments::is_embed_page(u), "{u}");
        assert!(
            !cv_text::attachments::has_remote_image_refs(&format!("![]({u})")),
            "{u}"
        );
    }
    for u in [
        "https://i.ytimg.com/vi/abc/hqdefault.jpg",
        "https://www.youtube.com/",
        "https://pbs.twimg.com/media/a.jpg",
        "https://x.com/someone",
    ] {
        assert!(!cv_text::attachments::is_embed_page(u), "{u}");
    }
    assert!(cv_text::attachments::has_remote_image_refs(
        "![](https://www.youtube.com/watch?v=a)\n![](https://x.test/a.png)"
    ));
}

#[test]
fn a_page_response_is_ineligible_not_failed() {
    let img = png(3);
    let routes = HashMap::from([
        route("/ok.png", "image/png", img),
        route(
            "/page.png",
            "text/html; charset=utf-8",
            b"<html></html>".to_vec(),
        ),
    ]);
    let (env, out) = with_server(routes, None, move |port, _| {
        let note = format!(
            "![](http://127.0.0.1:{port}/ok.png)\n![](http://127.0.0.1:{port}/page.png)\n![](https://www.youtube.com/watch?v=a)\n"
        );
        let env = env(&[("n.md", note.as_bytes())]);
        let step = env.step("");
        let out = env.run(&step, "n.md").unwrap();
        (env, out)
    });
    assert_eq!(
        (
            var(&out, "downloaded"),
            var(&out, "failed"),
            var(&out, "ineligible")
        ),
        (1, 0, 2)
    );
    let t = env.text("n.md");
    assert!(t.contains("/page.png") && t.contains("youtube.com/watch"));
    assert!(!t.contains("/ok.png"));
}

#[test]
fn folder_backfill_settles_ineligible_notes_and_skips_embed_only_notes() {
    let routes = HashMap::from([route("/page.png", "text/html", b"<html></html>".to_vec())]);
    let (env, outs) = with_server(routes, None, move |port, _| {
        let page = format!("![](http://127.0.0.1:{port}/page.png)\n");
        let env = env(&[
            ("daily/a.md", page.as_bytes()),
            ("daily/v.md", b"![](https://www.youtube.com/watch?v=a)\n"),
        ]);
        let step = env.step("scope = \"folder\"\nfolder = \"daily\"\nlimit = 10");
        let outs: Vec<StepOutcome> = (0..2).map(|_| env.run(&step, "").unwrap()).collect();
        (env, outs)
    });
    let v = |i: usize, k: &str| var(&outs[i], k);
    assert_eq!(
        (
            v(0, "localized"),
            v(0, "failed"),
            v(0, "ineligible"),
            v(0, "unattempted_remaining")
        ),
        (0, 0, 1, 0)
    );
    assert_eq!(
        ignored(&env),
        ["daily/a.md"],
        "settled, never the embed-only note"
    );
    assert_eq!((v(1, "ineligible"), v(1, "unattempted_remaining")), (0, 0));
}

#[test]
fn folder_scope_params_validate() {
    let c = config();
    let mk = |src: &str| {
        let mut t: toml::Table = toml::from_str(src).unwrap();
        t.insert("type".into(), toml::Value::String("localize".into()));
        crate::steps::validate(&Step::from_table(t).unwrap(), &c)
    };
    assert!(mk("scope = \"folder\"\nfolder = \"daily\"").is_ok());
    assert!(mk("scope = \"note\"").is_ok());
    assert!(mk("scope = \"folder\"").is_err());
    assert!(mk("folder = \"daily\"").is_err());
    assert!(mk("scope = \"vault\"").is_err());
    assert!(mk("scope = \"folder\"\nfolder = \"d\"\nlimit = 0").is_err());
}

#[test]
fn remote_ref_predicate_skips_fences_and_sees_data_uris() {
    assert!(cv_text::attachments::has_remote_image_refs(
        "![](https://x.test/a.png)"
    ));
    assert!(cv_text::attachments::has_remote_image_refs(
        "![](data:image/gif;base64,AA)"
    ));
    assert!(!cv_text::attachments::has_remote_image_refs(
        "```\n![](https://x.test/a.png)\n```\n"
    ));
    assert!(!cv_text::attachments::has_remote_image_refs(
        "[link](https://x.test/a.png) ![](local.png)"
    ));
}

/// Plan round-15 WP-1: page-relative image refs resolve against the note's `source` URL
/// (root-relative, bare file, encoded reserved character), download through the remote path
/// and are rewritten to managed `_MD5` attachments; a 404 stays as it is.
#[test]
fn relative_image_refs_resolve_against_the_source_url() {
    let (a, b, c) = (png(21), png(22), png(23));
    let routes = HashMap::from([
        route("/img/a.png", "image/png", a.clone()),
        route("/blog/b.png", "image/png", b.clone()),
        // `x%23y.png` is the file `x#y.png`: requested encoded, never as a fragment.
        route("/blog/x%23y.png", "image/png", c.clone()),
    ]);
    let (env, out, port) = with_server(routes, None, move |port, _| {
        let note = format!(
            "---\nsource: http://127.0.0.1:{port}/blog/post?id=1#top\n---\n![](/img/a.png)\n![alt](b.png)\n![](x%23y.png)\n![](gone.png)\n"
        );
        let env = env(&[("n.md", note.as_bytes())]);
        let step = env.step("");
        let out = env.run(&step, "n.md").unwrap();
        (env, out, port)
    });
    assert_eq!(var(&out, "downloaded"), 3);
    assert_eq!(var(&out, "failed"), 1);
    let m = |x: &Vec<u8>| super::util::md5_hex(x);
    assert_eq!(
        env.text("n.md"),
        format!(
            "---\nsource: http://127.0.0.1:{port}/blog/post?id=1#top\n---\n![](_attachments/n/{}_MD5.png)\n![](_attachments/n/{}_MD5.png)\n![](_attachments/n/{}_MD5.png)\n![](gone.png)\n",
            m(&a),
            m(&b),
            m(&c)
        )
    );
    assert!(
        env.file(&format!("_attachments/n/{}_MD5.png", m(&c)))
            .is_some()
    );
}

#[test]
fn relative_image_refs_without_an_http_source_are_untouched() {
    let routes = HashMap::from([route("/a.png", "image/png", png(24))]);
    let (note_a, note_b) = with_server(routes, None, |port, _| {
        let none = "![](/a.png)\n".to_owned();
        let ftp = format!("---\nsource: ftp://127.0.0.1:{port}/p\n---\n![](/a.png)\n");
        let env = env(&[("a.md", none.as_bytes()), ("b.md", ftp.as_bytes())]);
        let step = env.step("");
        env.run(&step, "a.md").unwrap();
        env.run(&step, "b.md").unwrap();
        ((none, env.text("a.md")), (ftp, env.text("b.md")))
    });
    assert_eq!(note_a.0, note_a.1);
    assert_eq!(note_b.0, note_b.1);
}
