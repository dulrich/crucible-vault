use super::*;

fn st(states: &[(Stage, State)], stage: Stage) -> State {
    states
        .iter()
        .find(|(s, _)| *s == stage)
        .map(|(_, st)| st.clone())
        .expect("stage in required set")
}

fn failed(stage: Stage, f: &NoteFacts, attempts: u32) -> Outcome {
    row(stage, f, OutcomeKind::Failed, attempts)
}

fn row(stage: Stage, f: &NoteFacts, kind: OutcomeKind, attempts: u32) -> Outcome {
    Outcome {
        stage,
        kind,
        reason: "boom".into(),
        attempts,
        fingerprint: fingerprint(stage, f),
    }
}

const CAP: u32 = 3;

fn states(f: &NoteFacts, o: &[Outcome]) -> Vec<(Stage, State)> {
    stage_states(f, o, CAP)
}

fn skipped(stage: Stage, f: &NoteFacts, reason: &str) -> Outcome {
    Outcome {
        stage,
        kind: OutcomeKind::Skipped,
        reason: reason.into(),
        attempts: 1,
        fingerprint: fingerprint(stage, f),
    }
}

/// A note of `kind` whose every stage has its done evidence.
fn all_done(kind: IntakeKind) -> NoteFacts {
    let mut f = NoteFacts::new(kind);
    f.images = vec![("a".into(), ImageRecord::Described)];
    f.yt_metadata_linked = true;
    f.refined = true;
    f.enriched = true;
    f.tracker = TrackerLink::Settled;
    f.index_current = true;
    f
}

#[test]
fn required_sets_follow_stage_order_and_prerequisites_come_first() {
    for kind in IntakeKind::ALL {
        let req = kind.required();
        let pos = |s: Stage| Stage::ALL.iter().position(|x| *x == s).unwrap();
        assert!(req.windows(2).all(|w| pos(w[0]) < pos(w[1])), "{kind:?}");
        for s in req {
            assert!(s.prerequisites().iter().all(|p| pos(*p) < pos(*s)));
        }
    }
    for s in Stage::ALL {
        assert_eq!(Stage::parse(s.as_str()), Some(s));
    }
    assert_eq!(Stage::parse("nope"), None);
}

#[test]
fn every_kind_reads_done_from_evidence_and_pending_without_it() {
    for kind in IntakeKind::ALL {
        let done = states(&all_done(kind), &[]);
        assert_eq!(done.len(), kind.required().len());
        for (s, state) in &done {
            assert_eq!(*state, State::Done, "{kind:?} {s:?}");
        }
        let mut bare = NoteFacts::new(kind);
        bare.awaiting_filing = true;
        bare.remote_refs = vec!["https://x.example/a.png".into()];
        bare.images = vec![("a".into(), ImageRecord::Missing)];
        bare.youtube_preamble = true;
        bare.transcript_of = true;
        bare.transcript_words = Some(MIN_REFINE_WORDS);
        bare.tracker = TrackerLink::Pending;
        for (s, state) in states(&bare, &[]) {
            assert_eq!(state, State::Pending, "{kind:?} {s:?}");
        }
    }
}

