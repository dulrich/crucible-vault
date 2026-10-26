//! The TOML definition format (one file = one `.base`) and its compiled form.
//!
//! Keys mirror `.base` YAML 1:1 (`groupBy`, `displayName`, `columnSize` keep their camelCase;
//! snake_case aliases are accepted). Expressions stay strings in Bases syntax.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::expr::{self, Expr};

/// A filter tree: an expression string, or `and`/`or`/`not` lists.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum FilterNode {
    Expr(String),
    And { and: Vec<FilterNode> },
    Or { or: Vec<FilterNode> },
    Not { not: Vec<FilterNode> },
}

/// `[properties."<id>"]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PropertyDef {
    #[serde(rename = "displayName", alias = "display_name", default)]
    pub display_name: Option<String>,
    /// Plan round-8 WP-3: render this `note.<key>` column as a checkbox that toggles the
    /// frontmatter boolean. Ignored on `file.*` / `formula.*` ids.
    #[serde(default)]
    pub checkbox: bool,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum Direction {
    #[default]
    #[serde(alias = "asc")]
    ASC,
    #[serde(alias = "desc")]
    DESC,
}

/// `{property, direction}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortSpec {
    pub property: String,
    #[serde(default)]
    pub direction: Direction,
}

/// View type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewType {
    Table,
    Pivot,
    Tasks,
}

/// One pivot column: assigned to a row when `when` is truthy (first match in list order; a
/// column without `when` catches everything left).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PivotColumn {
    pub label: String,
    #[serde(default)]
    pub when: Option<String>,
}

/// Pivot aggregate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Aggregate {
    #[default]
    Sum,
    Count,
}

/// Task grouping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskGroup {
    #[default]
    Heading,
    File,
    None,
}

/// `[[views]]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewDef {
    #[serde(rename = "type")]
    pub kind: ViewType,
    pub name: String,
    /// Overrides the file's `source` for this view (plan round-11-feedback-todos Decision 5).
    #[serde(default)]
    pub source: Option<Source>,
    #[serde(default)]
    pub filters: Option<FilterNode>,
    #[serde(default)]
    pub order: Vec<String>,
    #[serde(default)]
    pub sort: Vec<SortSpec>,
    #[serde(rename = "groupBy", alias = "group_by", default)]
    pub group_by: Option<SortSpec>,
    /// Property id → `Sum` | `Average` | `Count` | `Min` | `Max`.
    #[serde(default)]
    pub summaries: BTreeMap<String, String>,
    /// Filtered footer rows, shown after the plain `summaries` row (plan round-10 WP-4).
    #[serde(default, alias = "summaryRows")]
    pub summary_rows: Vec<SummaryRowDef>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// Logical px per `order` property id; carried to the editor as `Column::width` (evaluator ignores it).
    #[serde(rename = "columnSize", alias = "column_size", default)]
    pub column_size: BTreeMap<String, i64>,
    // --- pivot ---
    /// Row key expression.
    #[serde(default)]
    pub rows: Option<String>,
    /// Header of the row-key column.
    #[serde(default)]
    pub rows_label: Option<String>,
    #[serde(default)]
    pub rows_direction: Direction,
    #[serde(default)]
    pub columns: Vec<PivotColumn>,
    /// Value expression aggregated per cell.
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub aggregate: Aggregate,
    /// Cell display expression with `value` = the aggregated number (default: the number).
    #[serde(default)]
    pub display: Option<String>,
    /// Label of the per-row total column (omitted when unset).
    #[serde(default)]
    pub total_column: Option<String>,
    /// Label of the Total summary row (omitted when unset).
    #[serde(default)]
    pub total_row: Option<String>,
    /// Label of the average summary row: column total ÷ number of row keys with entries.
    #[serde(default)]
    pub average_row: Option<String>,
    // --- tasks ---
    /// Allowed status chars (`[" "]` = open); empty = any.
    #[serde(default)]
    pub status: Vec<String>,
    /// Drop tasks whose text is blank (default true).
    #[serde(default)]
    pub require_text: Option<bool>,
    /// Per-task predicate with `value` = `{status, text, heading, line}`.
    #[serde(default)]
    pub task_filter: Option<String>,
    #[serde(default)]
    pub task_group: TaskGroup,
    /// Plan round-8 WP-2: height cap of the rendered list (px or `"none"`; absent = 480).
    #[serde(rename = "maxHeight", alias = "max_height", default)]
    pub max_height: Option<crate::result::MaxHeight>,
    /// Plan round-13 WP-4: rows clicked in a `source = "sources"` section embedded in the same
    /// host note filter this view to `<select_by> == <clicked row's name>` (e.g. `file.serial`).
    #[serde(default, alias = "selectBy")]
    pub select_by: Option<String>,
}

