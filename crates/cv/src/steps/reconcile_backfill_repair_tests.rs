use super::*;

// ---- round-2 repair (plan intake-round-2-clean-dedupe-reading WP-6) ---------------------------

const ZED: &str = "https://zed.substack.com/p/slug-one";
const ZED_BLOG: &str =
    "[[blog]]\nname = \"Zed\"\nshort = \"ZZ\"\nlink = \"https://zed.substack.com/feed\"\n";

fn zed_eml() -> Vec<u8> {
    eml(
        "m1@zed.example",
        "zed@substack.com",
        &format!("<a href=\"{ZED}?token=abc\">Slug One</a><p>body</p>"),
    )
}

/// A mail note as `track_mail` writes it (canonical `source`).
fn mail_note(complete: bool, body: &str) -> String {
    format!(
        "---\ntitle: \"Slug One\"\nsource: {ZED}\nmessage-id: \"m1@zed.example\"\nauthor:\n  - \"[[Pub]]\"\ncomplete: {complete}\nvia: email\ntags:\n  - blog\n---\n\n{body}\n"
    )
}

fn rss_note(complete: bool, body: &str) -> String {
    format!(
        "---\ntitle: \"Slug One\"\nsource: {ZED}\nauthor:\n  - \"[[Pub]]\"\ncomplete: {complete}\nvia: rss\n---\n\n{body}\n"
    )
}

const LONG: &str = "The quick brown fox jumps over the lazy dog while seven wizards quietly box jars of old pickled plums for the winter market stalls.";

fn zed_world(rss: String, mail: String) -> World {
    let w = world(&[("Letters/rss.md", rss), ("mailnotes/m.md", mail)]);
    w.put("processed/m1", &zed_eml());
    add_row(
        &w,
        "mail",
        "Zed",
        "m1@zed.example",
        TrackerOutcome::Note,
        Some("mailnotes/m.md"),
    );
    w
}

fn repair_vars(o: &StepOutcome) -> Vec<(String, Value)> {
    let names = serde_json::to_value(super::reconcile_repair::Counts::default()).unwrap();
    names
        .as_object()
        .unwrap()
        .keys()
        .map(|k| (k.clone(), o.vars[k].clone()))
        .collect()
}

fn trashed(w: &World) -> usize {
    fn count(p: &std::path::Path) -> usize {
        std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| {
                let p = e.path();
                if p.is_dir() { count(&p) } else { 1 }
            })
            .sum()
    }
    count(&w.env.vault().join(".trash"))
}

#[test]
fn dry_run_repair_writes_nothing_and_the_real_run_merges_the_pair() {
    let w = zed_world(rss_note(false, "A teaser."), mail_note(true, LONG));
    let (mail0, rss0) = (
        w.env.read("mailnotes/m.md").unwrap(),
        w.env.read("Letters/rss.md").unwrap(),
    );
    let dry = w.reconcile(true);
    assert_eq!((n(&dry, "dup_merged"), n(&dry, "dup_marked")), (1, 0));
    assert_eq!(w.env.read("mailnotes/m.md").unwrap(), mail0);
    assert_eq!(w.env.read("Letters/rss.md").unwrap(), rss0);
    assert_eq!(trashed(&w), 0);
    let row = w.row("mail", "Zed", "m1@zed.example").unwrap();
    assert_eq!(
        (row.0.note_path.as_deref(), row.1),
        (Some("mailnotes/m.md"), None)
    );

    let real = w.reconcile(false);
    assert_eq!(repair_vars(&dry), repair_vars(&real));
    assert!(
        !w.env.vault().join("mailnotes/m.md").exists(),
        "loser trashed"
    );
    assert_eq!(trashed(&w), 1);
    let survivor = w.env.read("Letters/rss.md").unwrap();
    assert!(
        survivor.contains(LONG),
        "the fuller mail body replaced the stub"
    );
    let row = w.row("mail", "Zed", "m1@zed.example").unwrap();
    assert_eq!(
        (row.0.note_path.as_deref(), row.1),
        (Some("Letters/rss.md"), Some('x')),
        "re-pointed to the survivor and finalized"
    );

    let again = w.reconcile(false);
    for k in [
        "dup_merged",
        "dup_marked",
        "dup_other",
        "publication_backfilled",
    ] {
        assert_eq!(n(&again, k), 0, "{k}");
    }
    assert_eq!(trashed(&w), 1);
}

#[test]
fn a_contained_kept_mail_note_is_trashed() {
    let w = zed_world(
        rss_note(true, &format!("{LONG} And more text after it.")),
        mail_note(true, LONG),
    );
    let o = w.reconcile(false);
    assert_eq!((n(&o, "dup_merged"), n(&o, "dup_marked")), (1, 0));
    assert!(!w.env.vault().join("mailnotes/m.md").exists());
    assert_eq!(
        w.row("mail", "Zed", "m1@zed.example")
            .unwrap()
            .0
            .note_path
            .as_deref(),
        Some("Letters/rss.md")
    );
}