/// One row per state, judged on a youtube note (the widest required set) unless noted.
#[test]
fn table_of_states() {
    type Case = (&'static str, IntakeKind, fn(&mut NoteFacts), Stage, State);
    let cases: &[Case] = &[
        (
            "no images",
            IntakeKind::Blog,
            |f| f.images.clear(),
            Stage::ImageDescribe,
            State::Skipped("no images".into()),
        ),
        (
            "failed record counts as attempted",
            IntakeKind::Mail,
            |f| f.images = vec![("a".into(), ImageRecord::Failed)],
            Stage::ImageDescribe,
            State::Done,
        ),
        (
            "images left unattempted",
            IntakeKind::Clip,
            |f| {
                f.images = vec![
                    ("a".into(), ImageRecord::Described),
                    ("b".into(), ImageRecord::Missing),
                ]
            },
            Stage::ImageDescribe,
            State::Unfinished,
        ),
        (
            "enrich miss",
            IntakeKind::Blog,
            |f| {
                f.enriched = false;
                f.enrich_miss = 1;
            },
            Stage::Enrich,
            State::Unfinished,
        ),
        (
            "not youtube",
            IntakeKind::Clip,
            |f| {
                f.refined = false;
                f.transcript_of = false;
            },
            Stage::TranscriptRefine,
            State::Skipped("not youtube".into()),
        ),
        (
            "no transcript",
            IntakeKind::Youtube,
            |f| f.refined = false,
            Stage::TranscriptRefine,
            State::Skipped("no transcript".into()),
        ),
        (
            "short transcript",
            IntakeKind::Youtube,
            |f| {
                f.refined = false;
                f.transcript_words = Some(MIN_REFINE_WORDS - 1);
            },
            Stage::TranscriptRefine,
            State::Skipped("short transcript".into()),
        ),
        (
            "no tracker row",
            IntakeKind::Mail,
            |f| f.tracker = TrackerLink::NoRow,
            Stage::Finalize,
            State::Skipped("no tracker row".into()),
        ),
        (
            "index stale",
            IntakeKind::Blog,
            |f| f.index_current = false,
            Stage::Index,
            State::Pending,
        ),
        (
            "clip not filed",
            IntakeKind::Clip,
            |f| f.awaiting_filing = true,
            Stage::AutoFile,
            State::Pending,
        ),
        (
            "remote left",
            IntakeKind::Youtube,
            |f| f.remote_refs = vec!["https://x.example/a.png".into()],
            Stage::Localize,
            State::Pending,
        ),
        (
            "preamble left",
            IntakeKind::Youtube,
            |f| f.youtube_preamble = true,
            Stage::YtClipTidy,
            State::Pending,
        ),
        (
            "metadata missing",
            IntakeKind::Youtube,
            |f| f.yt_metadata_linked = false,
            Stage::YtMetadata,
            State::Pending,
        ),
    ];
    for (name, kind, edit, stage, want) in cases {
        let mut f = all_done(*kind);
        edit(&mut f);
        assert_eq!(st(&states(&f, &[]), *stage), *want, "{name}");
    }
}

#[test]
fn recorded_outcomes_failed_stuck_and_skipped() {
    let mut f = all_done(IntakeKind::Youtube);
    f.remote_refs = vec!["https://x.example/a.png".into()];
    f.images = vec![("a".into(), ImageRecord::Missing)];
    f.enriched = false;
    f.index_current = false;
    // Localize failed → describe (its hard dependant, without evidence) is stuck; index (soft)
    // is not.
    let s = states(&f, &[failed(Stage::Localize, &f, 2)]);
    assert_eq!(st(&s, Stage::Localize), State::Failed(2));
    assert_eq!(st(&s, Stage::ImageDescribe), State::Stuck);
    assert_eq!(st(&s, Stage::Index), State::Pending);
    // Enrich (no stuck prerequisite: refine is done) stays pending.
    assert_eq!(st(&s, Stage::Enrich), State::Pending);
    // Evidence beats a recorded failure.
    let s = states(
        &all_done(IntakeKind::Blog),
        &[failed(Stage::Enrich, &all_done(IntakeKind::Blog), 2)],
    );
    assert_eq!(st(&s, Stage::Enrich), State::Done);
    // A recorded skip on the current facts holds.
    let s = states(&f, &[skipped(Stage::Enrich, &f, "too long")]);
    assert_eq!(st(&s, Stage::Enrich), State::Skipped("too long".into()));
    // Rows for stages outside the kind's set and unknown kinds are ignored.
    let blog = all_done(IntakeKind::Blog);
    let s = states(&blog, &[failed(Stage::YtMetadata, &blog, 1)]);
    assert!(s.iter().all(|(st, _)| *st != Stage::YtMetadata));
    assert_eq!(OutcomeKind::parse("weird"), None);
}

#[test]
fn a_skip_whose_facts_changed_reads_pending() {
    let mut f = all_done(IntakeKind::Youtube);
    f.refined = false;
    f.transcript_words = Some(MIN_REFINE_WORDS + 10);
    f.images = vec![("a".into(), ImageRecord::Missing)];
    let refine = skipped(Stage::TranscriptRefine, &f, "model refused");
    let describe = skipped(Stage::ImageDescribe, &f, "provider off");
    let s = states(&f, &[refine.clone(), describe.clone()]);
    assert_eq!(
        st(&s, Stage::TranscriptRefine),
        State::Skipped("model refused".into())
    );
    assert_eq!(
        st(&s, Stage::ImageDescribe),
        State::Skipped("provider off".into())
    );
    // Same note, the transcript grew and a second image arrived: both skips are stale.
    f.transcript_words = Some(MIN_REFINE_WORDS + 500);
    f.images.push(("b".into(), ImageRecord::Missing));
    let s = states(&f, &[refine, describe]);
    assert_eq!(st(&s, Stage::TranscriptRefine), State::Pending);
    assert_eq!(st(&s, Stage::ImageDescribe), State::Pending);
}

#[test]
fn fingerprint_is_stable_and_order_free() {
    let mut a = NoteFacts::new(IntakeKind::Mail);
    a.images = vec![
        ("b".into(), ImageRecord::Missing),
        ("a".into(), ImageRecord::Described),
    ];
    let mut b = a.clone();
    b.images.reverse();
    b.images[0].1 = ImageRecord::Missing;
    assert_eq!(
        fingerprint(Stage::ImageDescribe, &a),
        fingerprint(Stage::ImageDescribe, &b)
    );
    // Pinned: the format must not drift between builds (stored rows compare against it).
    assert_eq!(
        fingerprint(Stage::Index, &NoteFacts::new(IntakeKind::Blog)),
        { format!("v1:{:016x}", fnv1a("index|blog|hash=")) }
    );
    assert!(fingerprint(Stage::Index, &a).starts_with("v1:"));
    assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
}

/// The stage-to-job table (plan `intake-stage-jobs`): each stage's action, hard prerequisites,
/// and soft ones (Index only), with exhausted against stuck.
#[test]
fn stage_to_job_table() {
    let table: [(Stage, &str, &[Stage], &[Stage]); 9] = [
        (Stage::AutoFile, "stage-file", &[], &[]),
        (Stage::Localize, "stage-localize", &[Stage::AutoFile], &[]),
        (
            Stage::ImageDescribe,
            "stage-describe",
            &[Stage::Localize],
            &[],
        ),
        (Stage::YtMetadata, "stage-yt-metadata", &[], &[]),
        (Stage::YtClipTidy, "stage-yt-tidy", &[Stage::AutoFile], &[]),
        (
            Stage::TranscriptRefine,
            "stage-refine",
            &[Stage::YtClipTidy],
            &[],
        ),
        (
            Stage::Enrich,
            "stage-enrich",
            &[Stage::TranscriptRefine],
            &[],
        ),
        (Stage::Finalize, "stage-finalize", &[], &[]),
        (
            Stage::Index,
            "stage-index",
            &[],
            &[Stage::ImageDescribe, Stage::Enrich],
        ),
    ];
    for (stage, action, hard, soft) in table {
        assert_eq!(stage.action_id(), action);
        assert_eq!(Stage::for_action(action), Some(stage));
        assert_eq!(stage.prerequisites(), hard, "{stage:?}");
        assert_eq!(stage.soft_prerequisites(), soft, "{stage:?}");
    }
    assert_eq!(Stage::for_action("clip-enrich"), None);

    let mut f = all_done(IntakeKind::Youtube);
    f.remote_refs = vec!["https://x.example/a.png".into()];
    f.images = vec![("a".into(), ImageRecord::Missing)];
    f.enriched = false;
    f.index_current = false;
    let p = Progress::default();
    // Hard: describe waits for a pending localize, and is stuck behind an exhausted one.
    let s = states(&f, &[]);
    assert!(admissible(&s, Stage::Localize, &p));
    assert!(!admissible(&s, Stage::ImageDescribe, &p));
    let s = states(&f, &[failed(Stage::Localize, &f, CAP)]);
    assert_eq!(st(&s, Stage::Localize), State::Exhausted(CAP));
    assert_eq!(st(&s, Stage::ImageDescribe), State::Stuck);
    assert!(!admissible(&s, Stage::Localize, &p));
    // An unfinished row at the cap is exhausted too; below it, unfinished and admissible.
    let s = states(&f, &[row(Stage::Enrich, &f, OutcomeKind::Unfinished, 1)]);
    assert_eq!(st(&s, Stage::Enrich), State::Unfinished);
    assert!(admissible(&s, Stage::Enrich, &p));
    let s = states(&f, &[row(Stage::Enrich, &f, OutcomeKind::Unfinished, CAP)]);
    assert_eq!(st(&s, Stage::Enrich), State::Exhausted(CAP));
    // Soft: a failed enrich never makes index stuck.
    let s = states(&f, &[failed(Stage::Enrich, &f, 1)]);
    assert_eq!(st(&s, Stage::Index), State::Pending);
    // Done and skipped stages are never admissible; a stage the kind lacks neither.
    assert!(!admissible(&s, Stage::YtMetadata, &p));
    let blog = states(&all_done(IntakeKind::Blog), &[]);
    assert!(!admissible(&blog, Stage::AutoFile, &p));
}

/// A failed row whose fingerprint no longer matches the note reads pending.
#[test]
fn a_failed_row_with_a_stale_fingerprint_reads_pending() {
    let mut f = all_done(IntakeKind::Blog);
    f.enriched = false;
    f.transcript_words = Some(10);
    let r = failed(Stage::Enrich, &f, 2);
    assert_eq!(
        st(&states(&f, std::slice::from_ref(&r)), Stage::Enrich),
        State::Failed(2)
    );
    f.transcript_words = Some(900);
    assert_eq!(st(&states(&f, &[r]), Stage::Enrich), State::Pending);
}

/// An exhausted Index with a new content hash, and a failed Localize whose refs changed to a
/// different set of the same size, both read pending.
#[test]
fn index_hash_and_localize_ref_set_are_fingerprinted() {
    let mut f = all_done(IntakeKind::Mail);
    f.index_current = false;
    f.prepared_hash = "h1".into();
    f.remote_refs = vec!["https://x.example/a.png".into()];
    let idx = failed(Stage::Index, &f, CAP);
    let loc = failed(Stage::Localize, &f, 1);
    let s = states(&f, &[idx.clone(), loc.clone()]);
    assert_eq!(st(&s, Stage::Index), State::Exhausted(CAP));
    assert_eq!(st(&s, Stage::Localize), State::Failed(1));
    f.prepared_hash = "h2".into();
    f.remote_refs = vec!["https://x.example/b.png".into()];
    let s = states(&f, &[idx, loc]);
    assert_eq!(st(&s, Stage::Index), State::Pending);
    assert_eq!(st(&s, Stage::Localize), State::Pending);
}

/// A recorded failed filing outranks the "nothing left to file" evidence; YtMetadata waits for
/// the channel step; the finalize decision drives Finalize.
#[test]
fn auto_file_failure_channel_step_and_finalize_decision() {
    let mut f = all_done(IntakeKind::Youtube);
    let s = states(&f, &[failed(Stage::AutoFile, &f, 1)]);
    assert_eq!(st(&s, Stage::AutoFile), State::Failed(1));
    assert_eq!(
        st(&s, Stage::YtClipTidy),
        State::Done,
        "evidence of its own"
    );
    f.yt_channel_pending = true;
    assert_eq!(st(&states(&f, &[]), Stage::YtMetadata), State::Pending);
    f.tracker = TrackerLink::Pending;
    assert_eq!(st(&states(&f, &[]), Stage::Finalize), State::Pending);
}

/// Index is not admissible while a healthy upstream stage can still progress; it is once that
/// stage is held by an open breaker, and once it is done.
#[test]
fn index_is_admitted_last() {
    let mut f = all_done(IntakeKind::Blog);
    f.enriched = false;
    f.index_current = false;
    let s = states(&f, &[]);
    assert!(admissible(&s, Stage::Enrich, &Progress::default()));
    assert!(!admissible(&s, Stage::Index, &Progress::default()));
    let held = Progress {
        held: vec![Stage::Enrich],
        ..Progress::default()
    };
    assert!(admissible(&s, Stage::Index, &held));
    // A queued, unheld job of a stage that is not itself admissible still holds Index back.
    let mut g = all_done(IntakeKind::Blog);
    g.index_current = false;
    let s = states(&g, &[]);
    assert!(admissible(&s, Stage::Index, &Progress::default()));
    let active = Progress {
        active: vec![Stage::Finalize],
        ..Progress::default()
    };
    assert!(!admissible(&s, Stage::Index, &active));
    // Exhausted upstream: Index runs on what the note holds.
    let s = states(&f, &[failed(Stage::Enrich, &f, CAP)]);
    assert!(admissible(&s, Stage::Index, &Progress::default()));
}