/// `[[views.summary_rows]]` (plan round-10 WP-4): an extra footer row computed over the rows
/// matching `filter`, with its own property → aggregate map.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryRowDef {
    pub label: String,
    /// Row predicate, compiled like a view filter expression.
    pub filter: String,
    /// Property id → `Sum` | `Average` | `Count` | `Min` | `Max` (ids normalized like `summaries`).
    #[serde(default)]
    pub summaries: BTreeMap<String, String>,
}

/// Where a definition file's rows come from (plan round-5-reading-pickers-intake WP-10).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Markdown files in the index (`file`/`note`/`formula` bound per row).
    #[default]
    Index,
    /// `state.db` `tracker_items` rows (fields bound as bare identifiers and on `note`).
    TrackerItems,
    /// The daemon-supplied `sources.toml` rows (plan round-11-feedback-todos Decision 5): every
    /// `[[serial]]` row plus every other row with `works = true`, with per-row daily-note counts
    /// (fields bound as bare identifiers and on `note`; see `ViewHost::set_sources`).
    Sources,
    /// `state.db` `trash_entries` rows (plan tidiness-trash-orphans WP-3): the daemon's trash
    /// ledger, read-only. Fields bind as bare identifiers: `original_path`, `name`, `location`,
    /// `reason`, `trashed`, `expires` (null when retention is off), `bytes`, `size_mib`.
    Trash,
}

/// One definition file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseDef {
    #[serde(default)]
    pub source: Source,
    #[serde(default)]
    pub filters: Option<FilterNode>,
    #[serde(default)]
    pub formulas: BTreeMap<String, String>,
    #[serde(default)]
    pub properties: BTreeMap<String, PropertyDef>,
    #[serde(default)]
    pub views: Vec<ViewDef>,
}

/// A compiled filter tree.
#[derive(Debug, Clone)]
pub enum Filter {
    Expr(Expr),
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Vec<Filter>),
}

fn compile_filter(n: &FilterNode, errs: &mut Vec<String>, what: &str) -> Filter {
    let list = |v: &[FilterNode], errs: &mut Vec<String>| {
        v.iter()
            .map(|x| compile_filter(x, errs, what))
            .collect::<Vec<_>>()
    };
    match n {
        FilterNode::Expr(s) => match expr::parse(s) {
            Ok(e) => Filter::Expr(e),
            Err(e) => {
                errs.push(format!("{what}: {e} in `{s}`"));
                Filter::Expr(Expr::Lit(crate::value::Value::Bool(false)))
            }
        },
        FilterNode::And { and } => Filter::And(list(and, errs)),
        FilterNode::Or { or } => Filter::Or(list(or, errs)),
        FilterNode::Not { not } => Filter::Not(list(not, errs)),
    }
}

fn compile_opt(s: Option<&String>, errs: &mut Vec<String>, what: &str) -> Option<Expr> {
    let s = s?;
    match expr::parse(s) {
        Ok(e) => Some(e),
        Err(e) => {
            errs.push(format!("{what}: {e} in `{s}`"));
            None
        }
    }
}

/// A summary function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SumFn {
    Sum,
    Average,
    Count,
    Min,
    Max,
}

impl SumFn {
    /// Every aggregate, in display order (the editor's summary picker lists their
    /// [`SumFn::label`]s).
    pub const ALL: [Self; 5] = [Self::Sum, Self::Average, Self::Count, Self::Min, Self::Max];

    fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "sum" => Self::Sum,
            "average" | "avg" => Self::Average,
            "count" => Self::Count,
            "min" => Self::Min,
            "max" => Self::Max,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Sum => "Sum",
            Self::Average => "Average",
            Self::Count => "Count",
            Self::Min => "Min",
            Self::Max => "Max",
        }
    }
}

/// A compiled `[[views.summary_rows]]` entry.
#[derive(Debug, Clone)]
pub struct SummaryRow {
    pub label: String,
    pub filter: Expr,
    pub summaries: Vec<(String, SumFn)>,
}

/// A compiled pivot.
#[derive(Debug, Clone)]
pub struct Pivot {
    pub rows: Expr,
    pub rows_label: String,
    pub rows_direction: Direction,
    pub columns: Vec<(String, Option<Expr>)>,
    pub value: Expr,
    pub aggregate: Aggregate,
    pub display: Option<Expr>,
    pub total_column: Option<String>,
    pub total_row: Option<String>,
    pub average_row: Option<String>,
}

/// A compiled task query.
#[derive(Debug, Clone)]
pub struct Tasks {
    pub status: Vec<String>,
    pub require_text: bool,
    pub task_filter: Option<Expr>,
    pub group: TaskGroup,
}

/// A compiled view.
#[derive(Debug, Clone)]
pub struct View {
    pub name: String,
    pub kind: ViewType,
    /// Effective row source: the view's `source`, else its file's.
    pub source: Source,
    /// Base-level and view-level filters (all must hold).
    pub filters: Vec<Filter>,
    /// `(property id, expression, label)`.
    pub order: Vec<(String, Expr, String)>,
    pub sort: Vec<(Expr, Direction)>,
    pub group: Option<(Expr, Direction)>,
    pub summaries: Vec<(String, SumFn)>,
    /// Filtered footer rows (plan round-10 WP-4).
    pub summary_rows: Vec<SummaryRow>,
    pub limit: Option<usize>,
    pub pivot: Option<Pivot>,
    pub tasks: Option<Tasks>,
    /// Definition errors: shown as notices; a filter error empties the view.
    pub errors: Vec<String>,
    pub filter_error: bool,
    /// Plan round-8 WP-2: `maxHeight` (`None` = the editor default).
    pub max_height: Option<crate::result::MaxHeight>,
    /// Plan round-8 WP-3: normalized `note.<key>` ids flagged `checkbox = true`.
    pub checkboxes: std::collections::BTreeSet<String>,
    /// Plan intake-round-2 WP-4: `columnSize` (positive px only) keyed by the `order` id as
    /// written (the definition's keys are matched through the same `note.` normalization).
    pub column_widths: BTreeMap<String, f32>,
    /// Plan round-13 WP-4: the definition's `select_by` property (carried to the editor).
    pub select_by: Option<String>,
}

impl View {
    /// The frontmatter key (`note.` dropped) the checkbox column `id` (as written in `order`)
    /// toggles; `None` when `id` is not a frontmatter checkbox.
    #[must_use]
    pub fn checkbox_key(&self, id: &str) -> Option<String> {
        let n = norm(id);
        self.checkboxes
            .contains(&n)
            .then(|| n.strip_prefix("note.").unwrap_or(&n).to_owned())
    }
}

/// A compiled definition file.
#[derive(Debug, Clone)]
pub struct Base {
    pub stem: String,
    pub source: Source,
    pub formulas: BTreeMap<String, Result<Expr, String>>,
    pub views: Vec<View>,
    /// Some filter or formula text mentions `file.serial` (the daemon-supplied serial match
    /// set; `ViewHost::needs_sources`). A textual check: a false positive only costs a recompute.
    pub uses_serial: bool,
}

fn default_label(id: &str) -> String {
    match id.split_once('.') {
        Some(("file", "name")) => "file name".into(),
        Some(("file" | "note" | "formula", rest)) => rest.to_string(),
        _ => id.to_string(),
    }
}

/// `note["k"]` / `note['k']` / `formula["k"]` → `note.k` / `formula.k`; anything else is `None`.
fn unbracket(id: &str) -> Option<String> {
    for prefix in ["note", "formula"] {
        let Some(inner) = id
            .strip_prefix(prefix)
            .and_then(|r| r.strip_prefix('['))
            .and_then(|r| r.strip_suffix(']'))
        else {
            continue;
        };
        let key = ['"', '\'']
            .iter()
            .find_map(|q| inner.strip_prefix(*q)?.strip_suffix(*q))?;
        return Some(format!("{prefix}.{key}"));
    }
    None
}

