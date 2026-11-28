//! Stage-seam tests (plan intake-stage-jobs WP-1): admission, service at enqueue, reload re-tag,
//! no terminal outage, the guard and breaker credit only on a real attempt. Synthetic config,
//! temp `state.db`, injected executors and a manual clock; claims are driven one at a time.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cv_actions::{ConfigSet, Step, StepKind};
use cv_protocol::{BreakerState, Lane};
use cv_state::{JobStatus as DbStatus, QueueConfig, ServiceFailureKind, StateDb};
use serde_json::Map;
use tokio::sync::broadcast;

use super::*;

type Behavior = dyn Fn(&Step, &mut StepCtx<'_>) -> Result<StepOutcome, StepError> + Send + Sync;

struct FnExec(Box<Behavior>);

impl StepExecutor for FnExec {
    fn execute(&self, step: &Step, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        (self.0)(step, ctx)
    }
}

/// Counts calls; each returns `Ok` with `attempted` as given.
fn counting(calls: &Arc<AtomicUsize>, attempted: bool) -> Arc<dyn StepExecutor> {
    let c = Arc::clone(calls);
    Arc::new(FnExec(Box::new(move |_, _| {
        c.fetch_add(1, Ordering::SeqCst);
        Ok(StepOutcome {
            attempted,
            ..StepOutcome::default()
        })
    })))
}

const LOC: &str = "localize";
const YT: &str = cv_providers::web::youtube::SERVICE;
const T0: i64 = 1_000_000;
/// Past every breaker open window.
const LATER: i64 = 11 * 60_000;

/// Stage action `stage-x` whose one step is `step` (A = `localize`, B = `yt_metadata`), plus a
/// second stage action on the same note, an unmarked action and a non-static stage action.
fn actions(step: &str) -> String {
    format!(
        r#"
[[action]]
id = "stage-x"
stage = "localize"
[[action.steps]]
type = "{step}"

[[action]]
id = "stage-tidy"
stage = "yt_clip_tidy"
[[action.steps]]
type = "set_props"

[[action]]
id = "plain"
[[action.steps]]
type = "set_props"
"#
    )
}

fn files(actions: &str, pipelines: &str) -> BTreeMap<String, Option<String>> {
    BTreeMap::from([
        ("actions.toml".to_owned(), Some(actions.to_owned())),
        (
            "agents.toml".to_owned(),
            Some(include_str!("../../../../config/examples/agents.toml").to_owned()),
        ),
        (
            "providers.toml".to_owned(),
            Some(include_str!("../../../../config/examples/providers.toml").to_owned()),
        ),
        ("pipelines.toml".to_owned(), Some(pipelines.to_owned())),
    ])
}

fn config(actions: &str) -> ConfigSet {
    let mut c = ConfigSet::new();
    c.reload(&files(actions, ""), &crate::steps::validate);
    config_load::disarm_unkeyed_stage_actions(&mut c);
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    c
}

struct Fx {
    dir: tempfile::TempDir,
    state: Arc<Mutex<StateDb>>,
    clock: Arc<ManualClock>,
    runner: Runner,
}

fn fx(exec: Arc<dyn StepExecutor>, step: &str) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(Mutex::new(
        StateDb::open(&dir.path().join("state.db"), 1, QueueConfig::default()).unwrap(),
    ));
    let (events, _) = broadcast::channel(256);
    let clock = Arc::new(ManualClock::new(T0));
    let runner = Runner::new(RunnerDeps {
        state: Arc::clone(&state),
        writer: Arc::new(None::<WriterHandle>),
        events,
        executor: exec,
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        concurrency: 2,
        fast_slots: 0,
        vault_root: dir.path().to_path_buf(),
        index_db: dir.path().join("index.db"),
        config_dir: dir.path().join("config"),
        search_db: dir.path().join("search.db"),
        data_home: None,
    });
    runner.set_config(config(&actions(step)));
    Fx {
        dir,
        state,
        clock,
        runner,
    }
}

impl Fx {
    fn bind(&self, path: &str) -> cv_protocol::NoteId {
        self.state.lock().unwrap().note_id_bind(path).unwrap()
    }

