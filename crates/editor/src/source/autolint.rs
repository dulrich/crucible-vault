//! Auto-lint at autosave (plan round-9-feedback-todos WP-6, decision D26): when the 1 s autosave
//! fires for a note the user edited, the frontmatter is linted in the buffer first (one
//! frontmatter-only edit joined into the user's last undo group), so the save carries it and the
//! daemon never rewrites an open buffer behind the editor's back. The pure decisions live in
//! `editor_model::autolint`; the lint settings are the vault `lint-note` action's step params
//! (parsed by the shared `cv_note` parser through [`editor_model::autolint::ParamValue`]).

use std::time::Instant;

use editor_model::autolint::{self, LintContext, LintSettings, ParamValue};
use editor_model::undo::Change;

use super::SourceBuffer;
use super::buffer::{Repr, to_iced, to_model};
use super::edit::apply_change;

/// Everything one auto-lint pass needs besides the buffer.
#[derive(Debug, Clone)]
pub struct AutoLint {
    /// The `auto_lint` pref.
    pub enabled: bool,
    pub settings: LintSettings,
    /// `captures.toml` template paths (their folders are exempt).
    pub templates: Vec<String>,
    /// Today, `YYYY-MM-DD`.
    pub today: String,
}

fn param_value(v: &toml::Value) -> ParamValue {
    match v {
        toml::Value::String(s) => ParamValue::Str(s.clone()),
        toml::Value::Boolean(b) => ParamValue::Bool(*b),
        toml::Value::Array(a) => ParamValue::List(a.iter().map(param_value).collect()),
        toml::Value::Table(m) => {
            ParamValue::Table(m.iter().map(|(k, v)| (k.clone(), param_value(v))).collect())
        }
        _ => ParamValue::Other,
    }
}

/// The lint settings of the `lint-note` action's first `lint` step in an `actions.toml` body.
/// `None` text, no such action or no lint step = the defaults (as the daemon's step with no
/// params); `Err` for an unparseable file or malformed params (auto-lint then skips).
pub fn lint_settings(actions_toml: Option<&str>) -> Result<LintSettings, String> {
    let Some(text) = actions_toml else {
        return Ok(LintSettings::default());
    };
    let doc: toml::Table = toml::from_str(text).map_err(|e| format!("actions.toml: {e}"))?;
    let step = doc
        .get("action")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|a| a.get("id").and_then(toml::Value::as_str) == Some("lint-note"))
        .filter_map(|a| a.get("steps").and_then(toml::Value::as_array))
        .flatten()
        .find(|s| s.get("type").and_then(toml::Value::as_str) == Some("lint"))
        .and_then(toml::Value::as_table);
    match step {
        None => Ok(LintSettings::default()),
        Some(t) => LintSettings::from_params(&|k| t.get(k).map(param_value)),
    }
}

/// The template vault paths in a `captures.toml` body (`periods.<p>.template`).
pub fn template_paths(captures_toml: Option<&str>) -> Vec<String> {
    let Some(doc) = captures_toml.and_then(|t| toml::from_str::<toml::Table>(t).ok()) else {
        return Vec::new();
    };
    ["day", "week", "month"]
        .into_iter()
        .filter_map(|p| {
            doc.get("periods")?
                .get(p)?
                .get("template")?
                .as_str()
                .map(str::to_owned)
        })
        .collect()
}

impl SourceBuffer {
    /// Lints the frontmatter in place when due: `cfg.enabled`, the buffer holds an unsaved user
    /// edit (dirty, no conflict, history non-empty with nothing undone — never on open or
    /// reload, which reset the history), the path is not exempt, and the cursor/selection is
    /// outside the frontmatter. Applies only the frontmatter span as one edit joined into the
    /// user's last undo group, shifting a body cursor by the line delta. Returns whether the
    /// text changed.
    pub fn auto_lint(&mut self, cfg: &AutoLint, now: Instant) -> bool {
        if !cfg.enabled
            || !self.save.dirty
            || self.save.conflict.is_some()
            || !self.history.can_join()
            || autolint::excluded(&self.path, &cfg.templates)
        {
            return false;
        }
        let text = self.text();
        let ctx = LintContext {
            basename: cv_text::vpath::stem(&self.path).to_owned(),
            today: cfg.today.clone(),
            created: cfg.today.clone(),
        };
        let before = to_model(self.cursor());
        let Some(plan) = autolint::plan(&text, before, &ctx, &cfg.settings) else {
            return false;
        };
        if !self.apply_lint_change(&plan.change, plan.after) {
            return false;
        }
        self.history
            .record_joined(plan.change, before, plan.after, now);
        self.mark_edited();
        true
    }

    fn apply_lint_change(&mut self, change: &Change, after: editor_model::undo::Cursor) -> bool {
        match &mut self.content {
            Repr::Shaped(c) => {
                let ok = apply_change(c, change);
                c.move_to(to_iced(after));
                ok
            }
            Repr::Plain { text, cursor } => {
                if change.apply(text).is_err() {
                    return false;
                }
                *cursor = after;
                true
            }
        }
    }
}
