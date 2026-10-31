//! `yt_clip_tidy` tests (plan `round-19-yt-cleanup-intake` WP-2): synthetic notes over the
//! shipped example config.

use serde_json::{Value, json};

use super::testkit::{active, example_env};

const STEP: &str = "type = \"yt_clip_tidy\"";
const FM: &str = "---\nsource: https://www.youtube.com/watch?v=dQw4w9WgXcQ\n---\n";
const ENRICH: &str = "%% cv-enrich %%\n> [!abstract]- Summary\n> gist\n\n%% /cv-enrich %%\n";
const TAIL: &str = "## Transcript\n\n**0:01** hi\n";

fn clip(fm: &str) -> String {
    format!(
        "{fm}{ENRICH}\n[![](Clippings/_attachments/t.jpg)](https://www.youtube.com/watch?v=dQw4w9WgXcQ)\n\nDescription.\n\n{TAIL}"
    )
}

#[test]
fn strips_embed_and_description_then_is_idempotent() {
    let e = example_env(&[("Clips/v.md", clip(FM).as_str())]);
    let o = e.run(STEP, json!({}), active("Clips/v.md")).unwrap();
    assert_eq!(o.vars.get("stripped"), Some(&Value::Bool(true)));
    assert_eq!(
        e.read("Clips/v.md").unwrap(),
        format!("{FM}{ENRICH}\n{TAIL}")
    );
    let o = e.run(STEP, json!({}), active("Clips/v.md")).unwrap();
    assert_eq!(o.vars.get("stripped"), Some(&Value::Bool(false)));
}

#[test]
fn non_youtube_source_is_untouched() {
    let other = clip("---\nsource: https://ex.test/post\n---\n");
    let e = example_env(&[("Clips/o.md", other.as_str())]);
    let o = e.run(STEP, json!({}), active("Clips/o.md")).unwrap();
    assert_eq!(o.vars.get("stripped"), Some(&Value::Bool(false)));
    assert_eq!(e.read("Clips/o.md").unwrap(), other);
}

#[test]
fn a_param_disarms() {
    let step = cv_actions::Step::from_table(
        "type = \"yt_clip_tidy\"\nx = 1"
            .parse::<toml::Table>()
            .unwrap(),
    )
    .unwrap();
    assert!(super::validate(&step, &super::testkit::example_config()).is_err());
}

// ---- backfill scope (plan round-19-yt-cleanup-intake WP-3) ------------------------------------

const BACKFILL: &str = "type = \"yt_clip_tidy\"\nscope = \"backfill\"";
const META_PATH: &str = "_yt_metadata/dQw4w9WgXcQ.md";
const META_LINK: &str = "yt-metadata:\n  - \"[[_yt_metadata/dQw4w9WgXcQ]]\"\n";
const TS_TAIL: &str = "## Transcript\n\n**0:01** hello there\n\n**0:05** second part\n";

fn meta(extra: &str) -> String {
    format!("---\nyt-video-id: dQw4w9WgXcQ\n{extra}---\n")
}

fn yt_fm(extra: &str) -> String {
    format!("---\nsource: https://www.youtube.com/watch?v=dQw4w9WgXcQ\nauthor: A\n{extra}---\n")
}

fn count(o: &super::StepOutcome, k: &str) -> u64 {
    o.vars.get(k).and_then(Value::as_u64).unwrap()
}

fn backfill(e: &super::testkit::StepEnv, params: Value) -> super::StepOutcome {
    e.run(BACKFILL, params, Default::default()).unwrap()
}

