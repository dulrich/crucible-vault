//! Source-eval training rows and label rules (plan `v0-automation-pipelines` WP-10), pure.
//!
//! Ported from Crucible `src/sourceEval/export.ts`: the row shape `SourceEvalTrainingExample`
//! (`:35-51`), `buildTrainingExamples` (`:166-200`), the human rule `buildHumanTrainingExample`
//! (`:202-220`: `eval-importance` set and not `eval-skip`), the weak rule
//! `buildWeakCaptureTrainingExample` (`:222-246`: no human importance; `#low-value` → 0,
//! `#gold` or any Observations month → max(…, 4), else no row; `rated` = today), `labelTags`
//! (`:432-440`), `normalizeSourceTags` (`:442-449`) and the JSONL shape (`:248-251`). The caller
//! (the `cv signals export` CLI) gathers the inputs from the index and does the JSON emission;
//! this module has no serde.

/// Label tags carried into `label.tags` (`LABEL_TAGS`, `export.ts:104`).
pub const LABEL_TAGS: &[&str] = &["gold", "high-value", "revisit", "reference", "low-value"];

/// `human` or `weak`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelSource {
    /// A human `eval-importance`.
    Human,
    /// Derived from tags / observations.
    Weak,
}

impl LabelSource {
    /// The JSON spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Weak => "weak",
        }
    }
}

/// `label` of a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainingLabel {
    /// 0..=5.
    pub importance: i64,
    /// Urgent flag.
    pub urgent: bool,
    /// Label tags (subset of [`LABEL_TAGS`]).
    pub tags: Vec<String>,
}

/// The row fields that do not depend on the label (`buildCaptureBaseExample`, `export.ts:253-279`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowBase {
    /// Video id / post id.
    pub id: String,
    /// `youtube` | `blog`.
    pub source_type: String,
    /// Channel id / blog key (no `type:` prefix).
    pub source_key: String,
    /// Source display name.
    pub source_name: String,
    /// `#`-prefixed, lowercased, deduplicated.
    pub source_tags: Vec<String>,
    /// `high` | `normal` | `low` | `skip` | `ignore`.
    pub source_priority: String,
    /// Title.
    pub title: String,
    /// Description.
    pub description: Option<String>,
    /// Author.
    pub author: Option<String>,
    /// Published date.
    pub published: Option<String>,
    /// Word count.
    pub word_count: Option<i64>,
    /// Duration in seconds.
    pub duration_seconds: Option<i64>,
}

/// One JSONL row (`SourceEvalTrainingExample`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainingRow {
    /// Label-independent fields.
    pub base: RowBase,
    /// The label.
    pub label: TrainingLabel,
    /// `human` | `weak`.
    pub label_source: LabelSource,
    /// Rating date.
    pub rated: Option<String>,
}

/// The human eval label of a capture (from `cv_index::signals::EvalLabel`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureLabel {
    /// `eval-importance`.
    pub importance: Option<i64>,
    /// `eval-urgent`.
    pub urgent: bool,
    /// `eval-rated`.
    pub rated: Option<String>,
    /// `eval-tags`.
    pub tags: Vec<String>,
}

/// One attributed capture with its signals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureInput {
    /// Row fields.
    pub base: RowBase,
    /// Note tags (no `#`).
    pub tags: Vec<String>,
    /// Human label, if any.
    pub label: Option<CaptureLabel>,
    /// `eval-skip: true`.
    pub eval_skip: bool,
    /// Monthly `# Observations` notes citing the capture.
    pub observed_months: u32,
}

/// `normalizeTag`: trim, strip leading `#`s, lowercase.
#[must_use]
pub fn normalize_tag(tag: &str) -> String {
    tag.trim().trim_start_matches('#').to_lowercase()
}