#[test]
fn a_kept_mail_note_that_is_not_contained_is_marked_and_nothing_is_trashed() {
    let other = "Entirely different words appear here describing harbors lighthouses and migrating geese over cold northern water at dawn.";
    let w = zed_world(rss_note(true, other), mail_note(true, LONG));
    let rss0 = w.env.read("Letters/rss.md").unwrap();
    let o = w.reconcile(false);
    assert_eq!((n(&o, "dup_merged"), n(&o, "dup_marked")), (0, 1));
    assert_eq!(trashed(&w), 0);
    let mail = w.env.read("mailnotes/m.md").unwrap();
    assert!(
        mail.contains("duplicate-of: \"[[Letters/rss]]\"")
            || mail.contains("duplicate-of: [[Letters/rss]]"),
        "{mail}"
    );
    assert!(
        mail.contains(&format!("source: {ZED}\n")),
        "source untouched: {mail}"
    );
    assert_eq!(w.env.read("Letters/rss.md").unwrap(), rss0);
    let again = w.reconcile(false);
    assert_eq!((n(&again, "dup_marked"), n(&again, "dup_merged")), (0, 0));
}

#[test]
fn an_identity_match_with_a_clip_is_counted_and_left_alone() {
    let clip =
        format!("---\ntitle: \"C\"\nsource: {ZED}\nvia: clip\ncomplete: false\n---\n\nA clip.\n");
    let w = zed_world(clip, mail_note(true, LONG));
    let o = w.reconcile(false);
    assert_eq!(
        (n(&o, "dup_other"), n(&o, "dup_merged"), n(&o, "dup_marked")),
        (1, 0, 0)
    );
    assert_eq!(trashed(&w), 0);
    assert!(
        w.env
            .read("mailnotes/m.md")
            .unwrap()
            .contains(&format!("source: {ZED}\n"))
    );
}

#[test]
fn publication_is_backfilled_once_and_never_overwritten() {
    let mut w = world(&[
        ("Letters/a.md", "---\ntitle: \"A\"\nauthor:\n  - \"[[P]]\"\nvia: rss\n---\n\nBody a.\n".to_owned()),
        ("Letters/b.md", "---\ntitle: \"B\"\nauthor:\n  - \"[[P]]\"\npublication: Mine\nvia: rss\n---\n\nBody b.\n".to_owned()),
        ("Letters/c.md", "---\ntitle: \"C\"\nvia: rss\n---\n\nBody c.\n".to_owned()),
    ]);
    w.sources(ZED_BLOG);
    for (id, p) in [("a", "Letters/a.md"), ("b", "Letters/b.md")] {
        add_row(
            &w,
            "blog",
            "https://zed.substack.com/feed",
            id,
            TrackerOutcome::Note,
            Some(p),
        );
    }
    add_row(
        &w,
        "blog",
        "https://other.example/feed",
        "c",
        TrackerOutcome::Note,
        Some("Letters/c.md"),
    );
    let b0 = w.env.read("Letters/b.md").unwrap();
    let dry = w.reconcile(true);
    assert_eq!(
        (
            n(&dry, "publication_backfilled"),
            n(&dry, "publication_no_row")
        ),
        (1, 1)
    );
    assert!(!w.env.read("Letters/a.md").unwrap().contains("publication"));
    let o = w.reconcile(false);
    assert_eq!(repair_vars(&dry), repair_vars(&o));
    let a = w.env.read("Letters/a.md").unwrap();
    assert!(
        a.contains("author:\n  - \"[[P]]\"\npublication: ZZ\n"),
        "{a}"
    );
    assert_eq!(w.env.read("Letters/b.md").unwrap(), b0, "never overwritten");
    let again = w.reconcile(false);
    assert_eq!(n(&again, "publication_backfilled"), 0);
    assert_eq!(w.env.read("Letters/a.md").unwrap(), a);
}

/// Round 11 E1: a note matching a `[[serial]]` row gets its publication with no tracker row;
/// one with a publication is untouched.
#[test]
fn serial_matched_note_without_a_tracker_row_gets_the_publication() {
    let mut w = world(&[
        ("Stories/a.md", "---\ntitle: \"A\"\nsource: https://saga.example/s/1\nauthor:\n  - \"[[Ann|A]]\"\n---\n\nBody a.\n".to_owned()),
        ("Stories/b.md", "---\ntitle: \"B\"\nsource: https://saga.example/s/2\nauthor:\n  - \"[[Ann]]\"\npublication: Mine\n---\n\nBody b.\n".to_owned()),
        ("Stories/c.md", "---\ntitle: \"C\"\nsource: https://elsewhere.example/s/3\nauthor:\n  - Bob\n---\n\nBody c.\n".to_owned()),
    ]);
    w.sources("[[serial]]\nname = \"The Saga\"\nshort = \"Saga\"\nauthor = \"Ann\"\nprefix = [\"https://saga.example/\"]\n");
    let b0 = w.env.read("Stories/b.md").unwrap();
    let c0 = w.env.read("Stories/c.md").unwrap();
    let o = w.reconcile(false);
    assert_eq!(n(&o, "publication_backfilled"), 1);
    let a = w.env.read("Stories/a.md").unwrap();
    assert!(a.contains("  - \"[[Ann|A]]\"\npublication: Saga\n"), "{a}");
    assert_eq!(w.env.read("Stories/b.md").unwrap(), b0, "never overwritten");
    assert_eq!(
        w.env.read("Stories/c.md").unwrap(),
        c0,
        "prefix mismatch (a prefix decides alone, Decision 7)"
    );
    assert_eq!(n(&w.reconcile(false), "publication_backfilled"), 0);
}

