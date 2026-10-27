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