    fn admit(&self, action: &str, path: &str) -> Admission {
        self.runner.admit_stage(action, path).unwrap()
    }

    fn job(&self, id: &str) -> cv_state::Job {
        self.state.lock().unwrap().job(id).unwrap().unwrap()
    }

    fn health(&self, svc: &str) -> cv_state::ServiceHealth {
        self.state.lock().unwrap().service_health(svc).unwrap()
    }

    fn open(&self, svc: &str) {
        let mut db = self.state.lock().unwrap();
        for _ in 0..3 {
            db.report_service_failure(svc, ServiceFailureKind::Timeout, "down", None, T0)
                .unwrap();
        }
        assert_eq!(db.service_health(svc).unwrap().state, BreakerState::Open);
    }

    /// Claims at the clock's now and runs the job to its settle; the claimed id.
    async fn claim_run(&self) -> Option<String> {
        let inner = Arc::clone(self.runner.inner());
        let job = self
            .state
            .lock()
            .unwrap()
            .claim(self.clock.now_ms())
            .unwrap()?;
        let id = job.id.clone();
        run_job(inner, job).await;
        Some(id)
    }
}

fn id_of(a: &Admission) -> String {
    match a {
        Admission::Inserted(id) | Admission::Duplicate(id) => id.clone(),
    }
}

/// Two admissions of one stage action and note insert one job: chain-free, tagged at enqueue,
/// keyed by the note id; an unmarked action and an unbound note are refused.
#[test]
fn two_admissions_insert_one_job_and_refusals_are_named() {
    let f = fx(counting(&Arc::new(AtomicUsize::new(0)), true), "localize");
    let note = f.bind("inbox/a.md");
    let first = f.admit("stage-x", "inbox/a.md");
    let Admission::Inserted(id) = &first else {
        panic!("{first:?}");
    };
    assert_eq!(
        f.admit("stage-x", "inbox/a.md"),
        Admission::Duplicate(id.clone())
    );
    let job = f.job(id);
    assert_eq!(job.service.as_deref(), Some(LOC));
    assert!(job.resource_keys.contains(&note_resource_key(note)));
    assert_eq!(job.lane, Lane::Background);
    let jp: JobParams = serde_json::from_value(job.params).unwrap();
    assert!(jp.is_stage() && jp.chain.is_empty());
    assert_eq!(jp.context.active_id, Some(note));
    let e = f.runner.admit_stage("plain", "inbox/a.md").unwrap_err();
    assert!(e.to_string().contains("no stage marker"), "{e}");
    let e = f.runner.admit_stage("stage-x", "inbox/b.md").unwrap_err();
    assert!(e.to_string().contains("no bound id"), "{e}");
}

/// A stage job admitted while its service's breaker is open is not claimed and makes no call;
/// an untagged stage job never waits on a breaker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_breaker_gates_an_admitted_job_with_no_provider_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let f = fx(counting(&calls, true), "localize");
    f.bind("inbox/a.md");
    f.open(LOC);
    let gated = id_of(&f.admit("stage-x", "inbox/a.md"));
    let tidy = id_of(&f.admit("stage-tidy", "inbox/a.md"));
    assert_eq!(f.job(&tidy).service, None);
    assert_eq!(f.claim_run().await.as_deref(), Some(tidy.as_str()));
    assert_eq!(
        f.claim_run().await,
        None,
        "the tagged job waits on its breaker"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "only the untagged job ran");
    assert_eq!(f.job(&gated).status, DbStatus::Queued);
}