/// Normalizes a property id for display-name lookup (`word-count` ≡ `note.word-count`;
/// `note["word-count"]` ≡ `note.word-count`). Re-exported as [`crate::canonical_property_id`]:
/// the canonical dotted form the editor's property-id picker offers.
pub fn norm(id: &str) -> String {
    if let Some(dotted) = unbracket(id) {
        return dotted;
    }
    match id.split_once('.') {
        Some(("file" | "note" | "formula", _)) => id.to_string(),
        _ => format!("note.{id}"),
    }
}

impl Base {
    /// Compiles `def` (from file stem `stem`); never fails — problems become view notices.
    pub fn compile(stem: &str, def: &BaseDef, load_notices: &mut Vec<String>) -> Self {
        let formulas: BTreeMap<String, Result<Expr, String>> = def
            .formulas
            .iter()
            .map(|(k, s)| {
                (
                    k.clone(),
                    expr::parse(s).map_err(|e| format!("formula `{k}`: {e}")),
                )
            })
            .collect();
        let formula_errs: Vec<String> = formulas
            .values()
            .filter_map(|r| r.as_ref().err().cloned())
            .collect();
        let labels: BTreeMap<String, String> = def
            .properties
            .iter()
            .filter_map(|(k, p)| p.display_name.clone().map(|d| (norm(k), d)))
            .collect();
        let checkboxes: std::collections::BTreeSet<String> = def
            .properties
            .iter()
            .filter(|(_, p)| p.checkbox)
            .map(|(k, _)| norm(k))
            .filter(|k| k.starts_with("note."))
            .collect();
        let mut views: Vec<View> = Vec::new();
        for v in &def.views {
            if views.iter().any(|x| x.name == v.name) {
                load_notices.push(format!("{stem}: duplicate view name `{}` skipped", v.name));
                continue;
            }
            let mut view = compile_view(v, def, &formula_errs, &labels);
            view.checkboxes.clone_from(&checkboxes);
            views.push(view);
        }
        Self {
            stem: stem.to_string(),
            source: def.source,
            formulas,
            views,
            uses_serial: format!("{def:?}").contains("file.serial"),
        }
    }

    pub fn id(&self, v: &View) -> String {
        format!("{}#{}", self.stem, v.name)
    }
}