/// `normalizeSourceTags`: `#`-prefixed normalized tags, deduplicated.
#[must_use]
pub fn normalize_source_tags(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let n = normalize_tag(t);
        let n = format!("#{n}");
        if n.len() > 1 && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// `labelTags`: note tags + eval tags restricted to [`LABEL_TAGS`].
#[must_use]
pub fn label_tags(note_tags: &[String], eval_tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in note_tags.iter().chain(eval_tags) {
        let n = normalize_tag(t);
        if LABEL_TAGS.contains(&n.as_str()) && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// `buildHumanTrainingExample`: `eval-importance` set and not `eval-skip`.
#[must_use]
pub fn human_row(c: &CaptureInput) -> Option<TrainingRow> {
    if c.eval_skip || c.base.id.is_empty() {
        return None;
    }
    let label = c.label.as_ref()?;
    let importance = label.importance?;
    Some(TrainingRow {
        base: c.base.clone(),
        label: TrainingLabel {
            importance,
            urgent: label.urgent,
            tags: label_tags(&c.tags, &label.tags),
        },
        label_source: LabelSource::Human,
        rated: label.rated.clone(),
    })
}

/// `buildWeakCaptureTrainingExample`: no human importance; `low-value` → 0, `gold` or an
/// observation month → max(…, 4); `rated` = `today`.
#[must_use]
pub fn weak_row(c: &CaptureInput, today: &str) -> Option<TrainingRow> {
    if c.eval_skip
        || c.base.id.is_empty()
        || c.label.as_ref().is_some_and(|l| l.importance.is_some())
    {
        return None;
    }
    let tags: Vec<String> = c.tags.iter().map(|t| normalize_tag(t)).collect();
    let mut importance: Option<i64> = None;
    if tags.iter().any(|t| t == "low-value") {
        importance = Some(0);
    }
    if tags.iter().any(|t| t == "gold") {
        importance = Some(importance.unwrap_or(0).max(4));
    }
    if c.observed_months > 0 {
        importance = Some(importance.unwrap_or(0).max(4));
    }
    let empty = Vec::new();
    Some(TrainingRow {
        base: c.base.clone(),
        label: TrainingLabel {
            importance: importance?,
            urgent: false,
            tags: label_tags(&c.tags, c.label.as_ref().map_or(&empty, |l| &l.tags)),
        },
        label_source: LabelSource::Weak,
        rated: Some(today.to_owned()),
    })
}

/// `buildTrainingExamples` (captures only): human rows first, then (with `include_weak`) weak
/// rows whose `source_type:id` has no row yet.
#[must_use]
pub fn build_rows(captures: &[CaptureInput], include_weak: bool, today: &str) -> Vec<TrainingRow> {
    let key = |r: &TrainingRow| format!("{}:{}", r.base.source_type, r.base.id);
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for c in captures {
        if let Some(r) = human_row(c) {
            seen.insert(key(&r));
            rows.push(r);
        }
    }
    if include_weak {
        for c in captures {
            if let Some(r) = weak_row(c, today)
                && seen.insert(key(&r))
            {
                rows.push(r);
            }
        }
    }
    rows
}

/// The export file name (`training-<YYYY-MM-DD>.jsonl`, `export.ts:117`).
#[must_use]
pub fn export_file_name(today: &str) -> String {
    format!("training-{today}.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(id: &str, tags: &[&str], importance: Option<i64>, months: u32) -> CaptureInput {
        CaptureInput {
            base: RowBase {
                id: id.into(),
                source_type: "youtube".into(),
                ..RowBase::default()
            },
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            label: importance.map(|i| CaptureLabel {
                importance: Some(i),
                urgent: true,
                rated: Some("2026-10-01".into()),
                tags: vec!["#Revisit".into(), "other".into()],
            }),
            eval_skip: false,
            observed_months: months,
        }
    }

    #[test]
    fn human_and_weak_rules() {
        let h = human_row(&cap("a", &["gold", "x"], Some(3), 0)).unwrap();
        assert_eq!(h.label_source, LabelSource::Human);
        assert_eq!(
            h.label,
            TrainingLabel {
                importance: 3,
                urgent: true,
                tags: vec!["gold".into(), "revisit".into()]
            }
        );
        assert_eq!(h.rated.as_deref(), Some("2026-10-01"));
        let mut skip = cap("a", &[], Some(3), 0);
        skip.eval_skip = true;
        assert!(human_row(&skip).is_none());

        let t = "2026-10-15";
        assert_eq!(
            weak_row(&cap("s", &["#low-value"], None, 0), t)
                .unwrap()
                .label
                .importance,
            0
        );
        assert_eq!(
            weak_row(&cap("g", &["Gold"], None, 0), t)
                .unwrap()
                .label
                .importance,
            4
        );
        let o = weak_row(&cap("o", &[], None, 1), t).unwrap();
        assert_eq!((o.label.importance, o.rated.as_deref()), (4, Some(t)));
        assert!(weak_row(&cap("n", &["x"], None, 0), t).is_none());
        // Human wins over weak; weak only with include_weak.
        let caps = [
            cap("a", &["gold"], Some(1), 2),
            cap("g", &["gold"], None, 0),
        ];
        let rows = build_rows(&caps, true, t);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].label.importance, rows[0].label_source),
            (1, LabelSource::Human)
        );
        assert_eq!(build_rows(&caps, false, t).len(), 1);
        assert_eq!(normalize_source_tags(&["AI".into(), "#ai".into()]), ["#ai"]);
    }
}