/// The tag equals the key a deferral of each service-bearing step kind stamps.
#[test]
fn tag_equals_the_deferral_key_of_each_service_bearing_step_kind() {
    let pipelines =
        "[image_describe]\nenabled = true\nprovider = \"local\"\nmodel = \"local-chat-model\"\n";
    let mut c = ConfigSet::new();
    c.reload(&files("", pipelines), &crate::steps::validate);
    let key = |src: &str| {
        let step = Step::from_table(toml::from_str(src).unwrap()).unwrap();
        crate::steps::service_key(&step, &c).unwrap()
    };
    // The agents' default models (agents.toml examples) name the provider the deferral uses
    // (`agent::provider_failure(&provider.id, ..)`).
    let refine = crate::steps::agent::default_service_key(&c, "transcript-refine").unwrap();
    assert_eq!(refine, "openrouter");
    let cases = [
        ("type = \"localize\"", Some(crate::steps::localize::SERVICE)),
        ("type = \"yt_metadata\"", Some(YT)),
        ("type = \"yt_channel_enrich\"", Some(YT)),
        ("type = \"image_describe\"\nscope = \"note\"", Some("local")),
        ("type = \"transcript_refine\"", Some("openrouter")),
        ("type = \"intake_enrich\"", Some("openrouter")),
        ("type = \"intake_enrich\"\nscope = \"backfill\"", None),
        (
            "type = \"agent\"\nagent = \"refine-transcript\"",
            Some("local"),
        ),
        (
            "type = \"search_upsert\"\nembed = \"local:local-embed-model\"",
            Some("local"),
        ),
        ("type = \"search_upsert\"", None),
        ("type = \"yt_clip_tidy\"", None),
        ("type = \"intake_finalize\"", None),
    ];
    for (src, want) in cases {
        assert_eq!(key(src).as_deref(), want, "{src}");
    }
    // The localize deferral is stamped with the fetch's service, the same constant.
    let e = cv_providers::web::FetchError::Transient {
        service: crate::steps::localize::SERVICE.into(),
        retry_after: None,
        kind: cv_providers::FailureKind::Timeout,
        detail: "t".into(),
    };
    let StepError::Deferred { service, .. } = StepError::from_fetch(&e) else {
        panic!("deferred");
    };
    assert_eq!(service.as_deref(), Some(LOC));
}

/// A stage action whose steps resolve to two keys is disarmed with a named error, at load
/// (`load_config_dir`, the reload path too).
#[test]
fn two_key_stage_action_is_disarmed_with_a_named_error() {
    let dir = tempfile::tempdir().unwrap();
    let src = "[[action]]\nid = \"two\"\nstage = \"localize\"\n[[action.steps]]\ntype = \"localize\"\n[[action.steps]]\ntype = \"yt_metadata\"\n";
    std::fs::write(dir.path().join("actions.toml"), src).unwrap();
    let mut c = ConfigSet::new();
    load_config_dir(dir.path(), &mut c);
    let why = c.disarm_reason("two").expect("disarmed");
    assert!(why.contains("more than one service key"), "{why}");
    assert!(why.contains(LOC) && why.contains(YT), "{why}");
    // A user action with the same two steps but no stage marker stays armed.
    std::fs::write(
        dir.path().join("actions.toml"),
        src.replace("stage = \"localize\"\n", ""),
    )
    .unwrap();
    load_config_dir(dir.path(), &mut c);
    assert!(c.armed("two").is_some());
}

/// A reload that moves a queued stage job from A to B gates and credits B, with A's breaker open
/// and then with B's open.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reload_moves_a_queued_job_from_a_to_b_and_b_gates_and_credits() {
    // A open: the re-tagged job runs (A does not gate it) and its success credits B only.
    let f = fx(counting(&Arc::new(AtomicUsize::new(0)), true), "localize");
    f.bind("inbox/a.md");
    let id = id_of(&f.admit("stage-x", "inbox/a.md"));
    f.open(LOC);
    f.state
        .lock()
        .unwrap()
        .report_service_failure(YT, ServiceFailureKind::Timeout, "blip", None, T0)
        .unwrap();
    f.runner.set_config(config(&actions("yt_metadata")));
    assert_eq!(f.job(&id).service.as_deref(), Some(YT));
    assert_eq!(f.claim_run().await.as_deref(), Some(id.as_str()));
    assert_eq!(f.job(&id).status, DbStatus::Done);
    assert_eq!(f.health(YT).failure_score, 0, "B credited");
    assert_eq!(f.health(LOC).state, BreakerState::Open, "A untouched");

    // B open: the re-tagged job waits on B, then is B's half-open probe and closes it.
    let f = fx(counting(&Arc::new(AtomicUsize::new(0)), true), "localize");
    f.bind("inbox/a.md");
    let id = id_of(&f.admit("stage-x", "inbox/a.md"));
    f.open(YT);
    f.runner.set_config(config(&actions("yt_metadata")));
    assert_eq!(f.claim_run().await, None, "B gates the job");
    f.clock.set(T0 + LATER);
    assert_eq!(f.claim_run().await.as_deref(), Some(id.as_str()));
    assert_eq!(f.job(&id).status, DbStatus::Done);
    assert_eq!(f.health(YT).state, BreakerState::Closed, "B credited");
}

