//! `removals_review` tests (plan `round-23-daemon-memory-mail-enrich-fanfic` WP-6): synthetic
//! enriched notes over the shipped example config.

use serde_json::{Value, json};

use super::removals_review::{
    Kind, Span, allocate, classify, precision, render_review, sample, tally,
};
use super::testkit::{StepEnv, active, example_env, today};

const REVIEW: &str = "type = \"removals_review\"\nmode = \"review\"";
const SCORE: &str = "type = \"removals_review\"\nmode = \"score\"";

/// An enriched note whose fold holds `spans` (one group each).
fn enriched(props: &str, spans: &[&str]) -> String {
    let mut s = format!(
        "---\nenriched: 2026-11-01\n{props}---\nBody text.\n\n%% cv-enrich %%\n> [!removed]-\n"
    );
    for (i, sp) in spans.iter().enumerate() {
        if i > 0 {
            s.push_str(">\n");
        }
        for l in sp.lines() {
            s.push_str(&format!("> {l}\n"));
        }
    }
    s.push_str("\n%% /cv-enrich %%\n");
    s
}

fn var(o: &super::StepOutcome, k: &str) -> Value {
    o.vars.get(k).cloned().unwrap_or(Value::Null)
}

fn review_path() -> String {
    format!("_crucible/reviews/removals-{}.md", today())
}

fn corpus() -> StepEnv {
    example_env(&[
        (
            "Blogs/b1.md",
            &enriched("post-id: 1\n", &["ad one", "ad two"]),
        ),
        ("Blogs/b2.md", &enriched("post-id: 2\n", &["ad three"])),
        (
            "Mail/m1.md",
            &enriched("message-id: <a@b>\n", &["footer a", "footer b", "footer c"]),
        ),
        (
            "Clips/y1.md",
            &enriched(
                "source: https://www.youtube.com/watch?v=abcdefghijk\n",
                &["sponsor"],
            ),
        ),
        (
            "Clips/c1.md",
            &enriched(
                "source: https://example.com/x\n",
                &["cookie banner", "share bar"],
            ),
        ),
        (
            "_hidden/h.md",
            &enriched("post-id: 9\n", &["never sampled"]),
        ),
        ("Blogs/plain.md", "---\npost-id: 3\n---\nnot enriched\n"),
    ])
}

fn ctx() -> cv_protocol::ActionContext {
    active("Blogs/b1.md")
}

#[test]
fn classify_the_four_types() {
    assert_eq!(classify(true, true, ""), Kind::Blog);
    assert_eq!(
        classify(false, true, "https://www.youtube.com/watch?v=abcdefghijk"),
        Kind::Mail
    );
    assert_eq!(
        classify(false, false, "https://www.youtube.com/watch?v=abcdefghijk"),
        Kind::Youtube
    );
    assert_eq!(classify(false, false, "https://example.com/a"), Kind::Clip);
    assert_eq!(classify(false, false, ""), Kind::Clip);
}

#[test]
fn allocation_is_proportional_with_a_floor() {
    // Everything fits: take it all.
    assert_eq!(allocate(&[2, 0, 3, 1], 10), [2, 0, 3, 1]);
    // Proportional, exactly `limit` in total, never above a population.
    let q = allocate(&[600, 300, 90, 10], 100);
    assert_eq!(q.iter().sum::<usize>(), 100);
    assert_eq!(q, [60, 30, 9, 1]);
    // One per non-empty type while the limit allows.
    let q = allocate(&[1000, 1000, 5, 0], 10);
    assert_eq!(q.iter().sum::<usize>(), 10);
    assert!(q[2] >= 1 && q[3] == 0, "{q:?}");
    // Fewer slots than types: the biggest populations get them.
    assert_eq!(allocate(&[5, 50, 20, 9], 2), [0, 1, 1, 0]);
}

