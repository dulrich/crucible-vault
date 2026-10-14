//! Step module `materialize`: period notes (day/week/month) and plain notes from vault templates,
//! plus the helpers the other WP-10 modules share (placeholder expansion, period settings,
//! template rendering, the idempotent "ensure note" write).
//!
//! Ported from Crucible `src/materialize.ts:37-83` (`materializePeriod`: create a missing note
//! from the period template, fill a zero-byte note, leave a non-empty note untouched),
//! `src/periods.ts:47-104` (per-period folder/format/template/asset-folder settings and the
//! default formats `YYYY-MM-DD`, `GGGG-[W]WW`, `YYYY-MM`) and `docs/materialize-and-templates.md`.
//! Dropped (Obsidian-induced): the `setMaterializing` re-entrancy flag (our writes are attributed
//! to the job and the cycle guard covers echoes, I5), opening a workspace leaf (we return
//! `open_path` to the client instead), `Notice` toasts, and the `enabled` toggle per period (a
//! period without settings simply disarms the actions that use it). The empty-file-only
//! self-guard for externally created notes is kept (`obsidian-crucible/AGENTS.md:280`) as
//! `only_if_empty`.
//!
//! # Period settings
//!
//! Live in `captures.toml`:
//!
//! ```toml
//! [periods.day]
//! folder = "daily/day"                    # required
//! format = "YYYY-MM-DD"                   # Moment format; default per period
//! template = "_custom/day-template.md"    # vault path; optional (no template = empty note)
//! asset_folder = true                     # report `<folder>/<name>` as vars.asset_folder
//! ```
//!
//! A step may override `folder`, `format`, `template` per step.
//!
//! # Step params
//!
//! | key | type | default | meaning |
//! |---|---|---|---|
//! | `period` | `"day"\|"week"\|"month"` (aliases `daily`, `weekly`, `monthly`; placeholders ok) | — | materialize a period note |
//! | `date` | string `YYYY-MM-DD` (placeholders ok, e.g. `"{param.date}"`) | today | any day inside the period |
//! | `path` | string (placeholders ok) | — | plain note to create (`.md` appended if missing) when no `period` |
//! | `template` | string (placeholders ok) | period template | vault path of the template; empty = none |
//! | `folder`, `format` | string | period settings | per-step overrides |
//! | `target` | `"active"` | — | materialize the active/triggering note itself (period inferred from its folder) |
//! | `only_if_empty` | bool | `false` | stop the action (successfully) when the note exists and is non-empty |
//!
//! Placeholders in string params: `{param.NAME}`, `{vars.NAME}`, `{path}` (active path),
//! `{period.day|week|month}` (today's period asset folder, `<folder>/<formatted today>`). A
//! missing value expands to the empty string. Template bodies use Crucible's `{{…}}` tokens
//! (`cv_text::template`) plus `{{vars.NAME}}` / `{{param.NAME}}`.
//!
//! Asset folders: the writer has no "make empty directory" operation, so the asset folder is not
//! created eagerly (a directory appears with its first asset); its path is reported as
//! `vars.asset_folder`. Deviation from Crucible's `ensureFolder`, documented in the WP-10 report.
//!
//! Outputs: `open_path` = the note, `vars.materialized_path`, `output_paths` when written.

use std::collections::BTreeMap;

use chrono::{NaiveDate, NaiveDateTime};
use cv_actions::{ConfigSet, Step};
use cv_text::template::{TemplateContext, apply_template};
use cv_text::{format_moment, parse_moment_date_strict};
use serde_json::Value;

use super::{StepCtx, StepError, StepOutcome};

// ---------------------------------------------------------------------------------------------
// Shared helpers (used by `capture`, `file`, `lint`).
// ---------------------------------------------------------------------------------------------

/// Local wall-clock now.
pub(super) fn now() -> NaiveDateTime {
    chrono::Local::now().naive_local()
}