fn compile_view(
    v: &ViewDef,
    def: &BaseDef,
    formula_errs: &[String],
    labels: &BTreeMap<String, String>,
) -> View {
    let mut errors: Vec<String> = formula_errs.to_vec();
    let mut ferrs = Vec::new();
    let mut filters = Vec::new();
    if let Some(f) = &def.filters {
        filters.push(compile_filter(f, &mut ferrs, "filters"));
    }
    if let Some(f) = &v.filters {
        filters.push(compile_filter(f, &mut ferrs, "view filters"));
    }
    let filter_error = !ferrs.is_empty();
    errors.extend(ferrs);
    let label = |id: &str| {
        labels
            .get(&norm(id))
            .cloned()
            .unwrap_or_else(|| default_label(id))
    };
    let order = v
        .order
        .iter()
        .map(|id| (id.clone(), expr::property(id), label(id)))
        .collect();
    let sizes: BTreeMap<String, i64> = v.column_size.iter().map(|(k, w)| (norm(k), *w)).collect();
    #[allow(clippy::cast_precision_loss)]
    let column_widths = v
        .order
        .iter()
        .filter_map(|id| {
            let w = *sizes.get(&norm(id))?;
            (w > 0).then(|| (id.clone(), w as f32))
        })
        .collect();
    let sort = v
        .sort
        .iter()
        .map(|s| (expr::property(&s.property), s.direction))
        .collect();
    let group = v
        .group_by
        .as_ref()
        .map(|s| (expr::property(&s.property), s.direction));
    let mut summaries = Vec::new();
    for (k, f) in &v.summaries {
        match SumFn::parse(f) {
            Some(s) => summaries.push((k.clone(), s)),
            None => errors.push(format!("summaries: unknown summary `{f}` for `{k}`")),
        }
    }
    let mut summary_rows = Vec::new();
    for r in &v.summary_rows {
        let Some(filter) = compile_opt(Some(&r.filter), &mut errors, "summary_rows.filter") else {
            continue;
        };
        let mut sums = Vec::new();
        for (k, f) in &r.summaries {
            match SumFn::parse(f) {
                Some(s) => sums.push((k.clone(), s)),
                None => errors.push(format!(
                    "summary_rows `{}`: unknown summary `{f}` for `{k}`",
                    r.label
                )),
            }
        }
        summary_rows.push(SummaryRow {
            label: r.label.clone(),
            filter,
            summaries: sums,
        });
    }
    let pivot = if v.kind == ViewType::Pivot {
        let rows = compile_opt(v.rows.as_ref(), &mut errors, "rows");
        let value = compile_opt(v.value.as_ref(), &mut errors, "value");
        let display = compile_opt(v.display.as_ref(), &mut errors, "display");
        let mut columns = Vec::new();
        for c in &v.columns {
            let when = compile_opt(c.when.as_ref(), &mut errors, "columns.when");
            if c.when.is_some() && when.is_none() {
                continue;
            }
            columns.push((c.label.clone(), when));
        }
        if v.rows.is_none() || v.value.is_none() || v.columns.is_empty() {
            errors.push("pivot needs `rows`, `columns` and `value`".into());
        }
        rows.zip(value).map(|(rows, value)| Pivot {
            rows,
            rows_label: v.rows_label.clone().unwrap_or_else(|| "key".into()),
            rows_direction: v.rows_direction,
            columns,
            value,
            aggregate: v.aggregate,
            display,
            total_column: v.total_column.clone(),
            total_row: v.total_row.clone(),
            average_row: v.average_row.clone(),
        })
    } else {
        None
    };
    let tasks = (v.kind == ViewType::Tasks).then(|| Tasks {
        status: v.status.clone(),
        require_text: v.require_text.unwrap_or(true),
        task_filter: compile_opt(v.task_filter.as_ref(), &mut errors, "task_filter"),
        group: v.task_group,
    });
    View {
        name: v.name.clone(),
        kind: v.kind,
        source: v.source.unwrap_or(def.source),
        filters,
        order,
        sort,
        group,
        summaries,
        summary_rows,
        limit: v.limit,
        pivot,
        tasks,
        filter_error,
        errors,
        max_height: v.max_height,
        checkboxes: std::collections::BTreeSet::new(),
        column_widths,
        select_by: v.select_by.clone(),
    }
}

/// Loads every `<dir>/views/*.toml` (sorted). A file that fails to read or parse becomes a load
/// notice; the others still load. A missing directory is no views, no notice.
pub fn load_dir(config_dir: &Path) -> (Vec<Base>, Vec<String>) {
    let dir = config_dir.join("views");
    let mut notices = Vec::new();
    let mut paths: Vec<_> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml") && p.is_file())
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), notices),
        Err(e) => {
            notices.push(format!("views dir: {e}"));
            return (Vec::new(), notices);
        }
    };
    paths.sort();
    let mut bases = Vec::new();
    for p in paths {
        let stem = p
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let checked = std::fs::read_to_string(&p)
            .map_err(|e| format!("{stem}.toml: {}", e.to_string().trim()))
            .and_then(|s| check_text(&stem, &s, &mut notices));
        match checked {
            Ok(base) => bases.push(base),
            Err(e) => notices.push(e),
        }
    }
    (bases, notices)
}

