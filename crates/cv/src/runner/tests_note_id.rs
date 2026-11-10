//! Plan note-identity WP-2 (continuing note-path-continuity WP-2): identity through the live daemon —
//! `locate_note` as a causal barrier, writes and renames by note id, jobs
//! following moved notes by id (note-identity D5) and the cycle guard armed before the write.

use super::*;
use cv_protocol::{
    BindNoteParams, BindNoteResult, LocateNoteParams, NoteId, NoteLocation, WriteNoteParams,
};

fn bind(c: &Client, path: &str) -> NoteId {
    let v = c
        .request(Request::BindNote(BindNoteParams { path: path.into() }))
        .unwrap();
    serde_json::from_value::<BindNoteResult>(v).unwrap().id
}

fn locate(c: &Client, id: NoteId) -> NoteLocation {
    let v = c
        .request(Request::LocateNote(LocateNoteParams { id }))
        .unwrap();
    serde_json::from_value(v).unwrap()
}

fn live(path: &str) -> NoteLocation {
    NoteLocation::Live { path: path.into() }
}

fn rename(c: &Client, from: &str, to: &str, id: Option<NoteId>) -> Result<Value, ClientError> {
    c.request(Request::Rename(cv_protocol::RenameParams {
        from: from.into(),
        to: to.into(),
        id,
    }))
}

fn write_by_id(c: &Client, id: NoteId, path: &str, text: &str, base: &str) -> Value {
    c.request(Request::WriteNote(WriteNoteParams {
        path: path.into(),
        content: text.into(),
        base_hash: base.into(),
        id: Some(id),
    }))
    .unwrap()
}

fn hash_of(text: &str) -> String {
    ContentHash::of(text.as_bytes()).as_str().to_owned()
}

fn rpc_code(r: Result<Value, ClientError>) -> i64 {
    match r {
        Err(ClientError::Rpc(e)) => e.code,
        other => panic!("expected an RPC error, got {other:?}"),
    }
}

/// D2: `bind_note` is stable for one file and refuses a path with no file (never a mint);
/// `locate_note` issued right after a rename observes it, through a chain and a rebuild.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn locate_issued_right_after_a_rename_observes_it() {
    let env = Env::new("");
    let d = start(&env, appending_exec(Arc::new(AtomicUsize::new(0)))).await;
    let c = env.client();
    let id = bind(&c, "a.md");
    assert_eq!(bind(&c, "a.md"), id);
    assert_eq!(locate(&c, id), live("a.md"));
    let missing = c.request(Request::BindNote(BindNoteParams {
        path: "nope.md".into(),
    }));
    assert_eq!(rpc_code(missing), error_codes::NOT_FOUND);
    for (from, to) in [("a.md", "b.md"), ("b.md", "sub/c.md")] {
        rename(&c, from, to, None).unwrap();
    }
    assert_eq!(locate(&c, id), live("sub/c.md"));
    assert_eq!(bind(&c, "sub/c.md"), id);
    d.shared().rebuild_index().await.unwrap();
    assert_eq!(locate(&c, id), live("sub/c.md"));
    assert_eq!(locate(&c, NoteId(u64::MAX >> 2)), NoteLocation::Unknown);
    d.shutdown().await.unwrap();
}

/// D2 invariant: a write by id with a stale path lands at the note's current path and never at
/// a reused one (A moves away, B is created at A's old path; B is untouched).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn write_by_id_lands_at_the_moved_path_never_at_a_reused_one() {
    let env = Env::new("");
    let d = start(&env, appending_exec(Arc::new(AtomicUsize::new(0)))).await;
    let c = env.client();
    let vault = env.vault();
    let a = bind(&c, "a.md");
    rename(&c, "a.md", "moved.md", None).unwrap();
    c.request(Request::CreateNote(cv_protocol::CreateNoteParams {
        path: "a.md".into(),
        content: "# B\n".into(),
    }))
    .unwrap();
    let b = bind(&c, "a.md");
    assert_ne!(a, b);
    let r = write_by_id(&c, a, "a.md", "# A2\n", &hash_of("# A\n"));
    assert_eq!(r["written"]["path"], json!("moved.md"), "{r:?}");
    assert_eq!(
        std::fs::read_to_string(vault.join("moved.md")).unwrap(),
        "# A2\n"
    );
    assert_eq!(
        std::fs::read_to_string(vault.join("a.md")).unwrap(),
        "# B\n"
    );
    // A CAS conflict by id reports the path too, and still writes nothing.
    let r = write_by_id(&c, a, "a.md", "# A3\n", &hash_of("# A\n"));
    assert_eq!(r["conflict"]["path"], json!("moved.md"), "{r:?}");
    // Without an id the reply is today's shape (no path).
    let r = c
        .request(Request::WriteNote(WriteNoteParams {
            path: "a.md".into(),
            content: "# B2\n".into(),
            base_hash: hash_of("# B\n"),
            id: None,
        }))
        .unwrap();
    assert!(r["written"].get("path").is_none(), "{r:?}");
    d.shutdown().await.unwrap();
}

