//! Step param helpers shared by every step module:
//! the schema-driven allowed-key check, typed key getters and `{…}` placeholder handling.

use cv_actions::StepKind;
use serde_json::{Map, Value};

use super::{StepCtx, StepError};

/// The `scope = note|backfill` grammar shared by `fanfic_title` and `yt_clip_tidy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Note,
    Backfill,
}

/// An optional `scope` key (`note` when absent): anything but `note`/`backfill` is
/// `"{what} step: `scope` {s:?} is not note|backfill"`.
pub(crate) fn get_scope(t: &toml::Table, what: &str) -> Result<Scope, String> {
    match get_str(t, "scope")?.as_deref() {
        None | Some("note") => Ok(Scope::Note),
        Some("backfill") => Ok(Scope::Backfill),
        Some(s) => Err(format!("{what} step: `scope` {s:?} is not note|backfill")),
    }
}

/// A boolean job param `key`: a bool, or the string `"true"`/`"false"` (any case); anything
/// else (absent, another type, another string) is `None`.
fn flag_of(params: &Map<String, Value>, key: &str) -> Option<bool> {
    match params.get(key) {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("true") => Some(true),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

/// The job's boolean param `key` (see [`flag_of`]). A present job flag wins over the step param
/// (`job_flag(ctx, key).unwrap_or(step_value)`), the one precedence rule (D3).
pub(crate) fn job_flag(ctx: &StepCtx<'_>, key: &str) -> Option<bool> {
    flag_of(ctx.params, key)
}

/// The effective `dry_run`: the job param when present, else the step's value.
pub(crate) fn job_dry_run(ctx: &StepCtx<'_>, step_value: bool) -> bool {
    job_flag(ctx, "dry_run").unwrap_or(step_value)
}

/// A positive-integer job param `key`, the job-side twin of [`get_pos_int`]: a JSON
/// integer > 0 or its decimal text (the action's param kinds have no integer) wins over the step
/// key; absent, null or a blank string is `None`; anything else is rejected, never rounded or
/// coerced. The error reads "job param `key` VALUE is not a positive integer" (callers prefix
/// their step name).
pub(crate) fn job_pos_int(params: &Map<String, Value>, key: &str) -> Result<Option<usize>, String> {
    let bad = |v: &Value| format!("job param `{key}` {v} is not a positive integer");
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v @ Value::Number(n)) => n
            .as_u64()
            .filter(|n| *n > 0)
            .and_then(|n| usize::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| bad(v)),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(v @ Value::String(s)) => s
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .map(Some)
            .ok_or_else(|| bad(v)),
        Some(v) => Err(bad(v)),
    }
}

/// [`job_pos_int`] on the step's job params, a rejection prefixed with the step name
/// (`"{what}: job param …"`) as a failed step.
pub(crate) fn job_pos_int_in(
    ctx: &StepCtx<'_>,
    what: &str,
    key: &str,
) -> Result<Option<usize>, StepError> {
    job_pos_int(ctx.params, key).map_err(|e| StepError::failed(format!("{what}: {e}")))
}

/// Whether `key` is a param of `kind`: the `cv_schema::step_schema(kind)` record's fields are
/// the allowed keys (SM6 — one source for the parser and the Settings form).
pub(crate) fn is_param(kind: StepKind, key: &str) -> bool {
    cv_schema::step_schema(kind.as_str()).is_some_and(|r| r.field(key).is_some())
}

/// The first key of `t` that is not a param of `kind` (fail closed on typos).
pub(crate) fn unknown_key(kind: StepKind, t: &toml::Table) -> Option<&String> {
    t.keys().find(|k| !is_param(kind, k))
}

/// Rejects keys outside `kind`'s schema record: `"{what} step: unknown key …"`.
pub(crate) fn check_keys(t: &toml::Table, kind: StepKind, what: &str) -> Result<(), String> {
    match unknown_key(kind, t) {
        Some(k) => Err(format!("{what} step: unknown key {k:?}")),
        None => Ok(()),
    }
}

/// An optional string key.
pub(crate) fn get_str(t: &toml::Table, key: &str) -> Result<Option<String>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

/// An optional bool key.
pub(crate) fn get_bool(t: &toml::Table, key: &str) -> Result<Option<bool>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(toml::Value::Boolean(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("`{key}` must be a boolean")),
    }
}

/// An optional positive-integer key: absent is `None`; anything but an integer > 0 is
/// `"`{key}` must be an integer > 0"` (callers prefix their step name where they did before).
pub(crate) fn get_pos_int(t: &toml::Table, key: &str) -> Result<Option<usize>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(toml::Value::Integer(n)) if *n > 0 => {
            Ok(Some(usize::try_from(*n).unwrap_or(usize::MAX)))
        }
        Some(_) => Err(format!("`{key}` must be an integer > 0")),
    }
}

/// An optional string-or-list-of-strings key.
pub(crate) fn get_str_list(t: &toml::Table, key: &str) -> Result<Vec<String>, String> {
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

/// String form of a JSON value (`String` verbatim, anything else as JSON text).
pub(crate) fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Checks a single-brace placeholder template's syntax (validation time).
pub(crate) fn check_placeholders(s: &str) -> Result<(), String> {
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
pub(crate) fn expand(ctx: &StepCtx<'_>, s: &str) -> Result<String, StepError> {
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
            let per = super::materialize::period(ctx.config, p, None).map_err(StepError::failed)?;
            let today = super::materialize::now().date();
            per.base_name(today)
                .map(|n| super::materialize::join(&per.folder, &n))?
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