#[test]
fn sampling_is_seeded_and_reproducible() {
    let spans: Vec<Span> = (0..50)
        .map(|i| Span {
            path: format!("N/{i:02}.md"),
            idx: 0,
            text: format!("t{i}"),
        })
        .collect();
    let (a, b) = (sample(&spans, 10, 7), sample(&spans, 10, 7));
    assert_eq!(a, b);
    assert_eq!(a.len(), 10);
    assert_ne!(a, sample(&spans, 10, 8));
    assert!(
        a.windows(2).all(|w| w[0].path < w[1].path),
        "path order kept"
    );
    assert_eq!(sample(&spans, 99, 1).len(), 50);
}

#[test]
fn review_writes_one_note_with_markers_and_counts() {
    let e = corpus();
    let o = e.run(REVIEW, json!({"limit": 100}), ctx()).unwrap();
    let path = review_path();
    assert_eq!(var(&o, "review_path"), json!(path));
    assert_eq!(var(&o, "open_path"), json!(path));
    assert_eq!(o.open_path.as_deref(), Some(path.as_str()));
    assert_eq!(var(&o, "population"), json!(9));
    assert_eq!(var(&o, "sampled"), json!(9));
    for (k, n) in [("blog", 3), ("mail", 3), ("youtube", 1), ("clip", 2)] {
        assert_eq!(var(&o, &format!("population_{k}")), json!(n), "{k}");
        assert_eq!(var(&o, &format!("sampled_{k}")), json!(n), "{k}");
    }
    let t = e.read(&path).unwrap();
    assert!(
        t.contains("Tick = this text should NOT have been removed."),
        "{t}"
    );
    assert!(
        t.contains("- [ ] [[Blogs/b1]] (blog) %%rm blog%%\n  > ad one\n"),
        "{t}"
    );
    assert!(t.contains("(youtube) %%rm youtube%%\n  > sponsor"), "{t}");
    assert!(
        !t.contains("never sampled") && !t.contains("not enriched"),
        "{t}"
    );
    assert_eq!(t.matches("- [ ] ").count(), 9);
    // Only the review note was written.
    assert!(e.read("Blogs/b1.md").unwrap().contains("ad one"));
}

#[test]
fn review_sample_is_stratified_limited_and_stable() {
    let e = corpus();
    let o = e
        .run(REVIEW, json!({"limit": 4, "seed": 5}), ctx())
        .unwrap();
    assert_eq!(var(&o, "sampled"), json!(4));
    for k in ["blog", "mail", "youtube", "clip"] {
        assert!(
            var(&o, &format!("sampled_{k}")).as_u64().unwrap() >= 1,
            "{k}"
        );
    }
    let first = e.read(&review_path()).unwrap();
    e.run(
        REVIEW,
        json!({"limit": 4, "seed": 5, "overwrite": true}),
        ctx(),
    )
    .unwrap();
    assert_eq!(e.read(&review_path()).unwrap(), first);
}

#[test]
fn existing_review_is_refused_without_overwrite() {
    let e = corpus();
    e.run(REVIEW, json!({}), ctx()).unwrap();
    let ticked = e
        .read(&review_path())
        .unwrap()
        .replacen("- [ ]", "- [x]", 1);
    // The user ticks one item.
    let path = review_path();
    let mut c = e.ctx();
    let (_, h) = c.read(&path).unwrap().unwrap();
    c.write(&path, ticked.clone().into_bytes(), Some(h))
        .unwrap();
    let err = e.run(REVIEW, json!({}), ctx()).unwrap_err().to_string();
    assert!(err.contains("already exists"), "{err}");
    assert_eq!(e.read(&path).unwrap(), ticked);
    e.run(REVIEW, json!({"overwrite": true}), ctx()).unwrap();
    assert!(!e.read(&path).unwrap().contains("- [x]"));
}

#[test]
fn long_spans_are_capped_with_a_marker() {
    let long = (1..=12)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let t = render_review(
        "2026-11-07",
        &[(
            Kind::Clip,
            Span {
                path: "A/n.md".into(),
                idx: 0,
                text: long,
            },
        )],
    );
    assert!(t.contains("  > line 8\n  > …\n"), "{t}");
    assert!(!t.contains("line 9"));
}