/// Cycle-2 E1: a claimed half-open probe of A whose tag a reload moves to B is requeued without
/// running; A's probe slot is freed without A closing, and B's breaker is untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retag_of_a_claimed_probe_retires_the_old_probe_only() {
    let calls = Arc::new(AtomicUsize::new(0));
    let f = fx(counting(&calls, true), "localize");
    f.bind("inbox/a.md");
    let id = id_of(&f.admit("stage-x", "inbox/a.md"));
    f.open(LOC);
    f.clock.set(T0 + LATER);
    let job = f
        .state
        .lock()
        .unwrap()
        .claim(f.clock.now_ms())
        .unwrap()
        .expect("A's probe");
    let a = f.health(LOC);
    assert_eq!(a.state, BreakerState::HalfOpen);
    assert!(a.probe_in_flight());
    let b_before = f.health(YT);
    // The reload lands while the job is claimed (a queued-only re-tag cannot see it).
    *write(&f.runner.inner().config) = Arc::new(config(&actions("yt_metadata")));
    run_job(Arc::clone(f.runner.inner()), job).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0, "requeued without running");
    let j = f.job(&id);
    assert_eq!(j.status, DbStatus::Queued);
    assert_eq!(j.service.as_deref(), Some(YT));
    assert_eq!(j.attempts, 0, "the re-tag spends no attempt");
    let a = f.health(LOC);
    assert_eq!(a.state, BreakerState::HalfOpen, "A not closed");
    assert!(!a.probe_in_flight(), "A's probe slot freed");
    assert_eq!(f.health(YT), b_before, "B untouched by the re-tag");
}

/// A tagged job that settles Done without an attempt, or by replaying checkpoints, does not close
/// a half-open breaker; it releases the probe.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn done_without_an_attempt_or_by_replay_does_not_close_half_open() {
    // No attempt: the step returned Ok without calling the provider.
    let f = fx(counting(&Arc::new(AtomicUsize::new(0)), false), "localize");
    f.bind("inbox/a.md");
    let id = id_of(&f.admit("stage-x", "inbox/a.md"));
    f.open(LOC);
    f.clock.set(T0 + LATER);
    assert_eq!(f.claim_run().await.as_deref(), Some(id.as_str()));
    assert_eq!(f.job(&id).status, DbStatus::Done);
    let h = f.health(LOC);
    assert_eq!(h.state, BreakerState::HalfOpen);
    assert!(!h.probe_in_flight(), "probe released");

    // Replay: the only step's checkpoint says `attempted` (an earlier claim); this claim runs
    // nothing, so it is no attempt.
    let calls = Arc::new(AtomicUsize::new(0));
    let f = fx(counting(&calls, true), "localize");
    f.bind("inbox/a.md");
    let id = id_of(&f.admit("stage-x", "inbox/a.md"));
    let cp = serde_json::to_string(&StepOutcome {
        attempted: true,
        ..StepOutcome::default()
    })
    .unwrap();
    f.state
        .lock()
        .unwrap()
        .put_checkpoint(&id, 0, &cp, T0)
        .unwrap();
    f.open(LOC);
    f.clock.set(T0 + LATER);
    assert_eq!(f.claim_run().await.as_deref(), Some(id.as_str()));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.job(&id).status, DbStatus::Done);
    assert_eq!(
        f.health(LOC).state,
        BreakerState::HalfOpen,
        "replay is not an attempt"
    );
}

