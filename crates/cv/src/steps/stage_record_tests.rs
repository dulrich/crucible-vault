//! Stage outcome recorder tests (plan intake-stage-jobs WP-3): a synthetic temp vault + index, a
//! temp `state.db`, a YouTube intake note; no network.

use cv_actions::{Step, StepKind};
use cv_ingest::stage::{Stage, State};
use cv_state::{FailureReason, StageOutcomeRow};
use serde_json::json;

use super::stage_record::{Env, Happened, LEFT_IN_PLACE, happened, record};
use super::testkit::{StepEnv, active, config_of};
use super::{DeferCause, StepError, StepOutcome};
use crate::note_facts::{ATTEMPT_CAP, FactEnv, note_view};

const P: &str = "Clips/v.md";

const ACTIONS: &str = r#"
[[action]]
id = "stage-localize"
stage = "localize"
[[action.steps]]
type = "localize"

[[action]]
id = "loc"
[[action.steps]]
type = "localize"

[[action]]
id = "stage-yt-metadata"
stage = "yt_metadata"
[[action.steps]]
type = "yt_metadata"
[[action.steps]]
type = "yt_channel_enrich"
"#;

/// A YouTube intake note: `remote` = a remote image ref left, `done` = enriched + refined.
fn note(remote: Option<&str>, done: bool) -> String {
    let words = "word ".repeat(400);
    let stamps = if done {
        "enriched: 2026-11-27\ntags:\n  - refined\n"
    } else {
        ""
    };
    let img = remote.map(|u| format!("![]({u})\n\n")).unwrap_or_default();
    format!(
        "---\nsource: https://www.youtube.com/watch?v=dQw4w9WgXcQ\n{stamps}---\n{img}## Transcript\n\n{words}\n"
    )
}

struct Fx {
    env: StepEnv,
    id: i64,
}

fn fx(text: &str) -> Fx {
    let config = config_of(&[("actions.toml", ACTIONS)]);
    let action = config.armed("stage-localize").unwrap().clone();
    let env = StepEnv::new(
        cv_vault::test_support::LiveFixture::new([(P, text)]),
        config,
        action,
    );
    let id = i64::try_from(env.state.lock().unwrap().note_id_bind(P).unwrap().0).unwrap();
    Fx { env, id }
}

impl Fx {
    fn rows(&self) -> Vec<StageOutcomeRow> {
        self.env
            .state
            .lock()
            .unwrap()
            .stage_outcomes(self.id)
            .unwrap()
    }

    fn row(&self, stage: Stage) -> Option<(String, String, u32)> {
        self.rows()
            .into_iter()
            .find(|r| r.stage == stage.as_str())
            .map(|r| (r.outcome, r.reason, r.attempts))
    }

    fn rec(&self, stage: Stage, what: Happened) {
        let env = Env {
            state: &self.env.state,
            vault: &self.env.fx.vault,
            index_db: &self.env.fx.index_db,
            search_db: None,
            config: &self.env.config,
        };
        record(&env, &active(P), stage, &what).unwrap();
    }

    fn rewrite(&self, text: &str) {
        let mut ctx = self.env.ctx();
        let (_, h) = ctx.read(P).unwrap().unwrap();
        ctx.write(P, text.as_bytes().to_vec(), Some(h)).unwrap();
    }

    /// Runs action `id`'s first step on the note through the production dispatch.
    fn run(&self, id: &str) -> Result<StepOutcome, StepError> {
        let action = self.env.config.armed(id).unwrap().clone();
        let context = active(P);
        let mut ctx = self.env.ctx();
        ctx.action = &action;
        ctx.context = &context;
        super::execute(&action.steps[0], &mut ctx)
    }

