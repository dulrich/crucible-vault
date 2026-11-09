//! Plan note-path-continuity WP-2: lineage through the live daemon — `resolve_path` as a causal
//! barrier, jobs following moved notes (D4) and the cycle guard armed before the write (D5).

use super::*;
use cv_protocol::{PathResolution, ResolvePathParams, ResolvePathResult};

fn resolve(c: &Client, path: &str, since: Option<u64>) -> ResolvePathResult {
    let v = c
        .request(Request::ResolvePath(ResolvePathParams {
            path: path.into(),
            since,
        }))
        .unwrap();
    serde_json::from_value(v).unwrap()
}

/// D2: a `resolve_path` issued right after a `rename` returns observes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resolve_issued_right_after_a_rename_observes_it() {
    let env = Env::new("");
    let d = start(&env, appending_exec(Arc::new(AtomicUsize::new(0)))).await;
    let c = env.client();
    let before = resolve(&c, "a.md", None);
    assert_eq!(
        before.resolution,
        PathResolution::Live {
            path: "a.md".into()
        }
    );
    for (from, to) in [("a.md", "b.md"), ("b.md", "sub/c.md")] {
        c.request(Request::Rename(cv_protocol::RenameParams {
            from: from.into(),
            to: to.into(),
        }))
        .unwrap();
    }
    let after = resolve(&c, "a.md", Some(before.head));
    assert_eq!(
        after.resolution,
        PathResolution::Moved {
            to: "sub/c.md".into()
        }
    );
    assert!(after.head > before.head);
    // Bound after the moves: nothing left `a.md` since, and nothing lives there.
    assert_eq!(
        resolve(&c, "a.md", Some(after.head)).resolution,
        PathResolution::Unknown
    );
    // Lineage survives an index rebuild (it lives in state.db).
    d.shared().rebuild_index().await.unwrap();
    assert_eq!(
        resolve(&c, "a.md", Some(before.head)).resolution,
        after.resolution
    );
    d.shutdown().await.unwrap();
}

const ENRICH: &str = r#"
[[action]]
id = "enrich"
needs = ["active_path"]
[[action.steps]]
type = "set_props"
[[action.steps]]
type = "set_props"
"#;

/// Step 0 plays the concurrent mover named by `params.mode` (through the writer, reporting no
/// move of its own); step 1 appends to the job's active note. Records each step-1 path.
fn mover_exec(seen: Arc<Mutex<Vec<String>>>) -> Arc<dyn StepExecutor> {
    Arc::new(FnExec(Box::new(move |_, ctx| {
        let path = ctx
            .context
            .active_path
            .clone()
            .ok_or_else(|| StepError::failed("no path"))?;
        let mode = ctx.params.get("mode").and_then(Value::as_str).unwrap_or("");
        if ctx.step_index == 0 {
            let w = ctx.writer()?.clone();
            let gone = |p: &str| {
                w.call({
                    let p = p.to_owned();
                    move |vw| vw.fs().hash(&p).ok().flatten().is_none()
                })
            };
            if path == "inbox/x.md" && !gone("inbox/x.md")? {
                let m = mode.to_owned();
                w.call(move |vw| {
                    let h = vw.fs().hash("inbox/x.md").unwrap().unwrap();
                    match m.as_str() {
                        "delete" | "delete_retry" => vw.delete("inbox/x.md", h).unwrap(),
                        _ => {
                            vw.rename("inbox/x.md", "archive/x.md", h).unwrap();
                            if m == "reuse" {
                                vw.create("inbox/x.md", b"# Y\n").unwrap();
                            }
                        }
                    }
                })?;
                if mode.ends_with("_retry") {
                    return Err(StepError::Deferred {
                        retry_after_ms: 1,
                        service: None,
                        cause: crate::steps::DeferCause::Wait,
                    });
                }
            }
            return Ok(StepOutcome::default());
        }
        seen.lock().unwrap().push(path.clone());
        let (mut bytes, hash) = ctx.read_required(&path)?;
        bytes.extend_from_slice(b"touched\n");
        ctx.write(&path, bytes, Some(hash))?;
        Ok(StepOutcome::default())
    })))
}

fn settle(d: &RunningDaemon, job_id: &str) -> cv_state::Job {
    let mut out = None;
    poll(|| {
        let j = d
            .shared()
            .state()
            .lock()
            .unwrap()
            .job(job_id)
            .unwrap()
            .unwrap();
        let done = matches!(
            j.status,
            cv_state::JobStatus::Done | cv_state::JobStatus::Failed
        );
        out = Some(j);
        done
    });
    out.unwrap()
}

