//! `resolve_removals` / `next_fold_note` over an in-process daemon on a synthetic temp vault
//! (plan intake-removals-sources-rescan WP-4). Never touches the live vault.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use cv::{Client, Config, Daemon, DaemonOptions, RunningDaemon};
use cv_protocol::{
    BindNoteParams, NextFoldNoteParams, NextFoldNoteResult, NoteId, RemovalKind, RemovalOp,
    RemovalRefusal, RemovalVerdict, RemovalsApplied, Request, ResolveRemovalsParams,
    ResolveRemovalsResult,
};
use cv_vault::{ContentHash, WatchConfig};

const POST: &str = "---\npublication: Synth Pub\ntitle: Synthetic\n---\nIntro paragraph of the synthetic post.\n\n%% cv-enrich %%\n%% cv-fold lines=4 kind=sponsor at=start gap=0 join=0 %%\n> [!removed]-\n> Sponsored by the synthetic widget company\n> ![[old_MD5.png]]\n> ![[shared_MD5.png]]\n> ![[fresh_MD5.png]]\n\n%% /cv-enrich %%\n";

struct Env {
    _tmp: tempfile::TempDir,
    vault: PathBuf,
    vars: HashMap<String, String>,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let vault = tmp.path().join("vault").join("Synth");
        std::fs::create_dir_all(vault.join("media")).unwrap();
        cv_vault::test_support::assert_temp_root(&vault);
        std::fs::write(vault.join("post.md"), POST).unwrap();
        std::fs::write(vault.join("other.md"), "# Other\n![[shared_MD5.png]]\n").unwrap();
        std::fs::write(vault.join("plain.md"), "# Plain\nno fold here\n").unwrap();
        let old = SystemTime::now() - Duration::from_secs(48 * 3600);
        for name in ["old_MD5.png", "shared_MD5.png", "fresh_MD5.png"] {
            let p = vault.join("media").join(name);
            std::fs::write(&p, name.as_bytes()).unwrap();
            if name != "fresh_MD5.png" {
                std::fs::File::options()
                    .write(true)
                    .open(&p)
                    .unwrap()
                    .set_modified(old)
                    .unwrap();
            }
        }
        let r = tmp.path();
        let mut vars = HashMap::new();
        for (k, v) in [
            ("HOME", r.join("home")),
            ("CV_VAULT", vault.clone()),
            ("CV_STATE_DB", r.join("state/state.db")),
            ("CV_INDEX_DB", r.join("cache/index.db")),
            ("CV_CONFIG_DIR", r.join("config")),
            ("CV_SOCKET", r.join("run/d.sock")),
        ] {
            vars.insert(k.to_owned(), v.display().to_string());
        }
        Self {
            _tmp: tmp,
            vault,
            vars,
        }
    }
    fn config(&self) -> Config {
        let vars = self.vars.clone();
        Config::resolve(move |k| vars.get(k).cloned()).unwrap()
    }
}

async fn start(env: &Env) -> (RunningDaemon, Client) {
    let opts = DaemonOptions {
        watch: WatchConfig {
            debounce: Duration::from_millis(50),
            echo_ttl: Duration::from_secs(5),
        },
        config_poll: Duration::from_millis(100),
    };
    let d = Daemon::start(env.config(), opts)
        .unwrap()
        .spawn()
        .await
        .unwrap();
    let c = Client::connect(
        Path::new(&env.vars["CV_SOCKET"]),
        "test",
        cv_index::SCHEMA_VERSION,
    )
    .unwrap();
    (d, c)
}

fn bind(c: &Client, path: &str) -> NoteId {
    let v = c
        .request(Request::BindNote(BindNoteParams { path: path.into() }))
        .unwrap();
    serde_json::from_value(v["id"].clone()).unwrap()
}

fn hash(env: &Env, path: &str) -> String {
    ContentHash::of(&std::fs::read(env.vault.join(path)).unwrap())
        .as_str()
        .to_owned()
}

fn resolve(c: &Client, id: NoteId, base_hash: String, op: RemovalOp) -> ResolveRemovalsResult {
    let v = c
        .request(Request::ResolveRemovals(ResolveRemovalsParams {
            id,
            base_hash,
            op,
        }))
        .unwrap();
    serde_json::from_value(v).unwrap()
}

fn applied(r: ResolveRemovalsResult) -> RemovalsApplied {
    match r {
        ResolveRemovalsResult::Applied(a) => a,
        other => panic!("expected applied, got {other:?}"),
    }
}

fn group_id(env: &Env) -> String {
    let text = std::fs::read_to_string(env.vault.join("post.md")).unwrap();
    let doc = cv_note::FrontmatterDoc::new(text);
    cv_ingest::enrich::fold_groups(doc.body())[0].id.clone()
}

fn labels(d: &RunningDaemon) -> Vec<cv_state::RemovalLabel> {
    d.shared().state().lock().unwrap().removal_labels().unwrap()
}

