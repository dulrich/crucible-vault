//! WP-11 `localize` step tests: synthetic vaults in temp dirs, a temp protected root (I1), and a
//! hand-rolled HTTP responder on 127.0.0.1 (no real network).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cv_actions::{ConfigSet, Step};
use cv_protocol::ActionContext;
use cv_state::{QueueConfig, StateDb};
use cv_vault::{ContentHash, MemoryShadowLog, NoCutover, VaultFs, VaultWriter, WriterHandle};
use serde_json::{Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{ChildSpawner, StepCtx, StepError, StepOutcome, localize};

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "cv-wp11-{}-{}",
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

struct NoChildren;
impl ChildSpawner for NoChildren {
    fn spawn_child(&self, _: &str, _: Map<String, Value>) -> Result<String, String> {
        Err("no children in tests".into())
    }
}

/// A test vault + writer thread + state db.
struct Env {
    dir: Tmp,
    writer: WriterHandle,
    state: Arc<Mutex<StateDb>>,
    config: ConfigSet,
}

impl Env {
    fn vault(&self) -> PathBuf {
        self.dir.0.join("vault")
    }
    fn index(&self) -> PathBuf {
        self.dir.0.join("index").join("index.db")
    }
    fn file(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.vault().join(rel)).ok()
    }
    fn text(&self, rel: &str) -> String {
        String::from_utf8(self.file(rel).unwrap()).unwrap()
    }
    fn step(&self, toml_src: &str) -> Step {
        let t: toml::Table = toml::from_str(toml_src).unwrap();
        let mut full = t.clone();
        full.insert("type".into(), toml::Value::String("localize".into()));
        let s = Step::from_table(full).unwrap();
        localize::validate(&s, &self.config).unwrap();
        s
    }

    fn run(&self, step: &Step, note: &str) -> Result<StepOutcome, StepError> {
        let cfg = &self.config;
        let action = cfg.armed("loc").unwrap();
        let params = Map::new();
        let context = ActionContext {
            active_path: Some(note.to_owned()),
            ..ActionContext::default()
        };
        let vars = Map::new();
        let vault = self.vault();
        let index = self.index();
        let mut ctx = StepCtx {
            job_id: "job-1",
            action,
            step_index: 0,
            params: &params,
            context: &context,
            vars: &vars,
            config: cfg,
            vault_root: &vault,
            index_db: &index,
            state: &self.state,
            writer: Some(self.writer.clone()),
            children: &NoChildren,
            written: Vec::new(),
        };
        localize::execute(step, &mut ctx)
    }
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

fn config() -> ConfigSet {
    let mut c = ConfigSet::new();
    let src = "[[action]]\nid = \"loc\"\n[[action.steps]]\ntype = \"localize\"\n";
    let files = BTreeMap::from([("actions.toml".to_owned(), Some(src.to_owned()))]);
    c.reload(&files, &crate::steps::validate);
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    c
}

/// `files` are written into the vault before the writer (and its index) opens. `shadow`
/// mounts the writer in shadow mode over that vault.
fn env(files: &[(&str, &[u8])], shadow: bool) -> Env {
    let dir = Tmp::new();
    let vault = dir.0.join("vault");
    let prot = dir.0.join("protected");
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::create_dir_all(&prot).unwrap();
    std::fs::create_dir_all(dir.0.join("index")).unwrap();
    for (p, b) in files {
        write(&vault, p, b);
    }
    let fs = if shadow {
        VaultFs::shadow(
            &vault,
            &dir.0.join("scratch"),
            std::slice::from_ref(&prot),
            Box::new(MemoryShadowLog::default()),
        )
        .unwrap()
    } else {
        VaultFs::live(&vault, &prot, &NoCutover).unwrap()
    };
    let w = VaultWriter::open(fs, &dir.0.join("index").join("index.db")).unwrap();
    let (writer, _join) = cv_vault::spawn_writer(w).unwrap();
    let state = StateDb::open(&dir.0.join("state.db"), 1, QueueConfig::default()).unwrap();
    Env {
        dir,
        writer,
        state: Arc::new(Mutex::new(state)),
        config: config(),
    }
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

/// A hand-rolled HTTP/1.1 responder: `routes` maps a request path to (content type, body);
/// anything else is 404. `hook` runs before each response (e.g. to edit the note mid-download).
struct Server {
    port: u16,
    hits: Arc<AtomicUsize>,
}

async fn serve(routes: HashMap<String, (String, Vec<u8>)>, hook: Option<Hook>) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let routes = Arc::new(routes);
    let h2 = Arc::clone(&hits);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let routes = Arc::clone(&routes);
            let hits = Arc::clone(&h2);
            let hook = hook.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
                hits.fetch_add(1, Ordering::SeqCst);
                if let Some(h) = &hook {
                    h(&path);
                }
                let resp = match routes.get(&path) {
                    Some((ct, body)) => {
                        let mut r = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes();
                        r.extend_from_slice(body);
                        r
                    }
                    None => {
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec()
                    }
                };
                let _ = sock.write_all(&resp).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Server { port, hits }
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
    f: impl FnOnce(u16, Arc<AtomicUsize>) -> T + Send + 'static,
) -> T {
    let rt = rt();
    rt.block_on(async move {
        let s = serve(routes, hook).await;
        tokio::task::spawn_blocking(move || f(s.port, s.hits))
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
    let md5 = localize::md5_hex(&img);
    let routes = HashMap::from([
        route("/a/1.png", "image/png", img.clone()),
        route("/b/other", "image/png", img.clone()),
    ]);
    let (env, out, out2) = with_server(routes, None, move |port, hits| {
        let note = format!(
            "# Post\nA ![1](http://127.0.0.1:{port}/a/1.png) and B ![x](http://127.0.0.1:{port}/b/other)\n"
        );
        let env = env(&[("notes/My Post.md", note.as_bytes())], false);
        let step = env.step("");
        let out = env.run(&step, "notes/My Post.md").unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        // Re-run converges: nothing downloaded, nothing written.
        let before = env.file("notes/My Post.md");
        let out2 = env.run(&step, "notes/My Post.md").unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 2);
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
    let md5 = localize::md5_hex(&converted);
    let routes = HashMap::from([route("/p.bmp", "image/bmp", bmp)]);
    let env = with_server(routes, None, move |port, _| {
        let note = format!("![](http://127.0.0.1:{port}/p.bmp)\n");
        let env = env(&[("n.md", note.as_bytes())], false);
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
    let md5 = localize::md5_hex(&img);
    let local = format!("_attachments/n/{md5}_MD5.png");
    let note = format!("Lazy ![](data:image/gif;base64,R0lGODlhAQABAAAAACw=) ![]({local})\n");
    let env = env(&[("n.md", note.as_bytes()), (&local, &img)], false);
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

#[test]
fn concurrent_edit_during_download_is_a_conflict_not_a_clobber() {
    let img = png(3);
    let edited = b"user edit while downloading\n".to_vec();
    let vault_slot: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let slot2 = Arc::clone(&vault_slot);
    let edited2 = edited.clone();
    let hook: Hook = Arc::new(move |_| {
        if let Some(v) = slot2.lock().unwrap().as_ref() {
            std::fs::write(v.join("n.md"), &edited2).unwrap();
        }
    });
    let routes = HashMap::from([route("/x.png", "image/png", img)]);
    let (env, res) = with_server(routes, Some(hook), move |port, _| {
        let note = format!("![](http://127.0.0.1:{port}/x.png)\n");
        let env = env(&[("n.md", note.as_bytes())], false);
        *vault_slot.lock().unwrap() = Some(env.vault());
        let step = env.step("");
        let res = env.run(&step, "n.md");
        (env, res)
    });
    match res {
        Err(StepError::Failed { message, .. }) => {
            assert!(message.contains("conflict"), "{message}")
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(env.file("n.md"), Some(edited));
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
        let env = env(&[("n.md", note.as_bytes())], false);
        let step = env.step("");
        let out = env.run(&step, "n.md").unwrap();
        (env, out, port)
    });
    assert_eq!(var(&out, "downloaded"), 2);
    assert_eq!(var(&out, "failed"), 1);
    let (ma, mb) = (localize::md5_hex(&a), localize::md5_hex(&b));
    assert_eq!(
        env.text("n.md"),
        format!(
            "![](_attachments/n/{ma}_MD5.png)\n![](http://127.0.0.1:{port}/missing.png)\n![](_attachments/n/{mb}_MD5.png)\n"
        )
    );
}

#[test]
fn all_unreachable_defers() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }; // listener dropped: connection refused
    let note = format!("![](http://127.0.0.1:{port}/a.png) ![](http://127.0.0.1:{port}/b.png)\n");
    let (env, res) = with_server(HashMap::new(), None, move |_, _| {
        let env = env(&[("n.md", note.as_bytes())], false);
        let step = env.step("timeout_secs = 5");
        let res = env.run(&step, "n.md");
        (env, res)
    });
    assert_eq!(
        res,
        Err(StepError::Deferred {
            retry_after_ms: 60_000,
            service: Some("network".into())
        })
    );
    assert!(env.text("n.md").contains("http://127.0.0.1"));
}

#[test]
fn no_runtime_fails_cleanly() {
    let env = env(&[("n.md", b"![](http://127.0.0.1:9/a.png)\n")], false);
    let step = env.step("");
    assert_eq!(
        env.run(&step, "n.md"),
        Err(StepError::failed("no async runtime"))
    );
}

#[test]
fn shadow_mode_never_touches_live_files() {
    let img = png(6);
    let md5 = localize::md5_hex(&img);
    let routes = HashMap::from([route("/s.png", "image/png", img.clone())]);
    let (env, live_before, port) = with_server(routes, None, move |port, _| {
        let note = format!("![](http://127.0.0.1:{port}/s.png) ![](data:image/gif;base64,AA)\n");
        let other = b"untouched\n".to_vec();
        let env = env(&[("n.md", note.as_bytes()), ("o.md", &other)], true);
        let live_before: Vec<ContentHash> = ["n.md", "o.md"]
            .iter()
            .map(|p| ContentHash::of(&env.file(p).unwrap()))
            .collect();
        let step = env.step("");
        let out = env.run(&step, "n.md").unwrap();
        assert_eq!(var(&out, "downloaded"), 1);
        (env, live_before, port)
    });
    let live_after: Vec<ContentHash> = ["n.md", "o.md"]
        .iter()
        .map(|p| ContentHash::of(&env.file(p).unwrap()))
        .collect();
    assert_eq!(live_before, live_after);
    let target = format!("_attachments/n/{md5}_MD5.png");
    assert!(env.file(&target).is_none(), "attachment must not land live");
    // The overlay (what the writer sees) has the rewrite.
    let seen = env.writer.read("n.md").unwrap().unwrap();
    assert_eq!(
        String::from_utf8(seen).unwrap(),
        format!("![]({target}) \n")
    );
    assert!(env.writer.read(&target).unwrap().is_some());
    let _ = port;
}

#[test]
fn repair_repoints_broken_ref_and_counts_landed_write() {
    let img = png(7);
    let md5 = localize::md5_hex(&img);
    let kept = format!("_resources/keep/{md5}_MD5.png");
    let note = format!(
        "![](_resources/Clippings/gone/{md5}_MD5.png)\n[[old/doc_MD5.pdf|the doc]]\n![[nowhere.png]]\n"
    );
    let env = env(
        &[
            ("n.md", note.as_bytes()),
            (&kept, &img),
            ("_resources/k/doc_MD5.pdf", b"%PDF"),
        ],
        false,
    );
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
    let md5 = localize::md5_hex(&img);
    let env = env(
        &[
            ("n.md", b"![[pics/photo.png]] ![[pics/shared.png]]\n"),
            ("other.md", b"![[pics/shared.png]]\n"),
            ("pics/photo.png", &img),
            ("pics/shared.png", &img),
        ],
        false,
    );
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