#[test]
fn score_counts_ticks_overall_and_per_type() {
    let note = "# R\n\nTick = x\n\n\
- [x] [[a]] (blog) %%rm blog%%\n  > q\n\
- [ ] [[b]] (blog) %%rm blog%%\n  > q\n\
- [ ] [[c]] (blog) %%rm blog%%\n  > q\n\
- [ ] [[d]] (blog) %%rm blog%%\n  > q\n\
- [X] [[e]] (mail) %%rm mail%%\n  > q\n\
- [x] [[f]] (mail) %%rm mail%%\n  > q\n\
- [ ] [[g]] (clip) %%rm clip%%\n  > - [x] not an item\n";
    let t = tally(note);
    assert_eq!(t["blog"], (4, 1));
    assert_eq!(t["mail"], (2, 2));
    assert_eq!(t["clip"], (1, 0));
    assert_eq!(precision(4, 1).as_deref(), Some("0.750"));
    assert_eq!(precision(0, 0), None);

    let e = example_env(&[("_crucible/reviews/removals-2026-11-07.md", note)]);
    let o = e.run(SCORE, json!({}), ctx()).unwrap();
    assert_eq!(var(&o, "items"), json!(7));
    assert_eq!(var(&o, "ticked"), json!(3));
    assert_eq!(var(&o, "precision"), json!("0.571"));
    assert_eq!(var(&o, "items_blog"), json!(4));
    assert_eq!(var(&o, "ticked_mail"), json!(2));
    assert_eq!(var(&o, "precision_blog"), json!("0.750"));
    assert_eq!(var(&o, "precision_mail"), json!("0.000"));
    assert_eq!(var(&o, "precision_clip"), json!("1.000"));
    assert!(o.output_paths.is_empty() && o.open_path.is_none());
}

#[test]
fn score_picks_the_newest_or_the_given_path_and_needs_items() {
    let item = |c: &str| format!("- [{c}] [[a]] (blog) %%rm blog%%\n");
    let e = example_env(&[
        ("_crucible/reviews/removals-2026-11-01.md", &item("x")),
        ("_crucible/reviews/removals-2026-11-07.md", &item(" ")),
        ("_crucible/reviews/other.md", "nothing\n"),
        ("Plain.md", "no items\n"),
    ]);
    let o = e.run(SCORE, json!({}), ctx()).unwrap();
    assert_eq!(var(&o, "ticked"), json!(0));
    let o = e
        .run(
            SCORE,
            json!({"path": "_crucible/reviews/removals-2026-11-01.md"}),
            ctx(),
        )
        .unwrap();
    assert_eq!(var(&o, "precision"), json!("0.000"));
    let err = e
        .run(SCORE, json!({"path": "Plain.md"}), ctx())
        .unwrap_err()
        .to_string();
    assert!(err.contains("no review items"), "{err}");
    let err = example_env(&[])
        .run(SCORE, json!({}), ctx())
        .unwrap_err()
        .to_string();
    assert!(err.contains("note found"), "{err}");
}

#[test]
fn bad_params_are_rejected() {
    for src in [
        "type = \"removals_review\"\nmode = \"all\"",
        "type = \"removals_review\"\nmode = \"score\"\nlimit = 5",
        "type = \"removals_review\"\npath = \"x.md\"",
        "type = \"removals_review\"\nlimit = 0",
        "type = \"removals_review\"\nx = 1",
    ] {
        let step = cv_actions::Step::from_table(src.parse::<toml::Table>().unwrap()).unwrap();
        assert!(
            super::validate(&step, &super::testkit::example_config()).is_err(),
            "{src}"
        );
    }
    let e = corpus();
    let err = e
        .run(REVIEW, json!({"limit": "0"}), ctx())
        .unwrap_err()
        .to_string();
    assert!(err.contains("positive integer"), "{err}");
}