fn trash(d: &RunningDaemon) -> Vec<cv_state::NewTrashEntry> {
    d.shared()
        .state()
        .lock()
        .unwrap()
        .trash_entries()
        .unwrap()
        .into_iter()
        .map(|e| e.entry)
        .collect()
}

fn cleanup_jobs(d: &RunningDaemon) -> usize {
    let q = cv_state::ListQuery {
        job_type: Some(format!("action:{}", cv_actions::MEDIA_CLEANUP_ACTION)),
        ..cv_state::ListQuery::default()
    };
    d.shared().state().lock().unwrap().list(&q).unwrap().len()
}

/// Refusals change nothing: note bytes, label rows and the trash ledger stay as they were.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refusals_change_nothing() {
    let env = Env::new();
    let (d, c) = start(&env).await;
    let id = bind(&c, "post.md");
    let all = RemovalOp::All {
        verdict: RemovalVerdict::Delete,
    };
    let r = resolve(&c, id, "0".repeat(64), all.clone());
    assert!(matches!(
        r,
        ResolveRemovalsResult::Refused {
            reason: RemovalRefusal::StaleHash,
            current_hash: Some(ref h)
        } if *h == hash(&env, "post.md")
    ));
    let unknown = RemovalOp::Group {
        group: "no-such-group".into(),
        verdict: RemovalVerdict::Delete,
    };
    let r = resolve(&c, id, hash(&env, "post.md"), unknown);
    assert!(matches!(
        r,
        ResolveRemovalsResult::Refused {
            reason: RemovalRefusal::UnknownGroup,
            ..
        }
    ));
    let r = resolve(&c, NoteId(9999), hash(&env, "post.md"), all);
    assert!(matches!(
        r,
        ResolveRemovalsResult::Refused {
            reason: RemovalRefusal::UnknownNote,
            ..
        }
    ));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        std::fs::read_to_string(env.vault.join("post.md")).unwrap(),
        POST
    );
    assert!(labels(&d).is_empty());
    assert!(trash(&d).is_empty());
    assert_eq!(cleanup_jobs(&d), 0);
    drop(c);
    d.shutdown().await.unwrap();
}

/// Delete one group: the note is rewritten with its frontmatter intact, every non-blank group
/// line is labelled, and only the old, otherwise-unreferenced media file is trashed as an orphan.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_group_labels_lines_and_trashes_only_the_old_orphan() {
    let env = Env::new();
    let (d, c) = start(&env).await;
    let id = bind(&c, "post.md");
    let op = RemovalOp::Group {
        group: group_id(&env),
        verdict: RemovalVerdict::Delete,
    };
    let a = applied(resolve(&c, id, hash(&env, "post.md"), op));
    assert_eq!(a.path, "post.md");
    assert_eq!(a.hash, hash(&env, "post.md"));
    assert_eq!((a.groups, a.lines_labelled), (1, 4));
    assert_eq!(a.media_queued, 3);
    assert!(!a.cleanup_failed);
    let job_id = a.cleanup_job.clone().expect("a queued cleanup job");
    let job = d
        .shared()
        .state()
        .lock()
        .unwrap()
        .job(&job_id)
        .unwrap()
        .unwrap();
    assert!(
        job.resource_keys
            .iter()
            .any(|k| k == cv_actions::ATTACHMENTS_KEY)
    );
    let text = std::fs::read_to_string(env.vault.join("post.md")).unwrap();
    assert!(text.starts_with("---\npublication: Synth Pub\ntitle: Synthetic\n---\n"));
    assert!(!text.contains("[!removed]") && !text.contains("widget"));
    let rows = labels(&d);
    assert_eq!(rows.len(), 4);
    for r in &rows {
        assert_eq!(
            (
                r.note_id,
                r.publication.as_str(),
                r.kind.as_str(),
                r.verdict.as_str(),
                r.origin.as_str()
            ),
            (
                i64::try_from(id.0).unwrap(),
                "Synth Pub",
                "sponsor",
                "delete",
                "fold"
            )
        );
        assert_eq!(r.line_key, cv_ingest::boilerplate::line_key(&r.line));
    }
    assert_eq!(rows[0].line, "Sponsored by the synthetic widget company");
    // The daemon's own claim loop runs the queued job to completion.
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        let j = d
            .shared()
            .state()
            .lock()
            .unwrap()
            .job(&job_id)
            .unwrap()
            .unwrap();
        if j.status.is_terminal() || Instant::now() > deadline {
            break j.status;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status, cv_state::JobStatus::Done);
    let cp = d
        .shared()
        .state()
        .lock()
        .unwrap()
        .checkpoints(&job_id)
        .unwrap();
    let vars: serde_json::Value = serde_json::from_str(&cp[0].outcome).unwrap();
    let n = |k: &str| vars["vars"][k].as_u64().unwrap();
    assert_eq!(
        (
            n("media_trashed"),
            n("media_referenced"),
            n("media_too_young"),
            n("media_skipped")
        ),
        (1, 1, 1, 0)
    );
    let t = trash(&d);
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].original_path, "media/old_MD5.png");
    assert_eq!(t[0].reason.as_str(), "orphan");
    assert!(env.vault.join("media/shared_MD5.png").exists());
    assert!(env.vault.join("media/fresh_MD5.png").exists());
    drop(c);
    d.shutdown().await.unwrap();
}

