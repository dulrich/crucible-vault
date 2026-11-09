//! Source-eval training rows and label rules (plan `v0-automation-pipelines` WP-10), pure.
//!
//! Ported from Crucible `src/sourceEval/export.ts`: the row shape `SourceEvalTrainingExample`
//! (`:35-51`), `buildTrainingExamples` (`:166-200`), the human rule `buildHumanTrainingExample`
//! (`:202-220`: `eval-importance` set and not `eval-skip`), the weak rule
//! `buildWeakCaptureTrainingExample` (`:222-246`: no human importance; configured label rules
//! and the Observations floor, else no row; `rated` = today), `labelTags` (`:432-440`),
//! `normalizeSourceTags` (`:442-449`) and the JSONL shape (`:248-251`). The caller (the
//! `cv signals export` CLI) gathers the inputs from the index, parses the label configuration
//! into [`LabelRules`] and does the JSON emission; this module has no serde.
//!
//! Crucible hard-coded its label vocabulary (`LABEL_TAGS` `export.ts:104`; `#low-value` →
//! 0, `#gold` / an Observations month → max(…, 4)); here it is configuration
//! (`vault.toml [signals]`, plan public-release WP-4, C#27/C#29) with an empty default.
//!
//! **Published eval-label schema** (C#28; the frontmatter keys a human label is read from,
//! [`parse_eval_label`]): `eval-importance` (integer 0..=5, number or numeric string),
//! `eval-urgent` (`true`), `eval-rated` (date string), `eval-tags` (list or comma string, `#`
//! optional), and `eval-skip` (`true` excludes the note from every row).

/// One configured label tag (`vault.toml [signals.labels.<tag>]`, C#27). Every configured
/// tag is carried into `label.tags`; the optional rule shapes the weak importance (0..=5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelRule {
    /// The tag, normalized ([`normalize_tag`]).
    pub tag: String,
    /// A fixed weak importance.
    pub value: Option<i64>,
    /// A weak importance floor (`max(importance or 0, floor)`).
    pub floor: Option<i64>,
    /// A weak importance ceiling (clamps an importance already set; never creates a row).
    pub ceiling: Option<i64>,
}

/// The label configuration [`weak_row`] and [`label_tags`] read. The default (empty) carries
/// no label tags and derives no weak importance.
///
/// Weak importance order (fixed, independent of table order): (1) every matching `value`,
/// the lowest wins; (2) every matching `ceiling` clamps a set importance, the lowest wins;
/// (3) every matching `floor`, then `observed_floor` when an Observations section cites the
/// capture, raise it to `max(importance or 0, floor)`. No rule matched = no weak row. The
/// former built-ins are `low-value = { value = 0 }`, `gold = { floor = 4 }` and
/// `observed_floor = 4`, with `high-value`, `revisit`, `reference` carried without a rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelRules {
    /// Configured label tags, in config order.
    pub labels: Vec<LabelRule>,
    /// The importance floor of a capture an Observations section cites; `None` = the
    /// Observations signal has no effect.
    pub observed_floor: Option<i64>,
}

impl LabelRules {
    fn carries(&self, tag: &str) -> bool {
        self.labels.iter().any(|l| l.tag == tag)
    }
}

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
    /// Label tags (subset of the configured [`LabelRules`] tags).
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

/// The human eval label of a capture ([`parse_eval_label`]).
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