/// Audit item E1: the composed backfill on an enriched `post` clip with a localized embed, a
/// description and a timestamped transcript.
#[test]
fn backfill_composes_strip_cleanup_and_publication_then_settles() {
    let fm = yt_fm(&format!("tags: [post]\n{META_LINK}"));
    let note = clip(&fm).replace("## Transcript\n\n**0:01** hi\n", TS_TAIL);
    let m = meta("channelTitle: Chan\npublication: Pub\n");
    let e = example_env(&[("Clips/v.md", note.as_str()), (META_PATH, m.as_str())]);
    let o = backfill(&e, json!({}));
    assert_eq!(count(&o, "scanned"), 1);
    assert_eq!(count(&o, "stripped"), 1);
    assert_eq!(count(&o, "cleaned"), 1);
    assert_eq!(count(&o, "publication_filled"), 1);
    let got = e.read("Clips/v.md").unwrap();
    assert!(!got.contains("![]"), "{got}");
    assert!(!got.contains("Description."), "{got}");
    assert!(!got.contains("**0:0"), "{got}");
    assert!(got.contains("author: A\npublication: Pub\n"), "{got}");
    assert!(got.contains(ENRICH), "enrich block must be byte-identical");
    let o = backfill(&e, json!({}));
    assert_eq!(count(&o, "unchanged"), 1);
    assert_eq!(count(&o, "stripped") + count(&o, "cleaned"), 0);
    assert_eq!(count(&o, "publication_filled"), 0);
    assert_eq!(e.read("Clips/v.md").unwrap(), got);
}

/// A `transcript`-kind note: strip only (timestamps stay); the publication falls back to
/// `channelTitle`; a non-empty publication and a non-YouTube note are untouched.
#[test]
fn backfill_strips_only_on_transcript_notes_and_respects_publication() {
    let long = clip(&yt_fm(&format!("tags: [transcript]\n{META_LINK}")))
        .replace("## Transcript\n\n**0:01** hi\n", TS_TAIL);
    let kept = clip(&yt_fm(&format!("publication: Mine\n{META_LINK}")));
    let other = clip("---\nsource: https://ex.test/post\n---\n");
    let unlinked = clip(&yt_fm(""));
    let m = meta("channelTitle: Chan\n");
    let e = example_env(&[
        ("Clips/long.md", long.as_str()),
        ("Clips/kept.md", kept.as_str()),
        ("Clips/other.md", other.as_str()),
        ("Clips/unlinked.md", unlinked.as_str()),
        (META_PATH, m.as_str()),
    ]);
    let o = backfill(&e, json!({}));
    assert_eq!(count(&o, "scanned"), 3);
    assert_eq!(count(&o, "stripped"), 3);
    assert_eq!(count(&o, "cleaned"), 0);
    assert_eq!(count(&o, "publication_filled"), 1);
    assert_eq!(count(&o, "no_metadata"), 1);
    let got = e.read("Clips/long.md").unwrap();
    assert!(got.contains("**0:01** hello there"), "{got}");
    assert!(got.contains("author: A\npublication: Chan\n"), "{got}");
    assert!(
        e.read("Clips/kept.md")
            .unwrap()
            .contains("publication: Mine\n")
    );
    assert_eq!(e.read("Clips/other.md").unwrap(), other);
}

#[test]
fn backfill_dry_run_counts_and_writes_nothing() {
    let note = clip(&yt_fm(&format!("tags: [post]\n{META_LINK}")))
        .replace("## Transcript\n\n**0:01** hi\n", TS_TAIL);
    let m = meta("publication: Pub\n");
    let e = example_env(&[("Clips/v.md", note.as_str()), (META_PATH, m.as_str())]);
    for params in [json!({"dry_run": true}), json!({"dry_run": "true"})] {
        let o = backfill(&e, params);
        assert_eq!(o.vars.get("dry_run"), Some(&Value::Bool(true)));
        assert_eq!(count(&o, "stripped"), 1);
        assert_eq!(count(&o, "cleaned"), 1);
        assert_eq!(count(&o, "publication_filled"), 1);
        assert_eq!(e.read("Clips/v.md").unwrap(), note);
    }
}

#[test]
fn backfill_parse_rejects_bad_scope_and_note_scope_dry_run() {
    for src in [
        "type = \"yt_clip_tidy\"\nscope = \"all\"",
        "type = \"yt_clip_tidy\"\ndry_run = true",
    ] {
        let step = cv_actions::Step::from_table(src.parse::<toml::Table>().unwrap()).unwrap();
        assert!(super::validate(&step, &super::testkit::example_config()).is_err());
    }
}
