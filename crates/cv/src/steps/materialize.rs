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
use cv_actions::{ConfigSet, StepKind};
use cv_text::template::{TemplateContext, apply_template};
use cv_text::vpath::{parent, stem};
use cv_text::{format_moment, parse_moment_date_strict};
use serde::Serialize;

use super::params::{check_keys, check_placeholders, expand, get_bool, get_str, value_text};
use super::{StepCtx, StepError, StepModule, StepOutcome};

// ---------------------------------------------------------------------------------------------
// Shared helpers (used by `capture`, `file`, `lint`).
// ---------------------------------------------------------------------------------------------

/// Local wall-clock now.
pub(super) fn now() -> NaiveDateTime {
    chrono::Local::now().naive_local()
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

/// Per-step period overrides (`folder`, `format`, `template`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Overrides {
    pub folder: Option<String>,
    pub format: Option<String>,
    pub template: Option<String>,
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
    overrides: Option<&Overrides>,
) -> Result<Period, String> {
    let p = canonical_period(name).ok_or_else(|| format!("unknown period {name:?}"))?;
    let empty = toml::Table::new();
    let none = Overrides::default();
    let base = config
        .table("captures.toml")
        .get("periods")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get(p))
        .and_then(toml::Value::as_table);
    let ov = overrides.unwrap_or(&none);
    let ov_folder = ov.folder.clone();
    if base.is_none() && ov_folder.is_none() {
        return Err(format!(
            "period {p:?} has no settings (captures.toml [periods.{p}])"
        ));
    }
    let base = base.unwrap_or(&empty);
    if let Some(k) = base
        .keys()
        .find(|k| !["folder", "format", "template", "asset_folder"].contains(&k.as_str()))
    {
        return Err(format!("[periods] step: unknown key {k:?}"));
    }
    let folder = match ov_folder {
        Some(f) => f,
        None => get_str(base, "folder")?
            .ok_or_else(|| format!("captures.toml [periods.{p}] needs `folder`"))?,
    };
    let format = match ov.format.clone() {
        Some(f) => f,
        None => get_str(base, "format")?.unwrap_or_else(|| default_format(p).to_owned()),
    };
    cv_text::moment_to_strftime(&format).map_err(|e| format!("period {p:?}: {e}"))?;
    let template = match ov.template.clone() {
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

/// Parsed `materialize` params.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Params {
    period: Option<String>,
    date: Option<String>,
    path: Option<String>,
    target_active: bool,
    only_if_empty: bool,
    overrides: Overrides,
}

/// The `materialize` step.
pub(crate) struct Materialize;

impl StepModule for Materialize {
    type Params = Params;

    /// Unknown period / period without settings disarm (I5).
    fn parse(kind: StepKind, t: &toml::Table, config: &ConfigSet) -> Result<Params, String> {
        check_keys(t, kind, "materialize")?;
        for k in ["date", "path", "template", "folder", "format"] {
            if let Some(s) = get_str(t, k)? {
                check_placeholders(&s)?;
            }
        }
        let only_if_empty = get_bool(t, "only_if_empty")?.unwrap_or(false);
        let target = get_str(t, "target")?;
        if let Some(tg) = &target
            && tg != "active"
        {
            return Err(format!(
                "materialize: unknown target {tg:?} (only \"active\")"
            ));
        }
        let p = Params {
            period: get_str(t, "period")?,
            date: get_str(t, "date")?,
            path: get_str(t, "path")?,
            target_active: target.is_some(),
            only_if_empty,
            overrides: Overrides {
                folder: get_str(t, "folder")?,
                format: get_str(t, "format")?,
                template: get_str(t, "template")?,
            },
        };
        match &p.period {
            // A run-time period (`"{param.period}"`) is checked when the step runs.
            Some(per) if per.contains('{') => check_placeholders(per)?,
            Some(per) => period(config, per, Some(&p.overrides)).map(|_| ())?,
            None if p.target_active => {}
            None => match &p.path {
                Some(path) if !path.trim().is_empty() => {}
                _ => {
                    return Err(
                        "materialize needs `period`, `path` or `target = \"active\"`".into(),
                    );
                }
            },
        }
        Ok(p)
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run(&p, ctx)
    }
}

/// Executes one materialize step.
fn run(p: &Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let only_if_empty = p.only_if_empty;
    let today = now().date();
    let date = parse_date(&expand(ctx, p.date.as_deref().unwrap_or_default())?, today)?;
    let template_override = match &p.overrides.template {
        Some(tp) => Some(expand(ctx, tp)?),
        None => None,
    };

    let (path, template, date, title, asset) = if p.target_active {
        let Some(path) = ctx.context.active_path.clone().filter(|p| !p.is_empty()) else {
            return Err(StepError::failed(
                "materialize target=active needs an active path",
            ));
        };
        let inferred = match &p.period {
            Some(pn) => {
                let pn = expand(ctx, pn)?;
                let per = period(ctx.config, &pn, Some(&p.overrides)).map_err(StepError::failed)?;
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
    } else if let Some(pn) = &p.period {
        let pn = expand(ctx, pn)?;
        let per = period(ctx.config, &pn, Some(&p.overrides)).map_err(StepError::failed)?;
        let name = per.base_name(date)?;
        let path = join(&per.folder, &format!("{name}.md"));
        let asset = per.asset_folder.then(|| join(&per.folder, &name));
        let template = template_override.or(per.template);
        (path, template, date, name, asset)
    } else {
        let mut path = expand(ctx, p.path.as_deref().unwrap_or_default())?;
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
    out.set_vars(&Vars {
        materialized_path: path.clone(),
        asset_folder: asset,
    });
    if done != Ensured::Untouched {
        out.push_output(path);
    }
    Ok(out)
}

/// `materialize` outcome vars.
#[derive(Serialize)]
struct Vars {
    materialized_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    asset_folder: Option<String>,
}