#[test]
fn reclean_r2_drops_a_small_localized_image_by_its_decoded_size() {
    let note = |name: &str| {
        format!(
            "---\nvia: email\n---\n\n![](_attachments/{name}.png)\n\nReal prose paragraph one.\n"
        )
    };
    let w = world(&[
        ("mailnotes/s.md", note("small")),
        ("mailnotes/l.md", note("large")),
    ]);
    let dir = w.env.vault().join("_attachments");
    std::fs::create_dir_all(&dir).unwrap();
    image::RgbImage::new(36, 36)
        .save(dir.join("small.png"))
        .unwrap();
    image::RgbImage::new(300, 200)
        .save(dir.join("large.png"))
        .unwrap();
    let o = w.reconcile(false);
    assert_eq!(n(&o, "mail_recleaned_r2"), 1);
    assert!(!w.env.read("mailnotes/s.md").unwrap().contains("small.png"));
    assert!(w.env.read("mailnotes/l.md").unwrap().contains("large.png"));
    assert!(
        w.env
            .read("mailnotes/s.md")
            .unwrap()
            .contains("Real prose paragraph one.")
    );
    assert_eq!(n(&w.reconcile(false), "mail_recleaned_r2"), 0);
}

/// A pre-D25 RSS note carries no `via`; its `blog` tracker row makes it RSS-origin, so its mail
/// twin merges.
#[test]
fn a_pre_d25_rss_note_with_a_blog_row_merges_with_its_mail_twin() {
    let rss =
        format!("---\ntitle: \"Slug One\"\nsource: {ZED}\ncomplete: false\n---\n\nA teaser.\n");
    let w = zed_world(rss, mail_note(true, LONG));
    add_row(
        &w,
        "blog",
        "https://zed.substack.com/feed",
        "z1",
        TrackerOutcome::Note,
        Some("Letters/rss.md"),
    );
    let o = w.reconcile(false);
    assert_eq!((n(&o, "dup_merged"), n(&o, "dup_other")), (1, 0));
    assert!(!w.env.vault().join("mailnotes/m.md").exists());
    assert!(w.env.read("Letters/rss.md").unwrap().contains(LONG));
}

#[test]
fn description_is_recomputed_on_reclean_and_css_only_when_it_carries_css() {
    let css = "---\ntitle: \"T\"\ndescription: \"@media (max-width:600px){.x{display:none}} Hello\"\nvia: email\n---\n\nHello there reader.\n";
    let plain = "---\ntitle: \"T\"\ndescription: \"My own words\"\nvia: email\n---\n\nHello there reader.\n";
    let chrome = format!("---\ntitle: \"T\"\ndescription: \"stale\"\nvia: email\n---\n\n{CHROME}");
    let w = world(&[
        ("mailnotes/css.md", css.to_owned()),
        ("mailnotes/plain.md", plain.to_owned()),
        ("mailnotes/chrome.md", chrome),
    ]);
    let o = w.reconcile(false);
    assert_eq!(
        (
            n(&o, "mail_description_repaired"),
            n(&o, "mail_recleaned_r2")
        ),
        (2, 1)
    );
    let c = w.env.read("mailnotes/css.md").unwrap();
    assert!(
        c.contains("description: \"Hello there reader.\"\n")
            || c.contains("description: Hello there reader.\n"),
        "{c}"
    );
    assert_eq!(w.env.read("mailnotes/plain.md").unwrap(), plain);
    let ch = w.env.read("mailnotes/chrome.md").unwrap();
    assert!(
        !ch.contains("stale") && ch.contains("Real prose paragraph one."),
        "{ch}"
    );
    let again = w.reconcile(false);
    assert_eq!(
        (
            n(&again, "mail_description_repaired"),
            n(&again, "mail_recleaned_r2")
        ),
        (0, 0)
    );
}

#[test]
fn css_predicate_is_narrow() {
    use super::reconcile_backfill::has_css;
    assert!(has_css("@media screen { a { color: red } } hi"));
    assert!(has_css(".btn{display:none} Read more"));
    assert!(has_css("p { margin-top: 0; }"));
    assert!(!has_css("Hello {name}, welcome { to the list }"));
    assert!(!has_css("A note about JSON {\"Key\": 1} values"));
    assert!(!has_css("Plain prose: no braces at all."));
}