/// Rejects keys outside `allowed` (fail closed on typos).
pub(super) fn check_keys(t: &toml::Table, allowed: &[&str], what: &str) -> Result<(), String> {
    match t.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!("{what} step: unknown key {k:?}")),
        None => Ok(()),
    }
}

/// An optional string key.
pub(super) fn get_str(t: &toml::Table, key: &str) -> Result<Option<String>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

/// An optional bool key.
pub(super) fn get_bool(t: &toml::Table, key: &str) -> Result<Option<bool>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(toml::Value::Boolean(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("`{key}` must be a boolean")),
    }
}

/// An optional string-or-list-of-strings key.
pub(super) fn get_str_list(t: &toml::Table, key: &str) -> Result<Vec<String>, String> {
    match t.get(key) {
        None => Ok(Vec::new()),
        Some(toml::Value::String(s)) => Ok(vec![s.clone()]),
        Some(toml::Value::Array(a)) => a
            .iter()
            .map(|v| match v {
                toml::Value::String(s) => Ok(s.clone()),
                _ => Err(format!("`{key}` must hold strings")),
            })
            .collect(),
        Some(_) => Err(format!("`{key}` must be a string or a list of strings")),
    }
}

/// The step's params table (empty for none).
pub(super) fn table(step: &Step) -> toml::Table {
    step.table().cloned().unwrap_or_default()
}

/// String form of a JSON value (`String` verbatim, anything else as JSON text).
pub(super) fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Checks a single-brace placeholder template's syntax (validation time).
pub(super) fn check_placeholders(s: &str) -> Result<(), String> {
    let mut rest = s;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let end = after
            .find('}')
            .ok_or_else(|| format!("{s:?}: unclosed `{{`"))?;
        let name = &after[..end];
        let ok = name == "path"
            || name.strip_prefix("param.").is_some_and(|n| !n.is_empty())
            || name.strip_prefix("vars.").is_some_and(|n| !n.is_empty())
            || matches!(name, "period.day" | "period.week" | "period.month");
        if !ok {
            return Err(format!("{s:?}: unknown placeholder {{{name}}}"));
        }
        rest = &after[end + 1..];
    }
    Ok(())
}

/// Expands `{param.NAME}`, `{vars.NAME}`, `{path}`, `{period.P}` in a step string param. A
/// missing value expands to `""`; an unknown placeholder fails.
pub(super) fn expand(ctx: &StepCtx<'_>, s: &str) -> Result<String, StepError> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after
            .find('}')
            .ok_or_else(|| StepError::failed(format!("unclosed `{{` in {s:?}")))?;
        let name = &after[..end];
        let v = if name == "path" {
            ctx.context.active_path.clone().unwrap_or_default()
        } else if let Some(p) = name.strip_prefix("param.") {
            ctx.params.get(p).map(value_text).unwrap_or_default()
        } else if let Some(p) = name.strip_prefix("vars.") {
            ctx.vars.get(p).map(value_text).unwrap_or_default()
        } else if let Some(p) = name.strip_prefix("period.") {
            let per = period(ctx.config, p, None).map_err(StepError::failed)?;
            let today = now().date();
            per.base_name(today).map(|n| join(&per.folder, &n))?
        } else {
            return Err(StepError::failed(format!(
                "unknown placeholder {{{name}}} in {s:?}"
            )));
        };
        out.push_str(&v);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// `folder/name` with no doubled or leading slash.
pub(super) fn join(folder: &str, name: &str) -> String {
    let f = folder.trim_matches('/');
    if f.is_empty() {
        name.to_owned()
    } else {
        format!("{f}/{name}")
    }
}

/// The basename without extension.
pub(super) fn stem(path: &str) -> &str {
    let base = path.rsplit('/').next().unwrap_or(path);
    base.strip_suffix(".md").unwrap_or(base)
}

/// The parent folder ("" at the root).
pub(super) fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

/// Template extras: `vars.NAME`, `param.NAME`, `source_link` (`[[active stem]]`).
pub(super) fn template_extra(ctx: &StepCtx<'_>) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for (k, v) in ctx.vars {
        m.insert(format!("vars.{k}"), value_text(v));
    }
    for (k, v) in ctx.params {
        m.insert(format!("param.{k}"), value_text(v));
    }
    if let Some(p) = ctx.context.active_path.as_deref().filter(|p| !p.is_empty()) {
        m.insert("source_link".into(), format!("[[{}]]", stem(p)));
    }
    m
}

