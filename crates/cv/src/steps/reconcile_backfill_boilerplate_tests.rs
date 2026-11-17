use super::*;

// ---- per-sender boilerplate (plan round-10-feedback-todos WP-2) ------------------------------

const FOOT1: &str = "You are receiving this because you subscribed to Newsbit.";
const FOOT2: &str = "Example Media Inc, 1 Example Street, Nowhere.";

fn bp_eml(mid: &str, from: &str, subject: &str, day: u32, words: &str) -> Vec<u8> {
    format!(
        "From: Sender <{from}>\nSubject: {subject}\nDate: Tue, {day} Oct 2026 10:00:00 +0000\nMessage-ID: <{mid}>\nMIME-Version: 1.0\nContent-Type: text/html; charset=utf-8\n\n<p>{words} one is here.</p><p>{words} two is here.</p><p>{words} three is here.</p><p>{FOOT1}</p><p>{FOOT2}</p>\n"
    )
    .replace('\n', "\r\n")
    .into_bytes()
}

fn bp_note(words: &str) -> String {
    format!(
        "---\ntitle: \"T\"\ndescription: \"stale\"\nvia: email\n---\n\n{words} one is here.\n\n{words} two is here.\n\n{words} three is here.\n\n{FOOT1}\n\n{FOOT2}\n"
    )
}

fn note_of(w: &World, key: &str, id: &str) -> String {
    let (r, _) = w.row(KIND, key, id).unwrap();
    w.env.read(r.note_path.as_deref().unwrap()).unwrap()
}

#[test]
fn capture_strips_a_footer_shared_with_an_earlier_message_and_leaves_unknown_senders() {
    let mut w = world(&[]);
    w.sources(HEATMAP);
    w.put(
        "new/m1",
        &bp_eml(
            "m1@plain.example",
            "hello@plain.example",
            "First",
            14,
            "Alpha",
        ),
    );
    w.put(
        "new/m2",
        &bp_eml(
            "m2@plain.example",
            "hello@plain.example",
            "Second",
            15,
            "Beta",
        ),
    );
    w.put(
        "new/u1",
        &bp_eml(
            "u1@else.example",
            "stranger@else.example",
            "Third",
            16,
            "Gamma",
        ),
    );
    w.track();
    let notes = [
        note_of(&w, "Newsbit", "m1@plain.example"),
        note_of(&w, "Newsbit", "m2@plain.example"),
    ];
    assert_eq!(
        notes.iter().filter(|t| t.contains(FOOT1)).count(),
        1,
        "{notes:?}"
    );
    assert!(notes.iter().all(|t| t.contains("three is here.")));
    let unknown = note_of(&w, UNKNOWN, "u1@else.example");
    assert!(unknown.contains(FOOT1) && unknown.contains(FOOT2));
    // Reconcile strips the earlier note without learning either message again.
    let o = w.reconcile(false);
    assert_eq!((n(&o, "bp_learned"), n(&o, "bp_stripped")), (0, 1));
    assert!(!note_of(&w, "Newsbit", "m1@plain.example").contains(FOOT2));
    assert!(note_of(&w, UNKNOWN, "u1@else.example").contains(FOOT1));
}

#[test]
fn reconcile_seeds_from_the_maildir_and_note_bodies_then_strips_once() {
    let a = bp_note("Alpha");
    let b = bp_note("Beta");
    let mut w = world(&[("Letters/a.md", a.clone()), ("Letters/b.md", b.clone())]);
    w.sources(HEATMAP);
    w.put(
        "processed/m1",
        &bp_eml(
            "m1@plain.example",
            "hello@plain.example",
            "First",
            14,
            "Alpha",
        ),
    );
    add_row(
        &w,
        KIND,
        "Newsbit",
        "m1@plain.example",
        TrackerOutcome::Note,
        Some("Letters/a.md"),
    );
    // m2's `.eml` is gone: its note body is learned instead.
    add_row(
        &w,
        KIND,
        "Newsbit",
        "m2@plain.example",
        TrackerOutcome::Note,
        Some("Letters/b.md"),
    );
    let bp = |o: &StepOutcome| {
        ["bp_learned", "bp_stripped", "bp_guarded", "bp_conflict"].map(|k| n(o, k))
    };
    let dry = w.reconcile(true);
    assert_eq!(bp(&dry), [2, 2, 0, 0]);
    assert_eq!(w.env.read("Letters/a.md").unwrap(), a);
    assert_eq!(w.env.read("Letters/b.md").unwrap(), b);
    {
        let db = w.env.state.lock().unwrap();
        assert!(db.mail_boilerplate_lines("Newsbit").unwrap().is_empty());
        assert!(
            !db.mail_boilerplate_seen("Newsbit", "m1@plain.example")
                .unwrap()
        );
    }
    let o = w.reconcile(false);
    assert_eq!(bp(&o), bp(&dry));
    for p in ["Letters/a.md", "Letters/b.md"] {
        let t = w.env.read(p).unwrap();
        assert!(
            !t.contains(FOOT1) && !t.contains(FOOT2) && t.contains("three is here."),
            "{t}"
        );
        assert!(!t.contains("stale"), "description recomputed: {t}");
    }
    let after = w.env.read("Letters/a.md").unwrap();
    let again = w.reconcile(false);
    assert_eq!(bp(&again), [0, 0, 0, 0]);
    assert_eq!(w.env.read("Letters/a.md").unwrap(), after);
}

