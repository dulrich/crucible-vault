use super::*;

fn st(states: &[(Stage, State)], stage: Stage) -> State {
    states
        .iter()
        .find(|(s, _)| *s == stage)
        .map(|(_, st)| st.clone())
        .expect("stage in required set")
}

fn failed(stage: Stage, attempts: u32) -> Outcome {
    Outcome {
        stage,
        kind: OutcomeKind::Failed,
        reason: "boom".into(),
        attempts,
        fingerprint: String::new(),
    }
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
    f.tracker = TrackerLink::Linked;
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
        let done = stage_states(&all_done(kind), &[]);
        assert_eq!(done.len(), kind.required().len());
        for (s, state) in &done {
            assert_eq!(*state, State::Done, "{kind:?} {s:?}");
        }
        let mut bare = NoteFacts::new(kind);
        bare.awaiting_filing = true;
        bare.remote_images = 2;
        bare.images = vec![("a".into(), ImageRecord::Missing)];
        bare.youtube_preamble = true;
        bare.transcript_of = true;
        bare.transcript_words = Some(MIN_REFINE_WORDS);
        bare.tracker = TrackerLink::Unlinked;
        for (s, state) in stage_states(&bare, &[]) {
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
            |f| f.remote_images = 1,
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
        assert_eq!(st(&stage_states(&f, &[]), *stage), *want, "{name}");
    }
}

#[test]
fn recorded_outcomes_failed_stuck_and_skipped() {
    let mut f = all_done(IntakeKind::Youtube);
    f.remote_images = 1;
    f.images = vec![("a".into(), ImageRecord::Missing)];
    f.enriched = false;
    f.index_current = false;
    // Localize failed → describe (its dependant, without evidence) is stuck, and so is index.
    let s = stage_states(&f, &[failed(Stage::Localize, 3)]);
    assert_eq!(st(&s, Stage::Localize), State::Failed(3));
    assert_eq!(st(&s, Stage::ImageDescribe), State::Stuck);
    assert_eq!(st(&s, Stage::Index), State::Stuck);
    // Enrich (no stuck prerequisite: refine is done) stays pending.
    assert_eq!(st(&s, Stage::Enrich), State::Pending);
    // Evidence beats a recorded failure.
    let s = stage_states(&all_done(IntakeKind::Blog), &[failed(Stage::Enrich, 2)]);
    assert_eq!(st(&s, Stage::Enrich), State::Done);
    // A recorded skip on the current facts holds.
    let s = stage_states(&f, &[skipped(Stage::Enrich, &f, "too long")]);
    assert_eq!(st(&s, Stage::Enrich), State::Skipped("too long".into()));
    // Rows for stages outside the kind's set and unknown kinds are ignored.
    let blog = all_done(IntakeKind::Blog);
    let s = stage_states(&blog, &[failed(Stage::YtMetadata, 1)]);
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
    let s = stage_states(&f, &[refine.clone(), describe.clone()]);
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
    let s = stage_states(&f, &[refine, describe]);
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
        { format!("v1:{:016x}", fnv1a("index|blog|")) }
    );
    assert!(fingerprint(Stage::Index, &a).starts_with("v1:"));
    assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
}