    fn state(&self, stage: Stage) -> State {
        let reader = cv_index::Reader::open_current(&self.env.fx.index_db).unwrap();
        let db = self.env.state.lock().unwrap();
        let fe = FactEnv {
            vault: &self.env.fx.vault,
            config: &self.env.config,
            reader: &reader,
            state: Some(&*db),
            indexed: None,
        };
        let v = note_view(&fe, P).unwrap().unwrap();
        v.states(ATTEMPT_CAP)
            .into_iter()
            .find(|(s, _)| *s == stage)
            .unwrap()
            .1
    }
}

fn ok(skip: Option<&str>) -> Happened {
    Happened::Ok {
        skip: skip.map(str::to_owned),
    }
}

fn row(outcome: &str, reason: &str, attempts: u32) -> Option<(String, String, u32)> {
    Some((outcome.to_owned(), reason.to_owned(), attempts))
}

/// Per stage: a skip records its reason, an Ok with work left records unfinished (+1), a
/// non-transient error records failed (+1, continuing the count on the same fingerprint), the
/// model honours the rows, and a later success clears the row (a failure on a done stage too).
#[test]
fn per_stage_skip_unfinished_failed_then_success_clears() {
    let f = fx(&note(Some("https://example.com/a.png"), false));
    for stage in [Stage::Localize, Stage::TranscriptRefine, Stage::Enrich] {
        f.rec(stage, ok(Some("disabled")));
        assert_eq!(f.row(stage), row("skipped", "disabled", 0), "{stage:?}");
        assert_eq!(f.state(stage), State::Skipped("disabled".into()));
        f.rec(stage, ok(None));
        assert_eq!(f.row(stage), row("unfinished", "", 1), "{stage:?}");
        assert_eq!(f.state(stage), State::Unfinished);
        f.rec(stage, Happened::Failed("boom".into()));
        assert_eq!(f.row(stage), row("failed", "boom", 2), "{stage:?}");
        assert_eq!(f.state(stage), State::Failed(2));
        f.rec(stage, Happened::Failed("boom".into()));
        assert_eq!(f.state(stage), State::Exhausted(3), "{stage:?}");
    }
    f.rewrite(&note(None, true));
    for stage in [Stage::Localize, Stage::TranscriptRefine, Stage::Enrich] {
        f.rec(stage, ok(None));
        assert_eq!(f.row(stage), None, "{stage:?}: a done stage has no row");
        assert_eq!(f.state(stage), State::Done);
        f.rec(stage, Happened::Failed("late".into()));
        assert_eq!(f.row(stage), None, "{stage:?}: a done stage stays row-free");
    }
}

/// Attempts continue only on an equal fingerprint: a changed remote ref set restarts at 1.
#[test]
fn a_changed_fingerprint_restarts_the_count() {
    let f = fx(&note(Some("https://example.com/a.png"), false));
    f.rec(Stage::Localize, ok(None));
    f.rec(Stage::Localize, ok(None));
    assert_eq!(f.row(Stage::Localize), row("unfinished", "", 2));
    f.rewrite(&note(Some("https://example.com/b.png"), false));
    assert_eq!(
        f.state(Stage::Localize),
        State::Pending,
        "a stale row is ignored"
    );
    f.rec(Stage::Localize, ok(None));
    assert_eq!(f.row(Stage::Localize), row("unfinished", "", 1));
}

/// Through the production dispatch: a stage-marked run that fails non-transiently records
/// failed; the same step under an unmarked action records nothing; a later stage success clears.
#[test]
fn stage_runs_record_and_other_runs_record_nothing() {
    // No async runtime: the download fails as a plain step failure.
    let f = fx(&note(Some("http://127.0.0.1:9/a.png"), false));
    assert!(matches!(f.run("loc"), Err(StepError::Failed { .. })));
    assert!(f.rows().is_empty(), "a non-stage run records nothing");
    assert!(matches!(
        f.run("stage-localize"),
        Err(StepError::Failed { .. })
    ));
    let (outcome, reason, attempts) = f.row(Stage::Localize).unwrap();
    assert_eq!((outcome.as_str(), attempts), ("failed", 1));
    assert!(reason.contains("no async runtime"), "{reason}");
    assert!(matches!(
        f.run("stage-localize"),
        Err(StepError::Failed { .. })
    ));
    assert_eq!(f.row(Stage::Localize).unwrap().2, 2);
    f.rewrite(&note(None, false));
    f.run("stage-localize").unwrap();
    assert_eq!(f.row(Stage::Localize), None, "success clears the row");
}