/// One raw frontmatter value as the label rule reads it (the caller maps its JSON: a list
/// keeps only its string items).
#[derive(Debug, Clone, PartialEq)]
pub enum RawProp<'a> {
    /// A number.
    Num(f64),
    /// A string.
    Str(&'a str),
    /// A boolean.
    Bool(bool),
    /// A list's string items.
    List(Vec<&'a str>),
}

/// Crucible `parseEvalLabel` (`src/sourceEval/signals.ts:4-15`) over the note's properties
/// (`prop(key)`): `eval-importance` (0..=5, integral; number or numeric string),
/// `eval-urgent === true`, trimmed non-empty `eval-rated`, `eval-tags` (list or comma string,
/// `#` stripped, deduplicated in order). `None` when every field is empty.
#[must_use]
pub fn parse_eval_label<'a>(prop: impl Fn(&str) -> Option<RawProp<'a>>) -> Option<CaptureLabel> {
    let importance = match prop("eval-importance") {
        Some(RawProp::Num(n)) => Some(n),
        Some(RawProp::Str(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|n| n.is_finite() && (0.0..=5.0).contains(n) && n.fract() == 0.0)
    .map(|n| n as i64);
    let urgent = prop("eval-urgent") == Some(RawProp::Bool(true));
    let rated = match prop("eval-rated") {
        Some(RawProp::Str(s)) => Some(s.trim()).filter(|s| !s.is_empty()).map(str::to_owned),
        _ => None,
    };
    let raw: Vec<&str> = match prop("eval-tags") {
        Some(RawProp::List(a)) => a,
        Some(RawProp::Str(s)) => s.split(',').collect(),
        _ => Vec::new(),
    };
    let mut tags: Vec<String> = Vec::new();
    for t in raw {
        let t = t.trim().trim_start_matches('#');
        if !t.is_empty() && !tags.iter().any(|x| x == t) {
            tags.push(t.to_owned());
        }
    }
    if importance.is_none() && !urgent && rated.is_none() && tags.is_empty() {
        return None;
    }
    Some(CaptureLabel {
        importance,
        urgent,
        rated,
        tags,
    })
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

/// `labelTags`: note tags + eval tags restricted to the configured label tags.
#[must_use]
pub fn label_tags(note_tags: &[String], eval_tags: &[String], rules: &LabelRules) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in note_tags.iter().chain(eval_tags) {
        let n = normalize_tag(t);
        if rules.carries(&n) && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// `buildHumanTrainingExample`: `eval-importance` set and not `eval-skip`.
#[must_use]
pub fn human_row(c: &CaptureInput, rules: &LabelRules) -> Option<TrainingRow> {
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
            tags: label_tags(&c.tags, &label.tags, rules),
        },
        label_source: LabelSource::Human,
        rated: label.rated.clone(),
    })
}

/// `buildWeakCaptureTrainingExample`: no human importance; the [`LabelRules`] order (values,
/// ceilings, floors, the Observations floor); `rated` = `today`.
#[must_use]
pub fn weak_row(c: &CaptureInput, today: &str, rules: &LabelRules) -> Option<TrainingRow> {
    if c.eval_skip
        || c.base.id.is_empty()
        || c.label.as_ref().is_some_and(|l| l.importance.is_some())
    {
        return None;
    }
    let tags: Vec<String> = c.tags.iter().map(|t| normalize_tag(t)).collect();
    let matching: Vec<&LabelRule> = rules
        .labels
        .iter()
        .filter(|r| tags.contains(&r.tag))
        .collect();
    let mut importance: Option<i64> = matching.iter().filter_map(|r| r.value).min();
    if let Some(ceiling) = matching.iter().filter_map(|r| r.ceiling).min() {
        importance = importance.map(|i| i.min(ceiling));
    }
    let observed = rules.observed_floor.filter(|_| c.observed_months > 0);
    for floor in matching.iter().filter_map(|r| r.floor).chain(observed) {
        importance = Some(importance.unwrap_or(0).max(floor));
    }
    let empty = Vec::new();
    Some(TrainingRow {
        base: c.base.clone(),
        label: TrainingLabel {
            importance: importance?,
            urgent: false,
            tags: label_tags(&c.tags, c.label.as_ref().map_or(&empty, |l| &l.tags), rules),
        },
        label_source: LabelSource::Weak,
        rated: Some(today.to_owned()),
    })
}

/// `buildTrainingExamples` (captures only): human rows first, then (with `include_weak`) weak
/// rows whose `source_type:id` has no row yet.
#[must_use]
pub fn build_rows(
    captures: &[CaptureInput],
    include_weak: bool,
    today: &str,
    rules: &LabelRules,
) -> Vec<TrainingRow> {
    let key = |r: &TrainingRow| format!("{}:{}", r.base.source_type, r.base.id);
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for c in captures {
        if let Some(r) = human_row(c, rules) {
            seen.insert(key(&r));
            rows.push(r);
        }
    }
    if include_weak {
        for c in captures {
            if let Some(r) = weak_row(c, today, rules)
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
    use super::{RawProp, parse_eval_label};

    fn label(props: &[(&str, RawProp<'static>)]) -> Option<super::CaptureLabel> {
        parse_eval_label(|k| props.iter().find(|(p, _)| *p == k).map(|(_, v)| v.clone()))
    }

    /// Moved from `cv_index::signals` (M3).
    #[test]
    fn eval_label_parsing() {
        assert_eq!(label(&[("title", RawProp::Str("x"))]), None);
        assert_eq!(
            label(&[
                ("eval-urgent", RawProp::Str("true")),
                ("eval-rated", RawProp::Str("  "))
            ]),
            None
        );
        let l = label(&[
            ("eval-importance", RawProp::Str("3")),
            ("eval-urgent", RawProp::Bool(true)),
            ("eval-rated", RawProp::Str(" 2026-10-01 ")),
            ("eval-tags", RawProp::Str("#gold, revisit,gold")),
        ])
        .unwrap();
        assert_eq!(l.importance, Some(3));
        assert!(l.urgent);
        assert_eq!(l.rated.as_deref(), Some("2026-10-01"));
        assert_eq!(l.tags, ["gold", "revisit"]);
        let l = label(&[
            ("eval-importance", RawProp::Num(9.0)),
            ("eval-tags", RawProp::List(vec!["a"])),
        ])
        .unwrap();
        assert_eq!((l.importance, l.tags.len()), (None, 1));
    }

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
        let h = human_row(&cap("a", &["gold", "x"], Some(3), 0), &owner()).unwrap();
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
        assert!(human_row(&skip, &owner()).is_none());

        let t = "2026-10-15";
        assert_eq!(
            weak_row(&cap("s", &["#low-value"], None, 0), t, &owner())
                .unwrap()
                .label
                .importance,
            0
        );
        assert_eq!(
            weak_row(&cap("g", &["Gold"], None, 0), t, &owner())
                .unwrap()
                .label
                .importance,
            4
        );
        let o = weak_row(&cap("o", &[], None, 1), t, &owner()).unwrap();
        assert_eq!((o.label.importance, o.rated.as_deref()), (4, Some(t)));
        assert!(weak_row(&cap("n", &["x"], None, 0), t, &owner()).is_none());
        // Human wins over weak; weak only with include_weak.
        let caps = [
            cap("a", &["gold"], Some(1), 2),
            cap("g", &["gold"], None, 0),
        ];
        let rows = build_rows(&caps, true, t, &owner());
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].label.importance, rows[0].label_source),
            (1, LabelSource::Human)
        );
        assert_eq!(build_rows(&caps, false, t, &owner()).len(), 1);
        assert_eq!(normalize_source_tags(&["AI".into(), "#ai".into()]), ["#ai"]);
    }

    /// The former built-in vocabulary, stated as configuration (WP-4 pin).
    fn owner() -> LabelRules {
        let rule = |tag: &str, value, floor| LabelRule {
            tag: tag.into(),
            value,
            floor,
            ceiling: None,
        };
        LabelRules {
            labels: vec![
                rule("gold", None, Some(4)),
                rule("high-value", None, None),
                rule("revisit", None, None),
                rule("reference", None, None),
                rule("low-value", Some(0), None),
            ],
            observed_floor: Some(4),
        }
    }

    /// Plan public-release WP-4: slop + gold = 4 (value then floor), table order irrelevant,
    /// a ceiling clamps but never creates a row.
    #[test]
    fn rule_order_and_ceiling() {
        let t = "2026-10-15";
        let mut r = owner();
        r.labels.reverse();
        let both = weak_row(&cap("b", &["low-value", "gold"], None, 0), t, &r).unwrap();
        assert_eq!(both.label.importance, 4);
        assert_eq!(both.label.tags, ["low-value", "gold"]);
        r.labels.push(LabelRule {
            tag: "meh".into(),
            ceiling: Some(2),
            ..LabelRule::default()
        });
        assert!(weak_row(&cap("m", &["meh"], None, 0), t, &r).is_none());
        let c = weak_row(&cap("m", &["meh", "gold"], None, 0), t, &r).unwrap();
        assert_eq!(c.label.importance, 4, "floors apply after ceilings");
        r.labels.push(LabelRule {
            tag: "fav".into(),
            value: Some(5),
            ..LabelRule::default()
        });
        let c = weak_row(&cap("m", &["meh", "fav"], None, 0), t, &r).unwrap();
        assert_eq!(c.label.importance, 2);
    }

    /// Plan public-release WP-4: the empty default carries no label tags and derives no weak
    /// row, Observations months included.
    #[test]
    fn empty_rules_have_no_effect() {
        let t = "2026-10-15";
        let none = LabelRules::default();
        for c in [
            cap("s", &["low-value"], None, 0),
            cap("g", &["gold"], None, 0),
            cap("o", &[], None, 3),
        ] {
            assert!(weak_row(&c, t, &none).is_none());
        }
        let h = human_row(&cap("a", &["gold"], Some(3), 0), &none).unwrap();
        assert!(h.label.tags.is_empty());
    }
}