/// Cycle-1 E1: a checkpoint written before the attempt flag existed still decodes (`false`).
#[test]
fn old_checkpoint_shape_decodes_without_the_attempt_flag() {
    let o: StepOutcome =
        serde_json::from_str(r#"{"open_path":"a.md","vars":{"n":1},"stop":true}"#).unwrap();
    assert!(!o.attempted && o.stop);
    assert_eq!(o.open_path.as_deref(), Some("a.md"));
    let o: StepOutcome = serde_json::from_str("{}").unwrap();
    assert_eq!(o, StepOutcome::default());
    // And a flag-free outcome serializes as before (no new key).
    assert_eq!(
        serde_json::to_string(&StepOutcome::default()).unwrap(),
        "{}"
    );
}

/// Two seam jobs for one note never run together, across a rename: both carry the note-id key.
#[test]
fn two_seam_jobs_for_one_note_do_not_run_together_across_a_rename() {
    let f = fx(counting(&Arc::new(AtomicUsize::new(0)), true), "localize");
    let note = f.bind("inbox/a.md");
    let first = id_of(&f.admit("stage-tidy", "inbox/a.md"));
    f.state
        .lock()
        .unwrap()
        .note_id_move("inbox/a.md", "stack/a.md", T0)
        .unwrap();
    let second = id_of(&f.admit("stage-x", "stack/a.md"));
    let key = note_resource_key(note);
    assert!(f.job(&first).resource_keys.contains(&key));
    assert!(f.job(&second).resource_keys.contains(&key));
    assert!(
        !f.job(&second)
            .resource_keys
            .contains(&"inbox/a.md".to_owned())
    );
    let mut db = f.state.lock().unwrap();
    let claimed = db.claim(T0).unwrap().expect("first claim");
    assert_eq!(claimed.id, first);
    assert!(
        db.claim(T0).unwrap().is_none(),
        "the note-id key serializes them"
    );
}

/// A seam job past the transient limit stays deferred (an ordinary job fails as before) and is
/// the probe when the breaker half-opens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seam_job_past_the_transient_limit_stays_deferred_and_probes() {
    let exec: Arc<dyn StepExecutor> = Arc::new(FnExec(Box::new(|_, _| {
        Err(StepError::Deferred {
            retry_after_ms: 0,
            service: Some(LOC.into()),
            cause: DeferCause::Transient(ServiceFailureKind::Timeout),
        })
    })));
    let f = fx(exec, "localize");
    f.bind("inbox/a.md");
    let id = id_of(&f.admit("stage-x", "inbox/a.md"));
    let mut now = T0;
    for _ in 0..(TRANSIENT_MAX_ATTEMPTS + 2) {
        // Each claim waits out the job's backoff and the breaker's window.
        now += 2 * TRANSIENT_CAP_MS;
        f.clock.set(now);
        assert_eq!(f.claim_run().await.as_deref(), Some(id.as_str()));
        assert_eq!(
            f.job(&id).status,
            DbStatus::Queued,
            "never a terminal outage"
        );
    }
    assert!(f.job(&id).attempts > TRANSIENT_MAX_ATTEMPTS);
    assert_eq!(f.health(LOC).state, BreakerState::Open);
    f.clock.set(now + 2 * TRANSIENT_CAP_MS);
    let job = f
        .state
        .lock()
        .unwrap()
        .claim(f.clock.now_ms())
        .unwrap()
        .expect("the probe");
    assert_eq!(job.id, id);
    assert!(f.health(LOC).probe_in_flight());
    // An ordinary job at the limit still fails with a service outage.
    let mut ordinary = job.clone();
    ordinary.attempts = TRANSIENT_MAX_ATTEMPTS;
    let deferred = || JobResult::Deferred {
        retry_after_ms: 0,
        service_unhealthy: Some(LOC.into()),
    };
    let cause = Some(DeferCause::Transient(ServiceFailureKind::Timeout));
    let (r, _) = Inner::apply_service_policy(&ordinary, false, deferred(), cause);
    assert!(matches!(r, JobResult::Failed { .. }));
    let (r, _) = Inner::apply_service_policy(&ordinary, true, deferred(), cause);
    assert!(matches!(r, JobResult::Deferred { .. }));
}