#[test]
fn reconcile_drops_semicolon_free_css_and_recomputes_the_css_description() {
    let css = "@media screen and (max-width:600px){.x .y a{font-size: 9px}.w{margin: 0 auto}.v .u{border: 1px solid red}}@media (min-width:700px){.x{padding: 9px 9 9 9}.z .q{color: #333}}@media print{.k{display: none}.j{width: 100%}.i{height: 50px}}";
    let note = format!(
        "---\ntitle: \"T\"\ndescription: \"@media screen and (max-width:600px){{.x .y a{{font-size: 9px}}\"\nvia: email\n---\n\nFirst paragraph.\n\n{css}\n\nSecond paragraph.\n"
    );
    let w = world(&[("Letters/c.md", note)]);
    let o = w.reconcile(false);
    assert_eq!(
        (
            n(&o, "mail_recleaned_r2"),
            n(&o, "mail_description_repaired")
        ),
        (1, 1)
    );
    let t = w.env.read("Letters/c.md").unwrap();
    assert!(
        !t.contains("@media") && t.contains("Second paragraph."),
        "{t}"
    );
    let again = w.reconcile(false);
    assert_eq!(n(&again, "mail_recleaned_r2"), 0);
}

// ---- per-publication learning, RSS and label tiers (plan intake-removals-sources-rescan WP-5) --

const FEED: &str = "https://feed.example/feed";
const BLOG: &str = "[[blog]]\nname = \"Feed Pub\"\nlink = \"https://feed.example/feed\"\n";
const RSS_FOOT1: &str = "Thanks for reading Feed Pub, subscribe for more.";
const RSS_FOOT2: &str = "Share this post with a friend today.";

fn label(publication: &str, line: &str, verdict: &str) -> cv_state::RemovalLabel {
    cv_state::RemovalLabel {
        note_id: 1,
        publication: publication.into(),
        kind: "footer".into(),
        verdict: verdict.into(),
        origin: "fold".into(),
        line: line.into(),
        line_key: cv_ingest::boilerplate::line_key(line),
        at_ms: 1,
    }
}

fn add_labels(w: &World, rows: &[cv_state::RemovalLabel]) {
    w.env
        .state
        .lock()
        .unwrap()
        .insert_removal_labels(rows)
        .unwrap();
}

fn rss_note(i: usize, top: bool) -> String {
    let top = if top {
        "%% cv-enrich %%\n> [!summary]\n> Thanks for reading Feed Pub, subscribe for more.\n\n%% /cv-enrich %%\n\n"
    } else {
        ""
    };
    format!(
        "---\ntitle: \"P{i}\"\npublication: \"Feed Pub\"\npost-id: \"p{i}\"\n---\n{top}Post {i} opening paragraph words.\n\nPost {i} second paragraph words.\n\nPost {i} third paragraph words.\n\nPost {i} fourth paragraph words.\n\n{RSS_FOOT1}\n\n{RSS_FOOT2}\n"
    )
}

/// `n` RSS notes under `Blogs/` with their `blog` rows (note 0 carries an enriched top block).
fn rss_world(n: usize) -> World {
    let files: Vec<(String, String)> = (0..n)
        .map(|i| (format!("Blogs/p{i}.md"), rss_note(i, i == 0)))
        .collect();
    let refs: Vec<(&str, String)> = files.iter().map(|(p, t)| (p.as_str(), t.clone())).collect();
    let mut w = world(&refs);
    w.sources(BLOG);
    for (i, (p, _)) in files.iter().enumerate() {
        add_row(
            &w,
            "blog",
            FEED,
            &format!("p{i}"),
            TrackerOutcome::Line,
            Some(p),
        );
    }
    w
}