/// D4 (round-23 `clip-enrich` shape): a first-attempt and a retried job whose note moved
/// complete at the new path; a deleted note still settles as before (retry: quiet done; first
/// attempt: failed); a path reused after the move is never acted on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn jobs_follow_a_moved_note_and_never_act_on_a_reused_path() {
    let env = Env::new(ENRICH);
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let d = start(&env, mover_exec(Arc::clone(&seen))).await;
    let c = env.client();
    let vault = env.vault();
    let reset = || {
        let _ = std::fs::remove_dir_all(vault.join("archive"));
        std::fs::write(vault.join("inbox/x.md"), "# X\n").unwrap();
        // Let the watcher record the external change before the next enqueue.
        std::thread::sleep(Duration::from_millis(300));
        seen.lock().unwrap().clear();
    };
    let enqueue = |mode: &str| -> String {
        let r = c
            .request(run("enrich", json!({ "mode": mode }), active("inbox/x.md")))
            .unwrap();
        r["job_id"].as_str().unwrap().to_owned()
    };
    for mode in ["move", "move_retry"] {
        reset();
        let j = settle(&d, &enqueue(mode));
        assert_eq!(j.status, cv_state::JobStatus::Done, "{mode}: {:?}", j.error);
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["archive/x.md".to_owned()],
            "{mode}"
        );
        assert_eq!(
            std::fs::read_to_string(vault.join("archive/x.md")).unwrap(),
            "# X\ntouched\n",
            "{mode}"
        );
        assert_eq!(j.attempts > 1, mode.ends_with("_retry"), "{mode}");
    }
    // Path reuse: X moved inbox→archive, Y created at inbox/x.md; the job acts on X only.
    reset();
    let j = settle(&d, &enqueue("reuse"));
    assert_eq!(j.status, cv_state::JobStatus::Done, "{:?}", j.error);
    assert_eq!(*seen.lock().unwrap(), vec!["archive/x.md".to_owned()]);
    assert_eq!(
        std::fs::read_to_string(vault.join("inbox/x.md")).unwrap(),
        "# Y\n",
        "Y untouched"
    );
    // Deleted: a retry settles quietly, a first attempt fails (today's behavior).
    reset();
    let j = settle(&d, &enqueue("delete_retry"));
    assert_eq!(j.status, cv_state::JobStatus::Done, "{:?}", j.error);
    assert!(!vault.join("inbox/x.md").exists());
    reset();
    let j = settle(&d, &enqueue("delete"));
    assert_eq!(j.status, cv_state::JobStatus::Failed);
    assert!(
        j.error.as_deref().unwrap_or("").contains("not found"),
        "{:?}",
        j.error
    );
    d.shutdown().await.unwrap();
}

const WATCH: &str = r#"
[[action]]
id = "watch"
starters = [{ type = "event", kinds = ["created", "modified"], origin = "daemon", selector = { folder = "inbox" } }]
[[action.steps]]
type = "set_props"
"#;

/// D5: the guard is armed before the write is issued. A step held open after its write sees the
/// writer's immediate daemon-origin event reach the starter while it still runs; the guard
/// already blocks the matching starter, and nothing is re-enqueued during or after the hold.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn guard_is_armed_before_the_write_while_the_step_is_held_open() {
    let env = Env::new(WATCH);
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let go = Mutex::new(go_rx);
    let (wrote_tx, wrote_rx) = std::sync::mpsc::channel::<()>();
    let wrote = Mutex::new(wrote_tx);
    let exec: Arc<dyn StepExecutor> = Arc::new(FnExec(Box::new(move |_, ctx| {
        let path = ctx.context.active_path.clone().unwrap_or_default();
        let (mut bytes, hash) = ctx.read_required(&path)?;
        bytes.extend_from_slice(b"touched\n");
        ctx.write(&path, bytes, Some(hash))?;
        let _ = wrote.lock().unwrap().send(());
        // Held open after the write.
        let _ = go.lock().unwrap().recv_timeout(Duration::from_secs(10));
        Ok(StepOutcome::default())
    })));
    let d = start(&env, exec).await;
    let mut rx = d.subscribe();
    // A daemon-origin create in the inbox starts the first run.
    let c = env.client();
    c.request(Request::CreateNote(cv_protocol::CreateNoteParams {
        path: "inbox/n.md".into(),
        content: "# N\n".into(),
    }))
    .unwrap();
    wrote_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    // The step's own write event (daemon origin, immediate) reaches the starter while held.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_echo = false;
    while Instant::now() < deadline && !saw_echo {
        match rx.try_recv() {
            Ok(Notification::FileChanged {
                ref path,
                kind: ChangeKind::Modified,
                origin: Origin::Daemon,
                ..
            }) if path == "inbox/n.md" => saw_echo = true,
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    assert!(saw_echo, "the write's event arrived while the step is held");
    // The guard is already armed although the executor has not returned (a pending/running
    // dedupe key would mask a re-enqueue, so the guard itself is asserted).
    assert!(
        d.shared()
            .runner()
            .unwrap()
            .cycle_blocked("watch", "inbox/n.md"),
        "guard armed before the executor returned"
    );
    std::thread::sleep(Duration::from_millis(400));
    let jobs = || {
        d.shared()
            .state()
            .lock()
            .unwrap()
            .list(&cv_state::ListQuery::default())
            .unwrap()
    };
    assert_eq!(jobs().len(), 1, "no re-enqueue while the step is held");
    go_tx.send(()).unwrap();
    let id = jobs()[0].id.clone();
    let j = settle(&d, &id);
    assert_eq!(j.status, cv_state::JobStatus::Done);
    assert_eq!(jobs().len(), 1, "no re-enqueue after it returns");
    d.shutdown().await.unwrap();
}