/// The guard says "not runnable": a fresh seam job settles Done without running; a seam job with
/// a checkpoint resumes its remaining step; a user-started run of the stage action is never
/// guarded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guard_skips_only_a_fresh_seam_job() {
    struct Never;
    impl StageGuard for Never {
        fn runnable(&self, _: &str, _: &ActionContext, _: &ConfigSet) -> bool {
            false
        }
    }
    let ran = Arc::new(Mutex::new(Vec::new()));
    let r2 = Arc::clone(&ran);
    let exec: Arc<dyn StepExecutor> = Arc::new(FnExec(Box::new(move |_, ctx| {
        r2.lock().unwrap().push(ctx.step_index);
        Ok(StepOutcome::default())
    })));
    let two_steps = "[[action]]\nid = \"stage-two\"\nstage = \"yt_metadata\"\n[[action.steps]]\ntype = \"yt_metadata\"\n[[action.steps]]\ntype = \"yt_channel_enrich\"\n";
    let f = fx(exec, "localize");
    f.runner.set_config(config(two_steps));
    f.runner.set_stage_guard(Arc::new(Never));
    f.bind("inbox/a.md");
    f.bind("inbox/b.md");
    let fresh = id_of(&f.admit("stage-two", "inbox/a.md"));
    assert_eq!(f.claim_run().await.as_deref(), Some(fresh.as_str()));
    assert_eq!(f.job(&fresh).status, DbStatus::Done);
    assert!(ran.lock().unwrap().is_empty(), "guarded: nothing ran");

    // Metadata linked (step 0 checkpointed), then the channel step deferred: it resumes.
    let resumed = id_of(&f.admit("stage-two", "inbox/b.md"));
    f.state
        .lock()
        .unwrap()
        .put_checkpoint(&resumed, 0, "{}", T0)
        .unwrap();
    assert_eq!(f.claim_run().await.as_deref(), Some(resumed.as_str()));
    assert_eq!(f.job(&resumed).status, DbStatus::Done);
    assert_eq!(*ran.lock().unwrap(), vec![1], "the channel step ran");

    // A user-started run of the stage action.
    ran.lock().unwrap().clear();
    let cfg = f.runner.config();
    let ctx = ActionContext {
        active_path: Some("inbox/a.md".into()),
        ..ActionContext::default()
    };
    let user = f
        .runner
        .enqueue(
            cfg.armed("stage-two").unwrap(),
            RunRequest::command(Map::new(), ctx),
        )
        .unwrap();
    assert_eq!(f.claim_run().await.as_deref(), Some(user.as_str()));
    assert_eq!(
        *ran.lock().unwrap(),
        vec![0, 1],
        "a user run is never guarded"
    );
}

/// A child `run_action` at depth four and a cycle are refused as today (the seam leaves children
/// unchanged).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_depth_four_and_cycle_are_refused_as_today() {
    let exec: Arc<dyn StepExecutor> = Arc::new(FnExec(Box::new(|step, ctx| {
        if step.kind == StepKind::RunAction {
            crate::steps::execute(step, ctx)
        } else {
            Ok(StepOutcome::default())
        }
    })));
    let chain = (1..=5)
        .map(|n| {
            let next = if n == 5 { "a1".to_owned() } else { format!("a{}", n + 1) };
            format!(
                "[[action]]\nid = \"a{n}\"\nresources = {{ note = false }}\n[[action.steps]]\ntype = \"run_action\"\naction = \"{next}\"\n"
            )
        })
        .collect::<String>();
    let f = fx(exec, "localize");
    f.runner.set_config(config(&chain));
    let cfg = f.runner.config();
    let top = f
        .runner
        .enqueue(
            cfg.armed("a1").unwrap(),
            RunRequest::command(Map::new(), ActionContext::default()),
        )
        .unwrap();
    let mut ran = Vec::new();
    while let Some(id) = f.claim_run().await {
        ran.push(id);
    }
    assert_eq!(f.job(&top).status, DbStatus::Done);
    let jobs = f
        .state
        .lock()
        .unwrap()
        .list(&cv_state::ListQuery::default())
        .unwrap();
    let failed: Vec<_> = jobs
        .iter()
        .filter(|j| j.status == DbStatus::Failed)
        .collect();
    assert_eq!(failed.len(), 1, "{jobs:?}");
    assert_eq!(failed[0].job_type, "action:a4");
    let err = failed[0].error.as_deref().unwrap();
    assert!(err.contains("depth limit 4"), "{err}");

    // A cycle: a child naming an ancestor is refused.
    let looped = "[[action]]\nid = \"x\"\nresources = { note = false }\n[[action.steps]]\ntype = \"run_action\"\naction = \"x\"\n";
    f.runner.set_config(config(looped));
    let cfg = f.runner.config();
    let x = f
        .runner
        .enqueue(
            cfg.armed("x").unwrap(),
            RunRequest::command(Map::new(), ActionContext::default()),
        )
        .unwrap();
    assert_eq!(f.claim_run().await.as_deref(), Some(x.as_str()));
    let j = f.job(&x);
    assert_eq!(j.status, DbStatus::Failed);
    assert!(j.error.unwrap().contains("cycle"));
    let _ = &f.dir;
}