/// D3: a by-id write with the "no file" base revives the retired id at the request's path (the
/// same id, and the write's event carries it); a path held by another file is refused and nothing
/// is overwritten; a retired id with a real base, or an unknown id, is `NOT_FOUND` and writes
/// nothing at the request's path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revive_keeps_the_id_and_refuses_a_taken_path() {
    let env = Env::new("");
    let d = start(&env, appending_exec(Arc::new(AtomicUsize::new(0)))).await;
    let c = env.client();
    let vault = env.vault();
    let mut rx = d.subscribe();
    let a = bind(&c, "a.md");
    c.request(Request::Delete(cv_protocol::DeleteParams {
        id: None,
        path: "a.md".into(),
        mode: cv_protocol::DeleteMode::Permanent,
    }))
    .unwrap();
    assert!(matches!(locate(&c, a), NoteLocation::Gone { .. }));
    // A real base on a gone note: not found, nothing written.
    let r = c.request(Request::WriteNote(WriteNoteParams {
        path: "a.md".into(),
        content: "# A\n".into(),
        base_hash: hash_of("# A\n"),
        id: Some(a),
    }));
    assert_eq!(rpc_code(r), error_codes::NOT_FOUND);
    assert!(!vault.join("a.md").exists());
    // Keep mine: revived at the same id.
    let r = write_by_id(&c, a, "a.md", "# mine\n", "");
    assert_eq!(r["written"]["path"], json!("a.md"), "{r:?}");
    assert_eq!(locate(&c, a), live("a.md"));
    assert_eq!(bind(&c, "a.md"), a);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut event_id = None;
    while Instant::now() < deadline && event_id.is_none() {
        match rx.try_recv() {
            Ok(Notification::FileChanged {
                ref path,
                kind: ChangeKind::Created,
                id,
                ..
            }) if path == "a.md" => event_id = Some(id),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    assert_eq!(
        event_id,
        Some(Some(a)),
        "the revive's create carries the same id"
    );
    // Taken: B retired, another file now at its path; Keep mine is refused, never overwritten.
    let b = bind(&c, "inbox/x.md");
    rename(&c, "inbox/x.md", "inbox/old.md", None).unwrap();
    std::fs::write(vault.join("inbox/x.md"), "# other\n").unwrap();
    rename(&c, "inbox/old.md", "inbox/gone.md", None).unwrap();
    c.request(Request::Delete(cv_protocol::DeleteParams {
        id: None,
        path: "inbox/gone.md".into(),
        mode: cv_protocol::DeleteMode::Permanent,
    }))
    .unwrap();
    assert!(matches!(locate(&c, b), NoteLocation::Gone { .. }));
    let r = write_by_id(&c, b, "inbox/x.md", "# mine\n", "");
    assert!(r.get("conflict").is_some(), "{r:?}");
    assert_eq!(
        std::fs::read_to_string(vault.join("inbox/x.md")).unwrap(),
        "# other\n"
    );
    assert!(matches!(locate(&c, b), NoteLocation::Gone { .. }));
    // Unknown id: not found, nothing created.
    let r = c.request(Request::WriteNote(WriteNoteParams {
        path: "new.md".into(),
        content: "# n\n".into(),
        base_hash: String::new(),
        id: Some(NoteId(u64::MAX >> 2)),
    }));
    assert_eq!(rpc_code(r), error_codes::NOT_FOUND);
    assert!(!vault.join("new.md").exists());
    d.shutdown().await.unwrap();
}

/// D2: a rename by id moves the note from its current path (a stale `from` is ignored), reports
/// the source it used and keeps the id; a gone id is `NOT_FOUND` and renames nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rename_by_id_acts_on_the_current_path() {
    let env = Env::new("");
    let d = start(&env, appending_exec(Arc::new(AtomicUsize::new(0)))).await;
    let c = env.client();
    let vault = env.vault();
    let a = bind(&c, "a.md");
    rename(&c, "a.md", "b.md", None).unwrap();
    std::fs::write(vault.join("a.md"), "# reused\n").unwrap();
    let r = rename(&c, "a.md", "c.md", Some(a)).unwrap();
    assert_eq!(r["from"], json!("b.md"), "{r:?}");
    assert_eq!(locate(&c, a), live("c.md"));
    assert_eq!(
        std::fs::read_to_string(vault.join("a.md")).unwrap(),
        "# reused\n"
    );
    assert!(
        rename(&c, "a.md", "z.md", None)
            .unwrap()
            .get("from")
            .is_none()
    );
    c.request(Request::Delete(cv_protocol::DeleteParams {
        id: None,
        path: "c.md".into(),
        mode: cv_protocol::DeleteMode::Permanent,
    }))
    .unwrap();
    let r = rename(&c, "c.md", "d.md", Some(a));
    assert_eq!(rpc_code(r), error_codes::NOT_FOUND);
    assert!(!vault.join("d.md").exists());
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
        // Plan note-identity D5: every job here holds its note's id.
        assert!(ctx.context.active_id.is_some(), "job bound by id");
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

/// Note-identity D5 (round-23 `clip-enrich` shape), on ids: a first-attempt and a retried job whose note moved
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
