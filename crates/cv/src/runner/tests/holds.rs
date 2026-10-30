//! Plan round-17 WP-2 / round-18: note holds published and released by the runner.

use super::*;

const HOLDS: &str = r#"
[[action]]
id = "refine"
label = "Refine"
[[action.steps]]
type = "guard"
[[action.steps]]
type = "agent"
agent = "refine-transcript"
output_var = "out"
[[action.steps]]
type = "capture"
target = "active"
mode = "replace"
format = "{{vars.out}}"

[[action]]
id = "free"
label = "Free"
resources = { note = false }
[[action.steps]]
type = "guard"

# Plan round-18 Decision 1: `note = true` but no agent-then-replace: never holds.
[[action]]
id = "plain"
label = "Plain"
[[action.steps]]
type = "capture"
target = "active"
mode = "replace"
format = "x"
"#;

/// `(status, held_note, action_label)` of one job's `JobUpdated` notifications.
type HoldSeen = (JobState, Option<String>, Option<String>);

/// Drains `rx`, grouping `(status, held_note, action_label)` by job id.
fn drain_holds(rx: &mut broadcast::Receiver<Notification>) -> BTreeMap<String, Vec<HoldSeen>> {
    let mut m: BTreeMap<String, Vec<HoldSeen>> = BTreeMap::new();
    while let Ok(n) = rx.try_recv() {
        if let Notification::JobUpdated(s) = n {
            let (path, label) = s
                .hold
                .map_or((None, None), |h| (Some(h.path), Some(h.label)));
            m.entry(s.job_id).or_default().push((s.status, path, label));
        }
    }
    m
}

fn held(status: JobState, path: &str, label: &str) -> HoldSeen {
    (status, Some(path.to_owned()), Some(label.to_owned()))
}

/// Plan round-17 WP-2: a `note = true` action holds its active note while it runs (on `running`,
/// the terminal status and the `held_notes` snapshot); a `note = false` action never holds, nor
/// (plan round-18 Decision 1) a `note = true` one without an agent-then-replace capture.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn note_hold_published_while_running_and_released_after() {
    let f = fixture();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go = Mutex::new(go_rx);
    let exec = FnExec(Box::new(move |_, ctx| {
        if ctx.action.id == "refine" && ctx.step_index == 0 {
            go.lock().unwrap().recv().unwrap();
        }
        Ok(StepOutcome::default())
    }));
    let r = runner(&f, Arc::new(exec), Arc::new(SystemClock), 2, HOLDS);
    let mut rx = f.events.subscribe();
    let refine = enqueue(&r, "refine", Some("a.md"), Lane::User);
    let free = enqueue(&r, "free", Some("b.md"), Lane::User);
    let plain = enqueue(&r, "plain", Some("c.md"), Lane::User);
    let (_stop_tx, stop) = watch::channel(false);
    let _h = r.spawn_claim_loop(stop);
    assert_eq!(wait_terminal(&f, &free).await, DbStatus::Done);
    assert_eq!(wait_terminal(&f, &plain).await, DbStatus::Done);
    let deadline = Instant::now() + Duration::from_secs(10);
    while r.held_notes().is_empty() {
        assert!(Instant::now() < deadline, "hold never appeared");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let snap: Vec<_> = r
        .held_notes()
        .into_iter()
        .map(|s| {
            let (path, label) = s
                .hold
                .map_or((None, None), |h| (Some(h.path), Some(h.label)));
            (s.job_id, s.status, path, label)
        })
        .collect();
    let want = held(JobState::Running, "a.md", "Refine");
    assert_eq!(snap, vec![(refine.clone(), want.0, want.1, want.2)]);
    go_tx.send(()).unwrap();
    assert_eq!(wait_terminal(&f, &refine).await, DbStatus::Done);
    assert!(r.held_notes().is_empty());
    let seen = drain_holds(&mut rx);
    assert_eq!(
        seen[&refine],
        vec![
            (JobState::Pending, None, None),
            held(JobState::Running, "a.md", "Refine"),
            held(JobState::Done, "a.md", "Refine"),
        ]
    );
    assert!(seen[&free].iter().all(|s| s.1.is_none() && s.2.is_none()));
    // Plan round-18 Decision 1: `note = true` without agent-then-replace never holds.
    assert!(seen[&plain].iter().all(|s| s.1.is_none() && s.2.is_none()));
}

/// Plan round-17 WP-2 (audit E3): a move of the held note republishes `running` with the new
/// path; a deferred attempt releases the hold and the retry re-holds it.
#[test]
fn note_hold_follows_a_move_and_is_released_on_deferral() {
    let f = fixture();
    let calls = Arc::new(AtomicUsize::new(0));
    let c2 = Arc::clone(&calls);
    let exec: Arc<dyn StepExecutor> = Arc::new(FnExec(Box::new(move |_, ctx| {
        if ctx.step_index == 1 && c2.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(StepError::Deferred {
                retry_after_ms: 20,
                service: None,
                cause: crate::steps::DeferCause::Wait,
            });
        }
        Ok(if ctx.step_index == 0 {
            moved_outcome()
        } else {
            StepOutcome::default()
        })
    })));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let src = format!("{MOVE_THEN_TAG}{AGENT_REPLACE}");
    let r = runner(&f, exec, Arc::new(SystemClock), 1, &src);
    let mut rx = f.events.subscribe();
    let id = enqueue(&r, "file", Some("inbox/a.md"), Lane::User);
    rt.block_on(async {
        let (_stop_tx, stop) = watch::channel(false);
        let _h = r.spawn_claim_loop(stop);
        assert_eq!(wait_terminal(&f, &id).await, DbStatus::Done);
    });
    assert!(r.held_notes().is_empty());
    let label = r.config().armed("file").unwrap().label.clone();
    let l = label.as_str();
    assert_eq!(
        drain_holds(&mut rx)[&id],
        vec![
            (JobState::Pending, None, None),
            held(JobState::Running, "inbox/a.md", l),
            held(JobState::Running, "stack/a.md", l),
            held(JobState::Deferred, "stack/a.md", l),
            // The retry replays the move checkpoint: the hold starts on the job's original
            // context and follows the replayed move.
            held(JobState::Running, "inbox/a.md", l),
            held(JobState::Running, "stack/a.md", l),
            held(JobState::Done, "stack/a.md", l),
        ]
    );
}

/// Plan round-18 Decision 1 against the shipped examples: `refine-transcript` holds its note,
/// `capture.replace-note` (no agent) does not, and no other example action holds.
#[test]
fn example_config_holds_only_refine_transcript() {
    let c = crate::steps::testkit::example_config();
    let holding: Vec<&str> = c
        .armed_actions()
        .filter(|a| c.action_holds_note(a))
        .map(|a| a.id.as_str())
        .collect();
    assert_eq!(holding, vec!["refine-transcript"]);
    assert!(!c.action_holds_note(c.armed("capture.replace-note").unwrap()));
}
