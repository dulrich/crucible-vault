//! The `Lint: all` pipeline — Crucible `LINT_STEPS` (`https://github.com/dulrich/obsidian-crucible/blob/master/src/lint.ts:176-376`,
//! quirk `src/AGENTS.md:33,67`) as a pure function `(note text, ctx, settings) -> edited text`.
//!
//! **Dropped (Obsidian-induced):** running the frontmatter sub-steps inside ONE
//! `updateFrontmatter` callback, which existed for Obsidian's stale-metadata-cache write barrier.
//! Here every step edits one in-memory [`FrontmatterDoc`] and [`lint_note`] returns the final
//! text, so one lint run yields at most one write by construction. Also dropped from the
//! registry, because they were pipeline mechanics of the plugin rather than content transforms:
//! `excluded-folder-guard` (the caller filters paths), `read-and-word-count` / `re-read-diff`
//! (the caller reads; [`LintOutcome::changed`] is the diff), `dataview-refresh` and `notice`
//! (client UI). Localize is deliberately NOT part of lint-all (`src/AGENTS.md:21`).

use std::collections::BTreeMap;

use cv_text::PostIdOptions;
use ofm::frontmatter::JsValue;

use crate::frontmatter::{FmError, FrontmatterDoc};
use crate::wordcount::calculate_word_count;

/// Whether a step edits frontmatter or the note body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Frontmatter,
    Content,
}

/// One registry row (Crucible `LintStep` minus `run`, which is dispatched by id).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LintStep {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: StepKind,
    /// User-toggleable via [`LintSettings::step_enabled`] (absent = enabled).
    pub toggleable: bool,
    /// For non-toggleable gated steps: the setting that governs them.
    pub configured_by_setting: Option<&'static str>,
}

const fn step(
    id: &'static str,
    label: &'static str,
    kind: StepKind,
    toggleable: bool,
    configured_by_setting: Option<&'static str>,
) -> LintStep {
    LintStep {
        id,
        label,
        kind,
        toggleable,
        configured_by_setting,
    }
}

/// The pipeline in fire order.
pub const LINT_STEPS: &[LintStep] = &[
    step(
        "insert-keys",
        "Insert configured frontmatter keys",
        StepKind::Frontmatter,
        false,
        Some("frontmatter_insert"),
    ),
    step(
        "created-date",
        "Stamp created date",
        StepKind::Frontmatter,
        false,
        Some("created_key"),
    ),
    step(
        "title-stamp",
        "Stamp title",
        StepKind::Frontmatter,
        true,
        None,
    ),
    step(
        "modified-date",
        "Stamp modified date",
        StepKind::Frontmatter,
        false,
        Some("modified_key"),
    ),
    step(
        "word-count",
        "Write word count",
        StepKind::Frontmatter,
        true,
        None,
    ),
    step(
        "derive-source-ids",
        "Derive source ID properties",
        StepKind::Frontmatter,
        true,
        None,
    ),
    step(
        "sort-yaml",
        "Sort frontmatter keys",
        StepKind::Frontmatter,
        true,
        None,
    ),
    step(
        "blank-line-after-yaml",
        "Ensure blank line after frontmatter",
        StepKind::Content,
        false,
        Some("blank_line_after_yaml"),
    ),
];

/// Lint settings (Crucible `lint*` settings).
#[derive(Debug, Clone, PartialEq)]
pub struct LintSettings {
    /// Already-rendered `lintFrontmatterInsert` text (`key: value` lines). The caller renders
    /// the template with `cv_text::apply_template` and an explicit `now` — keeps this crate
    /// clock-free.
    pub frontmatter_insert: String,
    /// Blank disables `created-date` (Crucible's asymmetry fix).
    pub created_key: String,
    /// Blank disables `modified-date`.
    pub modified_key: String,
    pub blank_line_after_yaml: bool,
    pub yaml_key_priority: Vec<String>,
    /// Absent id = enabled.
    pub step_enabled: BTreeMap<String, bool>,
    /// Blog post-id rules for `derive-source-ids`.
    pub post_id: PostIdOptions,
}

impl Default for LintSettings {
    fn default() -> Self {
        Self {
            frontmatter_insert: String::new(),
            created_key: "created".into(),
            modified_key: "updated".into(),
            blank_line_after_yaml: false,
            yaml_key_priority: Vec::new(),
            step_enabled: BTreeMap::new(),
            post_id: PostIdOptions::default(),
        }
    }
}

/// Per-note inputs; time-dependent values are explicit (no clock reads).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintContext {
    /// File basename (title stamp).
    pub basename: String,
    /// Today's date, already formatted (`YYYY-MM-DD`).
    pub today: String,
    /// The note's creation date, already formatted.
    pub created: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintOutcome {
    pub text: String,
    pub changed: bool,
    pub word_count: usize,
}