/// Renders a `{{…}}` template.
pub(super) fn render(
    ctx: &StepCtx<'_>,
    template: &str,
    target: NaiveDateTime,
    title: &str,
    value: &str,
) -> Result<String, StepError> {
    let tc = TemplateContext {
        target,
        now: now(),
        title: title.to_owned(),
        value: value.to_owned(),
        extra: template_extra(ctx),
    };
    apply_template(template, &tc).map_err(|e| StepError::failed(e.to_string()))
}

/// Resolved settings of one period.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Period {
    pub name: &'static str,
    pub folder: String,
    pub format: String,
    pub template: Option<String>,
    pub asset_folder: bool,
}

impl Period {
    /// The note basename (without `.md`) for the period containing `date`.
    pub fn base_name(&self, date: NaiveDate) -> Result<String, StepError> {
        format_moment(&date.and_time(chrono::NaiveTime::MIN), &self.format)
            .map_err(|e| StepError::failed(e.to_string()))
    }

    /// The note path for the period containing `date`.
    pub fn note_path(&self, date: NaiveDate) -> Result<String, StepError> {
        Ok(join(&self.folder, &format!("{}.md", self.base_name(date)?)))
    }
}

/// Canonical period name (`day`/`week`/`month`); accepts Crucible's `daily`/`weekly`/`monthly`.
pub(super) fn canonical_period(s: &str) -> Option<&'static str> {
    match s {
        "day" | "daily" => Some("day"),
        "week" | "weekly" => Some("week"),
        "month" | "monthly" => Some("month"),
        _ => None,
    }
}

fn default_format(p: &str) -> &'static str {
    match p {
        "week" => "GGGG-[W]WW",
        "month" => "YYYY-MM",
        _ => "YYYY-MM-DD",
    }
}

/// Period settings from `captures.toml` `[periods.P]`, with optional per-step overrides
/// (`folder`, `format`, `template`).
pub(super) fn period(
    config: &ConfigSet,
    name: &str,
    overrides: Option<&toml::Table>,
) -> Result<Period, String> {
    let p = canonical_period(name).ok_or_else(|| format!("unknown period {name:?}"))?;
    let empty = toml::Table::new();
    let base = config
        .table("captures.toml")
        .get("periods")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get(p))
        .and_then(toml::Value::as_table);
    let ov = overrides.unwrap_or(&empty);
    let ov_folder = get_str(ov, "folder")?;
    if base.is_none() && ov_folder.is_none() {
        return Err(format!(
            "period {p:?} has no settings (captures.toml [periods.{p}])"
        ));
    }
    let base = base.unwrap_or(&empty);
    check_keys(
        base,
        &["folder", "format", "template", "asset_folder"],
        "[periods]",
    )?;
    let folder = match ov_folder {
        Some(f) => f,
        None => get_str(base, "folder")?
            .ok_or_else(|| format!("captures.toml [periods.{p}] needs `folder`"))?,
    };
    let format = match get_str(ov, "format")? {
        Some(f) => f,
        None => get_str(base, "format")?.unwrap_or_else(|| default_format(p).to_owned()),
    };
    cv_text::moment_to_strftime(&format).map_err(|e| format!("period {p:?}: {e}"))?;
    let template = match get_str(ov, "template")? {
        Some(t) => Some(t),
        None => get_str(base, "template")?,
    }
    .filter(|t| !t.trim().is_empty());
    Ok(Period {
        name: p,
        folder: folder.trim_matches('/').to_owned(),
        format,
        template,
        asset_folder: get_bool(base, "asset_folder")?.unwrap_or(false),
    })
}

