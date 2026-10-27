//! Runner tests: synthetic config, temp `state.db`, injected executors and clocks (I1).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cv_actions::{ConfigSet, Step, StepKind};
use cv_protocol::Lane;
use cv_protocol::{ActionContext, JobState, Notification};
use cv_state::{JobStatus as DbStatus, QueueConfig, StateDb};
use serde_json::Map;
use tokio::sync::{broadcast, watch};

use super::*;

type Behavior = dyn Fn(&Step, &mut StepCtx<'_>) -> Result<StepOutcome, StepError> + Send + Sync;

struct FnExec(Box<Behavior>);

impl StepExecutor for FnExec {
    fn execute(&self, step: &Step, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        (self.0)(step, ctx)
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    state: Arc<Mutex<StateDb>>,
    events: broadcast::Sender<Notification>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let state = StateDb::open(&dir.path().join("state.db"), 1, QueueConfig::default()).unwrap();
    let (events, _) = broadcast::channel(256);
    Fixture {
        _dir: dir,
        state: Arc::new(Mutex::new(state)),
        events,
    }
}

fn config(src: &str) -> ConfigSet {
    let mut c = ConfigSet::new();
    let files = BTreeMap::from([("actions.toml".to_owned(), Some(src.to_owned()))]);
    c.reload(&files, &crate::steps::validate);
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    c
}

fn runner(
    f: &Fixture,
    exec: Arc<dyn StepExecutor>,
    clock: Arc<dyn Clock>,
    concurrency: usize,
    src: &str,
) -> Runner {
    let r = Runner::new(RunnerDeps {
        state: Arc::clone(&f.state),
        writer: Arc::new(None::<WriterHandle>),
        events: f.events.clone(),
        executor: exec,
        clock,
        concurrency,
        vault_root: f._dir.path().to_path_buf(),
        index_db: f._dir.path().join("index.db"),
        config_dir: f._dir.path().join("config"),
        search_db: f._dir.path().join("search.db"),
        data_home: None,
    });
    r.set_config(config(src));
    r
}

fn req(path: Option<&str>, lane: Lane) -> RunRequest {
    let context = ActionContext {
        active_path: path.map(str::to_owned),
        ..ActionContext::default()
    };
    RunRequest {
        default_lane: lane,
        ..RunRequest::command(Map::new(), context)
    }
}

fn enqueue(r: &Runner, id: &str, path: Option<&str>, lane: Lane) -> String {
    let cfg = r.config();
    let a = cfg.armed(id).unwrap();
    r.enqueue(a, req(path, lane)).unwrap()
}

fn status(f: &Fixture, id: &str) -> DbStatus {
    f.state.lock().unwrap().job(id).unwrap().unwrap().status
}

async fn wait_terminal(f: &Fixture, id: &str) -> DbStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let s = status(f, id);
        if s.is_terminal() {
            return s;
        }
        assert!(Instant::now() < deadline, "job {id} stuck in {s:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

const ONE_STEP: &str = r#"
[[action]]
id = "touch"
[[action.steps]]
type = "set_props"

[[action]]
id = "look"
[[action.steps]]
type = "guard"
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn user_lane_claimed_before_background_and_job_updated_published() {
    let f = fixture();
    let order = Arc::new(Mutex::new(Vec::new()));
    let o2 = Arc::clone(&order);
    let exec = FnExec(Box::new(move |_, ctx| {
        o2.lock()
            .unwrap()
            .push(ctx.context.active_path.clone().unwrap_or_default());
        Ok(StepOutcome::default())
    }));
    let r = runner(&f, Arc::new(exec), Arc::new(SystemClock), 1, ONE_STEP);
    let mut rx = f.events.subscribe();
    let bg = enqueue(&r, "look", Some("bg.md"), Lane::Background);
    let user = enqueue(&r, "look", Some("user.md"), Lane::User);
    let (_stop_tx, stop) = watch::channel(false);
    let _h = r.spawn_claim_loop(stop);
    assert_eq!(wait_terminal(&f, &bg).await, DbStatus::Done);
    assert_eq!(wait_terminal(&f, &user).await, DbStatus::Done);
    assert_eq!(*order.lock().unwrap(), vec!["user.md", "bg.md"]);
    let mut seen = Vec::new();
    while let Ok(n) = rx.try_recv() {
        if let Notification::JobUpdated(s) = n
            && s.job_id == user
        {
            seen.push(s.status);
        }
    }
    assert_eq!(
        seen,
        vec![JobState::Pending, JobState::Running, JobState::Done]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shared_resource_keys_never_run_concurrently() {
    let f = fixture();
    let live: Arc<Mutex<BTreeMap<String, usize>>> = Arc::default();
    let max_same = Arc::new(AtomicUsize::new(0));
    let max_total = Arc::new(AtomicUsize::new(0));
    let total = Arc::new(AtomicUsize::new(0));
    let (l2, ms, mt, t2) = (
        Arc::clone(&live),
        Arc::clone(&max_same),
        Arc::clone(&max_total),
        Arc::clone(&total),
    );
    let exec = FnExec(Box::new(move |_, ctx| {
        let path = ctx.context.active_path.clone().unwrap();
        {
            let mut m = l2.lock().unwrap();
            let n = m.entry(path.clone()).or_default();
            *n += 1;
            ms.fetch_max(*n, Ordering::SeqCst);
        }
        let now = t2.fetch_add(1, Ordering::SeqCst) + 1;
        mt.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(80));
        t2.fetch_sub(1, Ordering::SeqCst);
        *l2.lock().unwrap().get_mut(&path).unwrap() -= 1;
        Ok(StepOutcome::default())
    }));
    let r = runner(&f, Arc::new(exec), Arc::new(SystemClock), 2, ONE_STEP);
    let ids: Vec<String> = ["a.md", "a.md", "a.md", "b.md"]
        .iter()
        .map(|p| enqueue(&r, "touch", Some(p), Lane::User))
        .collect();
    let (_stop_tx, stop) = watch::channel(false);
    let _h = r.spawn_claim_loop(stop);
    for id in &ids {
        assert_eq!(wait_terminal(&f, id).await, DbStatus::Done);
    }
    assert_eq!(max_same.load(Ordering::SeqCst), 1, "I3 violated");
    assert_eq!(
        max_total.load(Ordering::SeqCst),
        2,
        "disjoint keys run in parallel"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mutating_action_without_keys_is_refused() {
    let f = fixture();
    let r = runner(
        &f,
        Arc::new(ProductionExecutor),
        Arc::new(SystemClock),
        1,
        ONE_STEP,
    );
    let cfg = r.config();
    let err = r
        .enqueue(cfg.armed("touch").unwrap(), req(None, Lane::User))
        .unwrap_err();
    assert!(matches!(err, EnqueueError::NoResourceKeys(_)));
    assert!(f.state.lock().unwrap().counts().unwrap().is_empty());
}

const INTAKE: &str = r#"
[[action]]
id = "tb"
resources = { note = false, keys = [] }
[[action.steps]]
type = "track_blogs"
"#;

/// Plan auto-intake-sprint WP-3 (audit M3): a `track_blogs` action with no configured keys still
/// derives the step-intrinsic `intake::identity` (plus `vault::attachments`), and two such jobs never run concurrently.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn intake_identity_key_is_intrinsic_and_serializes() {
    let f = fixture();
    let live = Arc::new(AtomicUsize::new(0));
    let max = Arc::new(AtomicUsize::new(0));
    let (l2, m2) = (Arc::clone(&live), Arc::clone(&max));
    let exec = FnExec(Box::new(move |_, _| {
        let n = l2.fetch_add(1, Ordering::SeqCst) + 1;
        m2.fetch_max(n, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(80));
        l2.fetch_sub(1, Ordering::SeqCst);
        Ok(StepOutcome::default())
    }));
    let r = runner(&f, Arc::new(exec), Arc::new(SystemClock), 2, INTAKE);
    let ids: Vec<String> = (0..2)
        .map(|_| enqueue(&r, "tb", None, Lane::Background))
        .collect();
    let job = f.state.lock().unwrap().job(&ids[0]).unwrap().unwrap();
    assert_eq!(
        job.resource_keys,
        vec![cv_actions::INTAKE_IDENTITY_KEY, cv_actions::ATTACHMENTS_KEY]
    );
    let (_stop_tx, stop) = watch::channel(false);
    let _h = r.spawn_claim_loop(stop);
    for id in &ids {
        assert_eq!(wait_terminal(&f, id).await, DbStatus::Done);
    }
    assert_eq!(
        max.load(Ordering::SeqCst),
        1,
        "intake jobs ran concurrently"
    );
}

const THREE: &str = r#"
[[action]]
id = "three"
[[action.steps]]
type = "guard"
[[action.steps]]
type = "guard"
[[action.steps]]
type = "guard"
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_stops_before_next_step() {
    let f = fixture();
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let entered_tx = Mutex::new(entered_tx);
    let release_rx = Mutex::new(release_rx);
    let calls = Arc::new(AtomicUsize::new(0));
    let c2 = Arc::clone(&calls);
    let exec = FnExec(Box::new(move |_, ctx| {
        c2.fetch_add(1, Ordering::SeqCst);
        if ctx.step_index == 0 {
            entered_tx.lock().unwrap().send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
        }
        Ok(StepOutcome::default())
    }));
    let r = runner(&f, Arc::new(exec), Arc::new(SystemClock), 1, THREE);
    let id = enqueue(&r, "three", None, Lane::User);
    let (_stop_tx, stop) = watch::channel(false);
    let _h = r.spawn_claim_loop(stop);
    tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
        .await
        .unwrap();
    assert!(r.request_cancel(&id));
    release_tx.send(()).unwrap();
    assert_eq!(wait_terminal(&f, &id).await, DbStatus::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!r.request_cancel(&id), "no longer running");
}

/// Plan round-11 Decision 3: a retried job whose note is gone settles quietly, never Failed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_after_the_note_is_gone_settles_done_not_failed() {
    let f = fixture();
    let calls = Arc::new(AtomicUsize::new(0));
    let c2 = Arc::clone(&calls);
    let exec = FnExec(Box::new(move |_, _| {
        if c2.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(StepError::Deferred {
                retry_after_ms: 20,
                service: None,
                cause: crate::steps::DeferCause::Wait,
            });
        }
        Err(StepError::NoteGone {
            path: "Clippings/old.md".into(),
        })
    }));
    let r = runner(&f, Arc::new(exec), Arc::new(SystemClock), 1, ONE_STEP);
    let id = enqueue(&r, "look", Some("Clippings/old.md"), Lane::User);
    let (_stop_tx, stop) = watch::channel(false);
    let _h = r.spawn_claim_loop(stop);
    assert_eq!(wait_terminal(&f, &id).await, DbStatus::Done);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// Audit E2 (pin 6): a crash mid-action never re-runs an applied step.
#[test]
fn crash_mid_action_resumes_at_first_unapplied_step() {
    let f = fixture();
    let counts: Arc<[AtomicUsize; 3]> = Arc::new(Default::default());
    let crashed = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let entered_tx = Mutex::new(entered_tx);
    let release_rx = Mutex::new(release_rx);
    let (c2, cr2) = (Arc::clone(&counts), Arc::clone(&crashed));
    let exec: Arc<dyn StepExecutor> = Arc::new(FnExec(Box::new(move |_, ctx| {
        c2[ctx.step_index].fetch_add(1, Ordering::SeqCst);
        if ctx.step_index == 1 && cr2.fetch_add(1, Ordering::SeqCst) == 0 {
            // First attempt at step 2: the process "dies" here (the runtime is torn down while
            // we block; this thread's result is never settled).
            entered_tx.lock().unwrap().send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
            return Err(StepError::failed("process gone"));
        }
        Ok(StepOutcome::default())
    })));
    let id;
    {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let r = runner(&f, Arc::clone(&exec), Arc::new(SystemClock), 1, THREE);
        id = enqueue(&r, "three", None, Lane::User);
        let (stop_tx, stop) = watch::channel(false);
        rt.block_on(async {
            let _h = r.spawn_claim_loop(stop);
        });
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        rt.shutdown_background();
        drop(stop_tx);
        release_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(status(&f, &id), DbStatus::Running, "never settled: crashed");
    assert_eq!(f.state.lock().unwrap().checkpoints(&id).unwrap().len(), 1);
    // The restarted process opens state.db afresh: a new claim token, so the dead lease is
    // foreign and startup recovery requeues it.
    *f.state.lock().unwrap() =
        StateDb::open(&f._dir.path().join("state.db"), 2, QueueConfig::default()).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let r = runner(&f, exec, Arc::new(SystemClock), 1, THREE);
    rt.block_on(async {
        let (_stop_tx, stop) = watch::channel(false);
        let _h = r.spawn_claim_loop(stop);
        assert_eq!(wait_terminal(&f, &id).await, DbStatus::Done);
    });
    let c: Vec<usize> = counts.iter().map(|a| a.load(Ordering::SeqCst)).collect();
    assert_eq!(
        c,
        vec![1, 2, 1],
        "step 1 exactly once; step 2 re-run after the crash"
    );
    assert_eq!(f.state.lock().unwrap().checkpoints(&id).unwrap().len(), 3);
}

const MOVE_THEN_TAG: &str = r#"
[[action]]
id = "file"
needs = ["active_path"]
[[action.steps]]
type = "move"
to = "stack"
[[action.steps]]
type = "upsert_tags"
tags = "x"
[[action.steps]]
type = "guard"
path = "keep.md"
"#;

/// `(step index, active path the step saw)`.
type Seen = (usize, Option<String>);

/// The `move` stand-in: reports `inbox/a.md` → `stack/a.md` like `steps::file::move_note`.
fn moved_outcome() -> StepOutcome {
    let mut o = StepOutcome {
        open_path: Some("stack/a.md".into()),
        ..StepOutcome::default()
    };
    o.vars
        .insert("moved_path".into(), serde_json::json!("stack/a.md"));
    o.vars
        .insert(steps::MOVED_FROM.into(), serde_json::json!("inbox/a.md"));
    o
}

/// WP-8: after a step moves the active note, later steps see the moved path as the active note
/// — on a fresh run and when the move is replayed from a checkpoint on resume.
#[test]
fn active_path_follows_a_move_fresh_and_on_resume() {
    for resume in [false, true] {
        let f = fixture();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let s2 = Arc::clone(&seen);
        let exec: Arc<dyn StepExecutor> = Arc::new(FnExec(Box::new(move |_, ctx| {
            s2.lock()
                .unwrap()
                .push((ctx.step_index, ctx.context.active_path.clone()));
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
        let r = runner(&f, exec, Arc::new(SystemClock), 1, MOVE_THEN_TAG);
        let mut rx = f.events.subscribe();
        let id = enqueue(&r, "file", Some("inbox/a.md"), Lane::User);
        if resume {
            let text = serde_json::to_string(&moved_outcome()).unwrap();
            f.state
                .lock()
                .unwrap()
                .put_checkpoint(&id, 0, &text, 0)
                .unwrap();
        }
        rt.block_on(async {
            let (_stop_tx, stop) = watch::channel(false);
            let _h = r.spawn_claim_loop(stop);
            assert_eq!(wait_terminal(&f, &id).await, DbStatus::Done);
        });
        let got = seen.lock().unwrap().clone();
        let mut want = vec![
            (1, Some("stack/a.md".to_owned())),
            (2, Some("stack/a.md".to_owned())),
        ];
        if !resume {
            want.insert(0, (0, Some("inbox/a.md".to_owned())));
        }
        assert_eq!(got, want, "resume={resume}");
        let mut open = None;
        while let Ok(n) = rx.try_recv() {
            if let Notification::JobUpdated(s) = n
                && s.job_id == id
                && s.status == JobState::Done
            {
                open = s.open_path;
            }
        }
        assert_eq!(open.as_deref(), Some("stack/a.md"), "resume={resume}");
    }
}

const SCHEDULED: &str = r#"
[[action]]
id = "hourly"
starters = [{ type = "schedule", every = "1h" }]
[[action.steps]]
type = "guard"
"#;

#[test]
fn schedule_catches_up_once_after_missed_periods() {
    let f = fixture();
    const H: i64 = 3_600_000;
    let clock = Arc::new(ManualClock::new(100 * H + 10));
    let r = runner(
        &f,
        Arc::new(ProductionExecutor),
        Arc::clone(&clock) as Arc<dyn Clock>,
        1,
        SCHEDULED,
    );
    assert!(
        r.schedule_tick().is_empty(),
        "first sight seeds, never fires"
    );
    assert!(r.schedule_tick().is_empty());
    // Daemon down for five periods; a fresh runner (restart) catches up exactly once.
    clock.set(105 * H + 20);
    let r2 = runner(
        &f,
        Arc::new(ProductionExecutor),
        Arc::clone(&clock) as Arc<dyn Clock>,
        1,
        SCHEDULED,
    );
    let fired = r2.schedule_tick();
    assert_eq!(fired.len(), 1);
    assert!(r2.schedule_tick().is_empty(), "no second catch-up");
    assert_eq!(
        f.state.lock().unwrap().schedule_last_run("hourly").unwrap(),
        Some(105 * H + 20)
    );
    let job = f.state.lock().unwrap().job(&fired[0]).unwrap().unwrap();
    assert_eq!(job.lane, Lane::Background);
    clock.set(106 * H);
    assert_eq!(
        r2.schedule_tick().len(),
        1,
        "the next period fires normally"
    );
}

const CHAIN: &str = r#"
[[action]]
id = "outer"
[[action.steps]]
type = "run_action"
action = "inner"
params = { x = "1" }

[[action]]
id = "inner"
[[action.steps]]
type = "guard"

[[action]]
id = "loop"
[[action.steps]]
type = "run_action"
action = "loop"
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_action_step_enqueues_child_and_rejects_cycles() {
    let f = fixture();
    let ran = Arc::new(Mutex::new(Vec::new()));
    let r2 = Arc::clone(&ran);
    let exec = FnExec(Box::new(move |step, ctx| {
        r2.lock().unwrap().push(ctx.action.id.clone());
        match step {
            s if s.kind == StepKind::RunAction => crate::steps::execute(s, ctx),
            _ => Ok(StepOutcome::default()),
        }
    }));
    let r = runner(&f, Arc::new(exec), Arc::new(SystemClock), 2, CHAIN);
    let outer = enqueue(&r, "outer", None, Lane::User);
    let looped = enqueue(&r, "loop", None, Lane::User);
    let (_stop_tx, stop) = watch::channel(false);
    let _h = r.spawn_claim_loop(stop);
    assert_eq!(wait_terminal(&f, &outer).await, DbStatus::Done);
    assert_eq!(wait_terminal(&f, &looped).await, DbStatus::Failed);
    let err = f.state.lock().unwrap().job(&looped).unwrap().unwrap().error;
    assert!(err.unwrap().contains("cycle"));
    let cp = f.state.lock().unwrap().checkpoints(&outer).unwrap();
    let o: StepOutcome = serde_json::from_str(&cp[0].outcome).unwrap();
    let child = o.vars["child_job_id"].as_str().unwrap().to_owned();
    assert_eq!(wait_terminal(&f, &child).await, DbStatus::Done);
    let cj = f.state.lock().unwrap().job(&child).unwrap().unwrap();
    assert_eq!(cj.lane, Lane::User, "child inherits the lane");
    let jp: JobParams = serde_json::from_value(cj.params).unwrap();
    assert_eq!(jp.chain, vec!["outer".to_owned()]);
    assert_eq!(jp.params["x"], "1");
}

const EVENTED: &str = r#"
[[action]]
id = "watch"
starters = [{ type = "event", kinds = ["modified"], origin = "any", selector = { folder = "inbox" } }]
[[action.steps]]
type = "set_props"
"#;

#[test]
fn event_selector_origin_and_cycle_guard() {
    use cv_protocol::{ChangeKind, Origin};
    let f = fixture();
    let r = runner(
        &f,
        Arc::new(ProductionExecutor),
        Arc::new(SystemClock),
        1,
        EVENTED,
    );
    assert!(events::on_change(&r, "other/a.md", ChangeKind::Modified, Origin::External).is_empty());
    assert!(events::on_change(&r, "inbox/a.md", ChangeKind::Created, Origin::External).is_empty());
    let first = events::on_change(&r, "inbox/a.md", ChangeKind::Modified, Origin::External);
    assert_eq!(first.len(), 1);
    // Dedupe key `<action>::<path>`: a pending run absorbs the repeat.
    assert_eq!(
        events::on_change(&r, "inbox/a.md", ChangeKind::Modified, Origin::External),
        first
    );
    // A job of `watch` wrote inbox/b.md → its daemon echo does not re-trigger `watch`.
    r.inner().record_writes("watch", &["inbox/b.md".to_owned()]);
    assert!(events::on_change(&r, "inbox/b.md", ChangeKind::Modified, Origin::Daemon).is_empty());
    assert_eq!(
        events::on_change(&r, "inbox/b.md", ChangeKind::Modified, Origin::External).len(),
        1,
        "an external edit still fires"
    );
}
const CREATED_ONLY: &str = r#"
[[action]]
id = "clip"
starters = [{ type = "event", kinds = ["created"], origin = "any", selector = { folder = "Clippings" } }]
[[action.steps]]
type = "set_props"
"#;

/// Plan intake-round-2-clean-dedupe-reading WP-5a: a daemon move into a `["created"]` starter's
/// folder arrives as `Renamed` and does not fire it; a genuine create still does.
#[test]
fn created_starter_ignores_a_daemon_move_into_its_folder() {
    use cv_protocol::{ChangeKind, Origin};
    let f = fixture();
    let r = runner(
        &f,
        Arc::new(ProductionExecutor),
        Arc::new(SystemClock),
        1,
        CREATED_ONLY,
    );
    assert!(
        events::on_change(&r, "Clippings/m.md", ChangeKind::Renamed, Origin::Daemon).is_empty()
    );
    assert_eq!(
        events::on_change(&r, "Clippings/n.md", ChangeKind::Created, Origin::Daemon).len(),
        1
    );
}

/// Round-12 WP-1: an attachment create under a folder selector starts nothing; a note still does.
#[test]
fn folder_selector_ignores_non_note_paths() {
    use cv_protocol::{ChangeKind, Origin};
    let f = fixture();
    let r = runner(
        &f,
        Arc::new(ProductionExecutor),
        Arc::new(SystemClock),
        1,
        CREATED_ONLY,
    );
    assert!(
        events::on_change(
            &r,
            "Clippings/p.webp",
            ChangeKind::Created,
            Origin::External
        )
        .is_empty()
    );
    assert_eq!(
        events::on_change(&r, "Clippings/p.md", ChangeKind::Created, Origin::External).len(),
        1
    );
}

mod queue;

/// WP-5a proposal 1: the bridge reports a rename's source as `Deleted`; no example starter
/// listens to `deleted`, so that extra event fires nothing.
#[test]
fn no_example_starter_fires_on_a_rename_source_deleted() {
    use cv_protocol::{ChangeKind, Origin};
    let f = fixture();
    let r = runner(
        &f,
        Arc::new(ProductionExecutor),
        Arc::new(SystemClock),
        1,
        EVENTED,
    );
    load_examples(&r);
    for p in [
        "Clippings/x.md",
        "inbox/x.md",
        "daily/x.md",
        "_yt_metadata/x.md",
        "x.md",
    ] {
        for o in [Origin::Daemon, Origin::External] {
            assert!(
                events::on_change(&r, p, ChangeKind::Deleted, o).is_empty(),
                "{p}"
            );
        }
    }
    // Sanity: the same config does fire on a create in Clippings.
    assert!(
        !events::on_change(&r, "Clippings/y.md", ChangeKind::Created, Origin::Daemon).is_empty()
    );
}

/// Loads every `config/examples/*.toml` into `r`.
fn load_examples(r: &Runner) {
    let mut c = ConfigSet::new();
    let files: BTreeMap<String, Option<String>> = [
        (
            "actions.toml",
            include_str!("../../../../config/examples/actions.toml"),
        ),
        (
            "captures.toml",
            include_str!("../../../../config/examples/captures.toml"),
        ),
        (
            "agents.toml",
            include_str!("../../../../config/examples/agents.toml"),
        ),
        (
            "kinds.toml",
            include_str!("../../../../config/examples/kinds.toml"),
        ),
        (
            "providers.toml",
            include_str!("../../../../config/examples/providers.toml"),
        ),
        (
            "pipelines.toml",
            include_str!("../../../../config/examples/pipelines.toml"),
        ),
        (
            "schedules.toml",
            include_str!("../../../../config/examples/schedules.toml"),
        ),
        (
            "sources.toml",
            include_str!("../../../../config/examples/sources.toml"),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), Some(v.to_owned())))
    .collect();
    c.reload(&files, &crate::steps::validate);
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    r.set_config(c);
}

/// Writes `files` (path, text) under the fixture root and builds its `index.db` so tag selectors resolve.
fn index_notes(f: &Fixture, files: &[(&str, &str)]) {
    let root = f._dir.path();
    for (p, t) in files {
        let abs = root.join(p);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, t).unwrap();
    }
    cv_index::Index::create(root, &root.join("index.db")).unwrap();
}

/// Plan intake-round-2-clean-dedupe-reading WP-5b: the example `clip-images` fires on a
/// `clippings`-tagged note in a day folder, once (not twice) for one in `Clippings/`, and
/// `clip-auto-file` has no event starter.
#[test]
fn clip_images_tag_starter_fires_once_and_clip_auto_file_never() {
    use cv_protocol::{ChangeKind, Origin};
    let f = fixture();
    let tagged = "---\ntags:\n  - clippings\n---\nbody\n";
    index_notes(
        &f,
        &[
            ("Clippings/a.md", tagged),
            ("daily/day/2026-10-21/b.md", tagged),
            (
                "daily/day/2026-10-21/c.md",
                "---\ntags:\n  - other\n---\nbody\n",
            ),
        ],
    );
    let r = runner(
        &f,
        Arc::new(ProductionExecutor),
        Arc::new(SystemClock),
        1,
        EVENTED,
    );
    load_examples(&r);
    let ids = |p: &str| events::on_change(&r, p, ChangeKind::Created, Origin::Daemon);
    let jobs = |ids: &[String]| -> Vec<String> {
        ids.iter()
            .map(|id| {
                f.state
                    .lock()
                    .unwrap()
                    .job(id)
                    .unwrap()
                    .unwrap()
                    .dedupe_key
                    .unwrap_or_default()
            })
            .collect()
    };
    // Matches both starters, one `clip-images` run (plus the folder-only `x-discover`).
    let both = jobs(&ids("Clippings/a.md"));
    assert_eq!(
        both.iter().filter(|a| a.contains("clip-images::")).count(),
        1
    );
    assert!(both.iter().all(|a| !a.contains("clip-auto-file")));
    let day = jobs(&ids("daily/day/2026-10-21/b.md"));
    assert_eq!(
        day.iter().filter(|a| a.contains("clip-images::")).count(),
        1
    );
    assert!(jobs(&ids("daily/day/2026-10-21/c.md")).is_empty());
}

/// WP-5b no-event-loop proof: `file-as-kind`'s daemon move arrives as `Renamed` (WP-5a:
/// `crates/cv-vault/src/watcher.rs` move-echo classification, bridge `file_changes`), so the
/// `["created"]` tag starter does not re-fire `clip-images` for the filed note, while a genuine
/// create still does.
#[test]
fn clip_images_tag_starter_ignores_the_daemon_move_that_files_the_clip() {
    use cv_protocol::{ChangeKind, Origin};
    let f = fixture();
    index_notes(
        &f,
        &[(
            "daily/day/2026-10-21/f.md",
            "---\ntags:\n  - clippings\n  - fanfic\n---\nbody\n",
        )],
    );
    let r = runner(
        &f,
        Arc::new(ProductionExecutor),
        Arc::new(SystemClock),
        1,
        EVENTED,
    );
    load_examples(&r);
    let p = "daily/day/2026-10-21/f.md";
    assert!(events::on_change(&r, p, ChangeKind::Renamed, Origin::Daemon).is_empty());
    assert!(events::on_change(&r, p, ChangeKind::Deleted, Origin::Daemon).is_empty());
    assert!(!events::on_change(&r, p, ChangeKind::Created, Origin::Daemon).is_empty());
}