/// Keep restores the lines, labels them `keep` and trashes nothing; a trim labels `selection`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keep_trashes_nothing_and_trim_labels_selection() {
    let env = Env::new();
    let (d, c) = start(&env).await;
    let id = bind(&c, "post.md");
    let a = applied(resolve(
        &c,
        id,
        hash(&env, "post.md"),
        RemovalOp::All {
            verdict: RemovalVerdict::Keep,
        },
    ));
    assert_eq!((a.groups, a.lines_labelled, a.media_queued), (1, 4, 0));
    assert!(a.cleanup_job.is_none());
    assert_eq!(cleanup_jobs(&d), 0);
    assert_eq!(a.restored_at_anchor + a.restored_at_end, 4);
    let text = std::fs::read_to_string(env.vault.join("post.md")).unwrap();
    assert!(text.contains("Sponsored by the synthetic widget company"));
    assert!(labels(&d).iter().all(|r| r.verdict == RemovalVerdict::Keep));
    let p = bind(&c, "plain.md");
    let t = applied(resolve(
        &c,
        p,
        hash(&env, "plain.md"),
        RemovalOp::Trim {
            first: 1,
            last: 1,
            kind: None,
        },
    ));
    assert_eq!(t.lines_labelled, 1);
    assert_eq!(
        std::fs::read_to_string(env.vault.join("plain.md")).unwrap(),
        "# Plain\n"
    );
    let rows = labels(&d);
    let last = rows.last().unwrap();
    assert_eq!(
        (
            last.origin.as_str(),
            last.verdict.as_str(),
            last.kind.as_str(),
            last.publication.as_str()
        ),
        ("selection", "delete", "", "")
    );
    let _ = RemovalKind::Footer;
    std::thread::sleep(Duration::from_millis(200));
    assert!(trash(&d).is_empty());
    drop(c);
    d.shutdown().await.unwrap();
}

/// Next-with-fold: path order after the current note, wrapping, never the current note.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn next_fold_note_orders_wraps_and_ends() {
    let env = Env::new();
    std::fs::write(env.vault.join("b.md"), POST).unwrap();
    std::fs::write(
        env.vault.join("c.md"),
        "# C\n> [!removed]- user callout without groups\n",
    )
    .unwrap();
    let (d, c) = start(&env).await;
    let after = |after: Option<NoteId>| -> Option<String> {
        let v = c
            .request(Request::NextFoldNote(NextFoldNoteParams { after }))
            .unwrap();
        serde_json::from_value::<NextFoldNoteResult>(v)
            .unwrap()
            .path
    };
    let next = |path: Option<&str>| after(path.map(|p| bind(&c, p)));
    assert_eq!(next(None).as_deref(), Some("b.md"));
    assert_eq!(next(Some("b.md")).as_deref(), Some("post.md"));
    assert_eq!(next(Some("c.md")).as_deref(), Some("post.md"));
    assert_eq!(next(Some("post.md")).as_deref(), Some("b.md"), "wraps");
    let id = bind(&c, "b.md");
    applied(resolve(
        &c,
        id,
        hash(&env, "b.md"),
        RemovalOp::All {
            verdict: RemovalVerdict::Keep,
        },
    ));
    assert_eq!(next(Some("post.md")), None, "only the current note has one");
    let pid = bind(&c, "post.md");
    applied(resolve(
        &c,
        pid,
        hash(&env, "post.md"),
        RemovalOp::All {
            verdict: RemovalVerdict::Keep,
        },
    ));
    assert_eq!(next(None), None);
    drop(c);
    d.shutdown().await.unwrap();
}

/// A gone or unknown `after` starts from the first candidate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn next_fold_note_with_a_gone_or_unknown_after_starts_from_the_first() {
    let env = Env::new();
    std::fs::write(env.vault.join("b.md"), POST).unwrap();
    std::fs::write(env.vault.join("m.md"), POST).unwrap();
    let (d, c) = start(&env).await;
    let after = |after: Option<NoteId>| -> Option<String> {
        let v = c
            .request(Request::NextFoldNote(NextFoldNoteParams { after }))
            .unwrap();
        serde_json::from_value::<NextFoldNoteResult>(v)
            .unwrap()
            .path
    };
    assert_eq!(
        after(Some(NoteId(999_999))).as_deref(),
        Some("b.md"),
        "unknown"
    );
    let gone = bind(&c, "m.md");
    assert_eq!(after(Some(gone)).as_deref(), Some("post.md"), "live");
    std::fs::remove_file(env.vault.join("m.md")).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let v = c
            .request(Request::LocateNote(cv_protocol::LocateNoteParams {
                id: gone,
            }))
            .unwrap();
        if v["state"] != "live" {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "m.md never went away");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(after(Some(gone)).as_deref(), Some("b.md"), "gone");
    d.shutdown().await.unwrap();
}