/// The period (with its date) whose folder holds `path` directly (`daily/day/2026-10-13.md` →
/// day, 2026-10-13). The date is parsed strictly from the basename; a non-parsing name (or a
/// format that does not pin a date, e.g. weeks) uses `fallback`.
pub(super) fn infer_period(
    config: &ConfigSet,
    path: &str,
    fallback: NaiveDate,
) -> Option<(Period, NaiveDate)> {
    let dir = parent(path);
    ["day", "week", "month"].into_iter().find_map(|p| {
        let per = period(config, p, None).ok()?;
        (per.folder == dir).then(|| {
            let d = parse_moment_date_strict(stem(path), &per.format)
                .ok()
                .flatten()
                .unwrap_or(fallback);
            (per, d)
        })
    })
}

/// Parses a `date` param (`YYYY-MM-DD`; empty = today).
pub(super) fn parse_date(s: &str, today: NaiveDate) -> Result<NaiveDate, StepError> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(today);
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| StepError::failed(format!("date {s:?} is not YYYY-MM-DD")))
}

/// What [`ensure_note`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ensured {
    /// The note was missing and was created.
    Created,
    /// The note existed with zero bytes and was filled (CAS on the empty hash).
    Filled,
    /// The note existed and was non-empty: left untouched (idempotence, E2).
    Untouched,
}