/// Plan intake-stage-jobs WP-2: the stage model's guard. A fresh seam job whose stage is done
/// for its note (no remote image left) settles without running; one whose stage is pending runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_guard_skips_a_done_stage_and_runs_a_pending_one() {
    let calls = Arc::new(AtomicUsize::new(0));
    let f = fx(counting(&calls, false), LOC);
    let w = |p: &str, s: &str| {
        let full = f.dir.path().join(p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, s).unwrap();
    };
    w(
        "Blog/done.md",
        "---\npost-id: https://b.example/d\n---\nbody\n",
    );
    w(
        "Blog/todo.md",
        "---\npost-id: https://b.example/t\n---\n![](https://img.example/p.png)\n",
    );
    drop(cv_index::Index::create(f.dir.path(), &f.dir.path().join("index.db")).unwrap());
    f.runner.set_stage_guard(Arc::new(ModelGuard::new(
        Arc::clone(&f.state),
        f.dir.path().to_path_buf(),
        f.dir.path().join("index.db"),
        f.dir.path().join("search.db"),
    )));
    f.bind("Blog/done.md");
    f.bind("Blog/todo.md");
    let done = id_of(&f.admit("stage-x", "Blog/done.md"));
    assert_eq!(f.claim_run().await.as_deref(), Some(done.as_str()));
    assert_eq!(f.job(&done).status, DbStatus::Done);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "a done stage never runs");
    let todo = id_of(&f.admit("stage-x", "Blog/todo.md"));
    assert_eq!(f.claim_run().await.as_deref(), Some(todo.as_str()));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "a pending stage runs");
}

/// Plan intake-stage-jobs WP-3, settle-site recording: a seam job that fails in a step that is
/// not a body of its stage records `failed` against the stage where the job settles; a failure
/// of a stage-body step is the step's own recording and is not recorded again at settle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_seam_failure_outside_the_stage_body_is_recorded_at_settle() {
    let failing: Arc<dyn StepExecutor> =
        Arc::new(FnExec(Box::new(|_, _| Err(StepError::failed("boom")))));
    for (step, recorded) in [("set_props", true), (LOC, false)] {
        let f = fx(Arc::clone(&failing), step);
        let full = f.dir.path().join("Blog/todo.md");
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(
            full,
            "---\npost-id: https://b.example/t\n---\n![](https://img.example/p.png)\n",
        )
        .unwrap();
        drop(cv_index::Index::create(f.dir.path(), &f.dir.path().join("index.db")).unwrap());
        let id = i64::try_from(f.bind("Blog/todo.md").0).unwrap();
        let job = id_of(&f.admit("stage-x", "Blog/todo.md"));
        assert_eq!(f.claim_run().await.as_deref(), Some(job.as_str()));
        assert_eq!(f.job(&job).status, DbStatus::Failed);
        let rows = f.state.lock().unwrap().stage_outcomes(id).unwrap();
        if recorded {
            assert_eq!(rows.len(), 1, "{step}");
            assert_eq!(
                (
                    rows[0].stage.as_str(),
                    rows[0].outcome.as_str(),
                    rows[0].attempts
                ),
                ("localize", "failed", 1)
            );
            assert!(rows[0].reason.contains("boom"), "{}", rows[0].reason);
        } else {
            assert!(rows.is_empty(), "{step}: the body step owns its recording");
        }
    }
}