#[test]
fn reconcile_learns_rss_once_per_post_folds_recurring_footers_and_is_idempotent() {
    let w = rss_world(4);
    // A Keep label on one footer line for the publication: it stays in every body.
    add_labels(&w, &[label("Feed Pub", RSS_FOOT2, "keep")]);
    let o = w.reconcile(false);
    assert_eq!(n(&o, "bp_rss_learned"), 4);
    assert_eq!(n(&o, "bp_rss_folded"), 4);
    assert_eq!(n(&o, "bp_lines_folded"), 4);
    assert_eq!(n(&o, "bp_lines_vetoed"), 4);
    assert_eq!(n(&o, "bp_publications"), 1);
    assert_eq!(n(&o, "bp_label_unknown"), 0);
    for i in 0..4 {
        let t = w.env.read(&format!("Blogs/p{i}.md")).unwrap();
        let g = cv_ingest::enrich::fold_groups(cv_note::FrontmatterDoc::new(t.as_str()).body());
        assert_eq!(g.len(), 1, "{t}");
        assert_eq!(g[0].kind, Some(cv_ingest::enrich::SpanKind::Footer));
        assert_eq!(g[0].lines, [RSS_FOOT1]);
        assert!(t.contains(&format!("\n{RSS_FOOT2}\n")), "{t}");
        assert!(t.starts_with(&format!(
            "---\ntitle: \"P{i}\"\npublication: \"Feed Pub\"\n"
        )));
    }
    // The generated top block (which quotes the footer) is never learned or moved.
    let t0 = w.env.read("Blogs/p0.md").unwrap();
    assert!(t0.contains("%% cv-enrich %%\n> [!summary]\n> Thanks for reading"));
    // Re-run: nothing learned again, nothing written.
    let o = w.reconcile(false);
    assert_eq!((n(&o, "bp_rss_learned"), n(&o, "bp_rss_folded")), (0, 0));
    assert_eq!(w.env.read("Blogs/p0.md").unwrap(), t0);
}

#[test]
fn rss_capture_learns_once_and_folds_from_the_third_post_unless_kept() {
    let w = rss_world(0);
    let ctx = w.env.ctx();
    let cap = |i: usize| {
        crate::steps::reconcile_boilerplate::learn_and_fold_rss(
            &ctx,
            "Feed Pub",
            "Feed Pub",
            &format!("p{i}"),
            &rss_note(i, false),
        )
        .unwrap()
    };
    assert_eq!(cap(0), rss_note(0, false));
    assert_eq!(cap(1), rss_note(1, false));
    // The same post again is not learned twice: still 2 posts, nothing folded.
    assert_eq!(cap(1), rss_note(1, false));
    let third = cap(2);
    assert!(
        third.contains("> [!removed]-") && third.contains("kind=footer"),
        "{third}"
    );
    // Keep label: the line survives a capture that would fold it.
    add_labels(
        &w,
        &[
            label("Feed Pub", RSS_FOOT1, "keep"),
            label("Feed Pub", RSS_FOOT2, "keep"),
        ],
    );
    assert_eq!(cap(3), rss_note(3, false));
}

#[test]
fn mail_capture_keeps_a_keep_labelled_footer_line_and_strips_the_rest() {
    let mut w = world(&[]);
    w.sources(HEATMAP);
    add_labels(&w, &[label("Newsbit", FOOT1, "keep")]);
    w.put(
        "new/m1",
        &bp_eml(
            "m1@plain.example",
            "hello@plain.example",
            "First",
            14,
            "Alpha",
        ),
    );
    w.put(
        "new/m2",
        &bp_eml(
            "m2@plain.example",
            "hello@plain.example",
            "Second",
            15,
            "Beta",
        ),
    );
    w.track();
    let notes = [
        note_of(&w, "Newsbit", "m1@plain.example"),
        note_of(&w, "Newsbit", "m2@plain.example"),
    ];
    assert!(notes.iter().all(|t| t.contains(FOOT1)), "{notes:?}");
    assert_eq!(notes.iter().filter(|t| t.contains(FOOT2)).count(), 1);
    let o = w.reconcile(false);
    assert_eq!(n(&o, "bp_stripped"), 1);
    assert!(n(&o, "bp_lines_vetoed") >= 1);
    assert!(note_of(&w, "Newsbit", "m1@plain.example").contains(FOOT1));
    assert!(!note_of(&w, "Newsbit", "m1@plain.example").contains(FOOT2));
}