impl LintSettings {
    fn gate(&self, s: &LintStep) -> bool {
        match s.id {
            "created-date" => !self.created_key.trim().is_empty(),
            "modified-date" => !self.modified_key.trim().is_empty(),
            "blank-line-after-yaml" => self.blank_line_after_yaml,
            "insert-keys" => !self.frontmatter_insert.trim().is_empty(),
            id if s.toggleable => self.step_enabled.get(id).copied() != Some(false),
            _ => true,
        }
    }
}

/// Crucible `parse-frontmatter-insert`: split each line at its first `:`.
fn parse_insert(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            let k = k.trim();
            (!k.is_empty()).then(|| (k.to_owned(), v.trim().to_owned()))
        })
        .collect()
}

/// Crucible `deriveSourceIdProperties` (`src/lint.ts:24-43`, `src/ingestion/AGENTS.md:18,26`):
/// from `source` only, never the body — `yt-video-id` for a YouTube URL, else `post-id` for a
/// parseable http(s) URL. Reuses `cv-text`'s extractors.
pub fn derive_source_ids(
    doc: &mut FrontmatterDoc,
    post_id: &PostIdOptions,
) -> Result<bool, FmError> {
    let Some(JsValue::Str(raw)) = doc.get("source")? else {
        return Ok(false);
    };
    let source = raw.trim();
    if source.is_empty() {
        return Ok(false);
    }
    if let Some(id) = cv_text::extract_video_id_from_url(source) {
        return doc.set_if_empty("yt-video-id", &JsValue::Str(id));
    }
    // `canonicalize_url` is None for non-http(s) or unparseable input — Crucible's
    // `/^https?:\/\//` + `new URL` guard.
    if cv_text::canonicalize_url(source).is_none() {
        return Ok(false);
    }
    let id = cv_text::post_id_from_url(source, post_id);
    doc.set_if_empty("post-id", &JsValue::Str(id))
}

fn run_step(
    id: &str,
    doc: &mut FrontmatterDoc,
    ctx: &LintContext,
    settings: &LintSettings,
    word_count: usize,
) -> Result<(), FmError> {
    match id {
        "insert-keys" => {
            for (k, v) in parse_insert(&settings.frontmatter_insert) {
                doc.set_if_empty(&k, &JsValue::Str(v))?;
            }
        }
        "created-date" => {
            doc.set_if_empty(&settings.created_key, &JsValue::Str(ctx.created.clone()))?;
        }
        "title-stamp" => {
            doc.set_if_empty("title", &JsValue::Str(ctx.basename.clone()))?;
        }
        "modified-date" => {
            doc.set(&settings.modified_key, &JsValue::Str(ctx.today.clone()))?;
        }
        "word-count" => {
            #[allow(clippy::cast_precision_loss)]
            doc.set("word-count", &JsValue::Num(word_count as f64))?;
        }
        "derive-source-ids" => {
            derive_source_ids(doc, &settings.post_id)?;
        }
        "sort-yaml" => {
            doc.sort_keys(&settings.yaml_key_priority)?;
        }
        "blank-line-after-yaml" => {
            doc.ensure_blank_line_after();
        }
        _ => {}
    }
    Ok(())
}