/// Ensures `path` exists: create it from `template` (a vault path) when missing, fill it when it
/// is zero bytes, leave it alone otherwise (`src/materialize.ts:52-60`). A configured template
/// that does not exist fails the step (Crucible silently used an empty body; failing closed
/// avoids materializing a note that a later run would never fill).
pub(super) fn ensure_note(
    ctx: &mut StepCtx<'_>,
    path: &str,
    template: Option<&str>,
    date: NaiveDate,
    title: &str,
) -> Result<Ensured, StepError> {
    let existing = ctx.read(path)?;
    if let Some((bytes, _)) = &existing
        && !bytes.is_empty()
    {
        return Ok(Ensured::Untouched);
    }
    let body = match template.filter(|t| !t.trim().is_empty()) {
        None => String::new(),
        Some(t) => {
            let Some((tb, _)) = ctx.read(t)? else {
                return Err(StepError::failed(format!("template {t:?} not found")));
            };
            let text = String::from_utf8(tb)
                .map_err(|_| StepError::failed(format!("template {t:?} is not UTF-8")))?;
            let n = now();
            let target = if date == n.date() {
                n
            } else {
                date.and_time(chrono::NaiveTime::MIN)
            };
            render(ctx, &text, target, title, "")?
        }
    };
    match existing {
        None => {
            ctx.create(path, body.into_bytes())?;
            Ok(Ensured::Created)
        }
        Some((_, hash)) => {
            if body.is_empty() {
                return Ok(Ensured::Untouched);
            }
            ctx.write(path, body.into_bytes(), Some(hash))?;
            Ok(Ensured::Filled)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The step.
// ---------------------------------------------------------------------------------------------

const KEYS: &[&str] = &[
    "period",
    "date",
    "path",
    "template",
    "folder",
    "format",
    "target",
    "only_if_empty",
];

/// Validates one step's params (unknown period / period without settings disarm, I5).
pub fn validate(step: &Step, config: &ConfigSet) -> Result<(), String> {
    let t = table(step);
    check_keys(&t, KEYS, "materialize")?;
    for k in ["date", "path", "template", "folder", "format"] {
        if let Some(s) = get_str(&t, k)? {
            check_placeholders(&s)?;
        }
    }
    get_bool(&t, "only_if_empty")?;
    let target = get_str(&t, "target")?;
    if let Some(tg) = &target
        && tg != "active"
    {
        return Err(format!(
            "materialize: unknown target {tg:?} (only \"active\")"
        ));
    }
    match get_str(&t, "period")? {
        // A run-time period (`"{param.period}"`) is checked when the step runs.
        Some(p) if p.contains('{') => check_placeholders(&p),
        Some(p) => period(config, &p, Some(&t)).map(|_| ()),
        None if target.is_some() => Ok(()),
        None => match get_str(&t, "path")? {
            Some(p) if !p.trim().is_empty() => Ok(()),
            _ => Err("materialize needs `period`, `path` or `target = \"active\"`".into()),
        },
    }
}

/// Executes one materialize step.
pub fn execute(step: &Step, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let t = table(step);
    let s = |k: &str| get_str(&t, k).map_err(StepError::failed);
    let only_if_empty = get_bool(&t, "only_if_empty")
        .map_err(StepError::failed)?
        .unwrap_or(false);
    let today = now().date();
    let date = parse_date(&expand(ctx, &s("date")?.unwrap_or_default())?, today)?;
    let template_override = match s("template")? {
        Some(tp) => Some(expand(ctx, &tp)?),
        None => None,
    };

    let (path, template, date, title, asset) = if s("target")?.is_some() {
        let Some(path) = ctx.context.active_path.clone().filter(|p| !p.is_empty()) else {
            return Err(StepError::failed(
                "materialize target=active needs an active path",
            ));
        };
        let inferred = match s("period")? {
            Some(p) => {
                let p = expand(ctx, &p)?;
                let per = period(ctx.config, &p, Some(&t)).map_err(StepError::failed)?;
                (per.folder == parent(&path)).then(|| {
                    let d = parse_moment_date_strict(stem(&path), &per.format)
                        .ok()
                        .flatten()
                        .unwrap_or(date);
                    (per, d)
                })
            }
            None => infer_period(ctx.config, &path, date),
        };
        let (template, d, asset) = match (&inferred, &template_override) {
            (_, Some(tp)) => (
                Some(tp.clone()),
                inferred.as_ref().map_or(date, |i| i.1),
                None,
            ),
            (Some((per, d)), None) => (
                per.template.clone(),
                *d,
                per.asset_folder.then(|| join(&per.folder, stem(&path))),
            ),
            (None, None) => {
                if only_if_empty {
                    // Not a period note: nothing for this guard-mode run to do.
                    return Ok(StepOutcome {
                        stop: true,
                        ..StepOutcome::default()
                    });
                }
                (None, date, None)
            }
        };
        let title = stem(&path).to_owned();
        (path, template, d, title, asset)
    } else if let Some(p) = s("period")? {
        let p = expand(ctx, &p)?;
        let per = period(ctx.config, &p, Some(&t)).map_err(StepError::failed)?;
        let name = per.base_name(date)?;
        let path = join(&per.folder, &format!("{name}.md"));
        let asset = per.asset_folder.then(|| join(&per.folder, &name));
        let template = template_override.or(per.template);
        (path, template, date, name, asset)
    } else {
        let mut path = expand(ctx, &s("path")?.unwrap_or_default())?;
        path = path.trim().trim_matches('/').to_owned();
        if path.is_empty() {
            return Err(StepError::failed("materialize: empty `path`"));
        }
        if !path.rsplit('/').next().is_some_and(|b| b.contains('.')) {
            path.push_str(".md");
        }
        let title = stem(&path).to_owned();
        (path, template_override, date, title, None)
    };

    if only_if_empty
        && let Some((bytes, _)) = ctx.read(&path)?
        && !bytes.is_empty()
    {
        return Ok(StepOutcome {
            stop: true,
            ..StepOutcome::default()
        });
    }
    let done = ensure_note(ctx, &path, template.as_deref(), date, &title)?;
    let mut out = StepOutcome {
        open_path: Some(path.clone()),
        ..StepOutcome::default()
    };
    out.vars
        .insert("materialized_path".into(), Value::String(path.clone()));
    if let Some(a) = asset {
        out.vars.insert("asset_folder".into(), Value::String(a));
    }
    if done != Ensured::Untouched {
        out.output_paths.push(path);
    }
    Ok(out)
}