/// Parses and compiles one view file's text — the one diagnosis shared by [`load_dir`] (the
/// daemon) and the editor's Settings check.
/// `Err`: the parse error (`"<stem>.toml: …"`; the file yields no base). Compile notices are
/// pushed to `notices`; per-view definition errors stay on the views ([`View::errors`]).
pub fn check_text(stem: &str, text: &str, notices: &mut Vec<String>) -> Result<Base, String> {
    let def = toml::from_str::<BaseDef>(text)
        .map_err(|e| format!("{stem}.toml: {}", e.to_string().trim()))?;
    Ok(Base::compile(stem, &def, notices))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[filters]
and = ['file.inFolder("daily/day")', { or = ["note.a", { not = ["note.b"] }] }]

[formulas]
minutes = '(number(note["word-count"]) / 200).round(1)'
bad = 'nope(1)'

[properties."note.word-count"]
displayName = "Words"

[[views]]
type = "table"
name = "T"
order = ["file.name", "word-count", "formula.minutes"]
sort = [{ property = "formula.minutes", direction = "DESC" }]
groupBy = { property = "note.kind", direction = "ASC" }
summaries = { "note.word-count" = "Sum", "formula.minutes" = "Bogus" }
columnSize = { "file.name" = 120 }

[[views]]
type = "pivot"
name = "P"
rows = "note.created"
columns = [{ label = "a", when = "file.hasTag(\"a\")" }, { label = "other" }]
value = "number(note.n)"
total_row = "Total"
average_row = "Average/day"

[[views]]
type = "tasks"
name = "K"
status = [" "]
"#;

    fn max_height_of(line: &str) -> Result<Option<crate::result::MaxHeight>, toml::de::Error> {
        let src = format!("[[views]]\ntype = \"table\"\nname = \"T\"\n{line}\n");
        toml::from_str::<BaseDef>(&src).map(|d| d.views[0].max_height)
    }

    #[test]
    fn max_height_parses_and_rejects() {
        use crate::result::MaxHeight;
        assert_eq!(max_height_of("").unwrap(), None);
        assert_eq!(
            max_height_of("maxHeight = 600").unwrap(),
            Some(MaxHeight::Px(600.0))
        );
        assert_eq!(
            max_height_of("maxHeight = 600.5").unwrap(),
            Some(MaxHeight::Px(600.5))
        );
        assert_eq!(
            max_height_of("max_height = \"none\"").unwrap(),
            Some(MaxHeight::Uncapped)
        );
        for bad in [
            "maxHeight = \"tall\"",
            "maxHeight = 0",
            "maxHeight = -1",
            "maxHeight = true",
        ] {
            let e = max_height_of(bad).unwrap_err().to_string();
            assert!(e.contains("maxHeight"), "{bad}: {e}");
        }
    }

    #[test]
    fn parses_and_compiles() {
        let def: BaseDef = toml::from_str(SAMPLE).unwrap();
        let mut n = Vec::new();
        let b = Base::compile("s", &def, &mut n);
        assert!(n.is_empty());
        assert_eq!(b.views.len(), 3);
        let t = &b.views[0];
        assert_eq!(b.id(t), "s#T");
        assert_eq!(t.order[1].2, "Words");
        assert_eq!(t.order[0].2, "file name");
        assert!(t.errors.iter().any(|e| e.contains("nope")));
        assert!(t.errors.iter().any(|e| e.contains("Bogus")));
        assert!(!t.filter_error);
        assert_eq!(t.summaries.len(), 1);
        assert!(b.views[1].pivot.is_some());
        assert!(b.views[2].tasks.as_ref().is_some_and(|k| k.require_text));
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<BaseDef>("[[views]]\ntype='table'\nname='x'\nbogus=1\n").is_err());
        assert!(toml::from_str::<BaseDef>("[[views]]\ntype='chart'\nname='x'\n").is_err());
        assert!(toml::from_str::<BaseDef>("source='links'\n").is_err());
        let d: BaseDef = toml::from_str("source='tracker_items'\n").unwrap();
        assert_eq!(d.source, Source::TrackerItems);
        let d: BaseDef = toml::from_str("source='trash'\n").unwrap();
        assert_eq!(d.source, Source::Trash);
        let d: BaseDef = toml::from_str("").unwrap();
        assert_eq!(d.source, Source::Index);
        let d: BaseDef =
            toml::from_str("source='sources'\n[[views]]\ntype='table'\nname='x'\nsource='index'\n")
                .unwrap();
        assert_eq!(
            (d.source, d.views[0].source),
            (Source::Sources, Some(Source::Index))
        );
    }
}
