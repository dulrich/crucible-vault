//! Step param helpers shared by every step module (plan tn-review-baseline-remediation WP-R8):
//! the schema-driven allowed-key check, typed key getters and `{…}` placeholder handling.

use cv_actions::StepKind;
use serde_json::Value;

use super::{StepCtx, StepError};

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