/// A restart between a stage's work and its row write (no frontmatter stamp: Localize reads the
/// body): the stage reads done by evidence when the work landed, pending when it did not —
/// never failed.
#[test]
fn restart_without_a_row_reads_by_evidence() {
    let f = fx(&note(Some("https://example.com/a.png"), false));
    assert_eq!(f.state(Stage::Localize), State::Pending);
    // The work landed (refs localized) and the row write never happened.
    f.rewrite(&note(None, false));
    assert!(f.rows().is_empty());
    assert_eq!(f.state(Stage::Localize), State::Done);
}

fn step(kind: &str) -> Step {
    Step::from_table(format!("type = \"{kind}\"").parse::<toml::Table>().unwrap()).unwrap()
}

/// Outages, conflicts and vanished notes record nothing; a multi-step stage records its Ok after
/// its last body step only; `auto_file` records "left in place" unless it handed the filing off.
#[test]
fn what_records_and_what_does_not() {
    let f = fx(&note(None, false));
    let loc = f.env.config.armed("stage-localize").unwrap().clone();
    let yt = f.env.config.armed("stage-yt-metadata").unwrap().clone();
    let s = &loc.steps[0];
    let none = |r: Result<StepOutcome, StepError>| happened(&loc, 0, s, Stage::Localize, &r);
    assert_eq!(
        none(Err(StepError::Deferred {
            retry_after_ms: 1,
            service: Some("localize".into()),
            cause: DeferCause::Transient(cv_state::ServiceFailureKind::Refused),
        })),
        None
    );
    assert_eq!(none(Err(StepError::conflict())), None);
    assert_eq!(none(Err(StepError::NoteGone { path: P.into() })), None);
    assert_eq!(
        none(Err(StepError::Failed {
            message: "down".into(),
            reason: Some(FailureReason::ServiceOutage),
        })),
        None
    );
    assert_eq!(none(Ok(StepOutcome::default())), Some(ok(None)));
    // yt_metadata then yt_channel_enrich: the first Ok waits for the channel step.
    let r = Ok(StepOutcome::default());
    assert_eq!(happened(&yt, 0, &yt.steps[0], Stage::YtMetadata, &r), None);
    assert_eq!(
        happened(&yt, 1, &yt.steps[1], Stage::YtMetadata, &r),
        Some(ok(None))
    );
    assert!(matches!(
        happened(
            &yt,
            0,
            &yt.steps[0],
            Stage::YtMetadata,
            &Err(StepError::failed("x"))
        ),
        Some(Happened::Failed(_))
    ));
    // auto_file.
    let af = step("auto_file");
    assert_eq!(af.kind, StepKind::AutoFile);
    let mut file_action = loc.clone();
    file_action.stage = Some("auto_file".into());
    file_action.steps = vec![af.clone()];
    let mut handed = StepOutcome::default();
    handed.vars.insert("child_job_id".into(), json!("job-1"));
    assert_eq!(
        happened(&file_action, 0, &af, Stage::AutoFile, &Ok(handed)),
        None
    );
    assert_eq!(
        happened(
            &file_action,
            0,
            &af,
            Stage::AutoFile,
            &Ok(StepOutcome::default())
        ),
        Some(ok(Some(LEFT_IN_PLACE)))
    );
    // A step's `skipped` var is its precondition no-op reason.
    let mut skipped = StepOutcome::default();
    skipped.vars.insert("skipped".into(), json!("disabled"));
    assert_eq!(none(Ok(skipped)), Some(ok(Some("disabled"))));
}