/// Run every enabled step in [`LINT_STEPS`] order over `text`. The word count is computed from
/// the original text (as Crucible's `read-and-word-count` did, before any write).
pub fn lint_note(
    text: &str,
    ctx: &LintContext,
    settings: &LintSettings,
) -> Result<LintOutcome, FmError> {
    let word_count = calculate_word_count(text);
    let mut doc = FrontmatterDoc::new(text);
    for s in LINT_STEPS {
        if settings.gate(s) {
            run_step(s.id, &mut doc, ctx, settings, word_count)?;
        }
    }
    let out = doc.into_text();
    Ok(LintOutcome {
        changed: out != text,
        text: out,
        word_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vectors ported from https://github.com/dulrich/obsidian-crucible/blob/master/tests/lintStepRegistry.test.mjs.

    const BODY: &str = "Some body text here.";
    const YT: &str = "https://youtu.be/dQw4w9WgXcQ";

    fn base() -> LintSettings {
        LintSettings {
            yaml_key_priority: ["title", "created", "updated", "word-count"]
                .map(String::from)
                .to_vec(),
            ..LintSettings::default()
        }
    }

    fn ctx() -> LintContext {
        LintContext {
            basename: "note".into(),
            today: "2026-07-29".into(),
            created: "2026-07-29".into(),
        }
    }

    fn content() -> String {
        format!("---\nsource: {YT}\n---\n\n{BODY}")
    }

    fn lint(s: &LintSettings) -> LintOutcome {
        lint_note(&content(), &ctx(), s).unwrap()
    }

    fn keys(text: &str) -> Vec<String> {
        FrontmatterDoc::new(text).keys().unwrap()
    }

    #[test]
    fn registry_fire_order() {
        let ids: Vec<&str> = LINT_STEPS.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            [
                "insert-keys",
                "created-date",
                "title-stamp",
                "modified-date",
                "word-count",
                "derive-source-ids",
                "sort-yaml",
                "blank-line-after-yaml"
            ]
        );
    }

    #[test]
    fn exactly_four_toggleable() {
        let mut t: Vec<&str> = LINT_STEPS
            .iter()
            .filter(|s| s.toggleable)
            .map(|s| s.id)
            .collect();
        t.sort_unstable();
        assert_eq!(
            t,
            [
                "derive-source-ids",
                "sort-yaml",
                "title-stamp",
                "word-count"
            ]
        );
    }

    #[test]
    fn all_defaults_output() {
        let o = lint(&base());
        assert!(o.changed);
        assert_eq!(o.word_count, 4);
        assert_eq!(
            o.text,
            format!(
                "---\ntitle: note\ncreated: 2026-07-29\nupdated: 2026-07-29\nword-count: 4\nsource: {YT}\nyt-video-id: dQw4w9WgXcQ\n---\n\n{BODY}"
            )
        );
        assert_eq!(
            keys(&o.text)[..4],
            ["title", "created", "updated", "word-count"]
        );
        // Idempotent: a second run is a no-op.
        let again = lint_note(&o.text, &ctx(), &base()).unwrap();
        assert!(!again.changed);
    }

    fn disabled(id: &str) -> LintSettings {
        let mut s = base();
        s.step_enabled.insert(id.into(), false);
        s
    }

    #[test]
    fn disabling_title_stamp() {
        let t = lint(&disabled("title-stamp")).text;
        assert!(!t.contains("title:"));
        for k in [
            "created: 2026-07-29",
            "updated: 2026-07-29",
            "yt-video-id: dQw4w9WgXcQ",
            "word-count: 4",
        ] {
            assert!(t.contains(k), "{k}");
        }
    }

    #[test]
    fn disabling_word_count() {
        let t = lint(&disabled("word-count")).text;
        assert!(!t.contains("word-count:"));
        for k in [
            "title: note",
            "created: 2026-07-29",
            "yt-video-id: dQw4w9WgXcQ",
        ] {
            assert!(t.contains(k), "{k}");
        }
    }

    #[test]
    fn disabling_derive_source_ids() {
        let t = lint(&disabled("derive-source-ids")).text;
        assert!(!t.contains("yt-video-id:"));
        assert!(t.contains("title: note") && t.contains("word-count: 4"));
    }

    #[test]
    fn disabling_sort_yaml_preserves_order() {
        let src = format!("---\nword-count: 1\nsource: {YT}\n---\n\n{BODY}");
        let t = lint_note(&src, &ctx(), &disabled("sort-yaml"))
            .unwrap()
            .text;
        assert_eq!(keys(&t)[0], "word-count");
    }

    #[test]
    fn blank_created_key_disables_created() {
        let s = LintSettings {
            created_key: String::new(),
            ..base()
        };
        let t = lint(&s).text;
        assert!(!t.contains("created:"));
        assert!(t.contains("title: note") && t.contains("updated: 2026-07-29"));
    }

    #[test]
    fn blank_modified_key_disables_modified() {
        let s = LintSettings {
            modified_key: String::new(),
            ..base()
        };
        let t = lint(&s).text;
        assert!(!t.contains("updated:"));
        assert!(t.contains("created: 2026-07-29"));
    }

    #[test]
    fn insert_keys_and_blank_line() {
        let s = LintSettings {
            frontmatter_insert: "status: inbox\nurl: https://a.b/c\nbad line".into(),
            blank_line_after_yaml: true,
            ..base()
        };
        let o = lint_note("---\nstatus: done\n---\nBody", &ctx(), &s).unwrap();
        assert!(o.text.contains("status: done\n"));
        assert!(o.text.contains("url: https://a.b/c\n"));
        assert!(o.text.ends_with("---\n\nBody"));
    }

    #[test]
    fn derive_post_id_from_source_only() {
        let src = "---\nsource: https://example.com/post/?utm_source=x\n---\nhttps://youtu.be/dQw4w9WgXcQ\n";
        let t = lint(&LintSettings::default()).text;
        assert!(t.contains("yt-video-id"));
        let o = lint_note(src, &ctx(), &LintSettings::default())
            .unwrap()
            .text;
        assert!(o.contains("post-id: https://example.com/post\n"), "{o}");
        assert!(!o.contains("yt-video-id"));
        let none = lint_note(
            "---\nsource: not a url\n---\n",
            &ctx(),
            &LintSettings::default(),
        )
        .unwrap()
        .text;
        assert!(!none.contains("post-id"));
    }

    #[test]
    fn untouched_keys_byte_identical_through_lint() {
        let src = "---\nalias: 'Odd  Spacing'  # c\n# keep me\ntags: [x, y]\n---\nword\n";
        let s = LintSettings {
            step_enabled: [("sort-yaml".to_owned(), false)].into(),
            ..base()
        };
        let o = lint_note(src, &ctx(), &s).unwrap().text;
        assert!(
            o.starts_with("---\nalias: 'Odd  Spacing'  # c\n# keep me\ntags: [x, y]\n"),
            "{o}"
        );
    }
}
