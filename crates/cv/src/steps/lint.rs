//! Step module `lint`: run `cv_note::lint_note` on one note and write only when it changed.
//!
//! Ported from Crucible's `obsidian-crucible:lint-note` command (`src/lint.ts`, settings
//! `lintFrontmatterInsert`, `lintCreatedKey`, `lintModifiedKey`, `lintBlankLineAfterYaml`,
//! `lintYamlKeyPriority`, `lintStepEnabled`) and `lint-cleanup-transcript` (the "Ingest as Post"
//! chain's transcript cleanup, `cv_note::cleanup_youtube_transcript`). Dropped
//! (Obsidian-induced): lint-on-save (an event action can run this step instead), the editor
//! buffer round-trip, and reading `ctime` from Obsidian's `TFile.stat` — the created stamp uses
//! today when the note has none (`created` is only ever set if empty). Not lint-all, no localize.
//!
//! # Step params
//!
//! | key | type | default |
//! |---|---|---|
//! | `path` | string (placeholders) | active note |
//! | `kind` | string (placeholders) | — (a kind with `cleanup_transcript = true` enables cleanup) |
//! | `cleanup_transcript` | bool | `false` |
//! | `frontmatter_insert` | string (`key: value` lines) | `""` |
//! | `created_key` | string | `"created"` |
//! | `modified_key` | string | `"updated"` |
//! | `blank_line_after_yaml` | bool | `false` |
//! | `yaml_key_priority` | list of strings | `[]` |
//! | `steps` | table `{ step-id = bool }` | all enabled |
//!
//! Defaults are `cv_note::LintSettings::default()` (Crucible's defaults).

use cv_actions::{ConfigSet, StepKind};
use cv_note::{LintContext, LintSettings, cleanup_youtube_transcript, lint_note};

use super::file::{kind_cleans_transcript, step_kind, target_path};
use cv_text::vpath::stem;

use super::materialize::now;
use super::params::{check_keys, check_placeholders, get_bool, get_str};
use super::{StepCtx, StepError, StepModule, StepOutcome};

fn param_value(v: &toml::Value) -> cv_note::ParamValue {
    use cv_note::ParamValue;
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

fn settings(t: &toml::Table) -> Result<LintSettings, String> {
    LintSettings::from_params(&|k| t.get(k).map(param_value))
}

/// Parsed `lint` params.
#[derive(Debug, Clone, Default)]
pub(crate) struct Params {
    path: Option<String>,
    kind: Option<String>,
    cleanup_transcript: bool,
    settings: LintSettings,
}

impl Params {
    /// Lints `path` with default settings, as note kind `kind` (inline lint, e.g. the blog
    /// tracker's captured notes).
    pub(crate) fn note(path: String, kind: Option<String>) -> Self {
        Params {
            path: Some(path),
            kind,
            ..Params::default()
        }
    }
}

/// The `lint` step.
pub(crate) struct Lint;

impl StepModule for Lint {
    type Params = Params;

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<Params, String> {
        check_keys(t, kind, "lint")?;
        for k in ["path", "kind"] {
            if let Some(s) = get_str(t, k)? {
                check_placeholders(&s)?;
            }
        }
        Ok(Params {
            path: get_str(t, "path")?,
            kind: get_str(t, "kind")?,
            cleanup_transcript: get_bool(t, "cleanup_transcript")?.unwrap_or(false),
            settings: settings(t)?,
        })
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run(&p, ctx)
    }
}

/// Executes one lint step.
pub(crate) fn run(p: &Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let settings = &p.settings;
    let mut cleanup = p.cleanup_transcript;
    if let Some(def) = step_kind(ctx, p.kind.as_deref())? {
        cleanup |= kind_cleans_transcript(def).map_err(StepError::failed)?;
    }
    let path = target_path(ctx, p.path.as_deref())?;
    let (bytes, hash) = ctx.read_required(&path)?;
    let original =
        String::from_utf8(bytes).map_err(|_| StepError::failed("lint target is not UTF-8"))?;
    let input = if cleanup {
        cleanup_youtube_transcript(&original)
    } else {
        original.clone()
    };
    let today = now().format("%Y-%m-%d").to_string();
    let lc = LintContext {
        basename: stem(&path).to_owned(),
        today: today.clone(),
        created: today,
    };
    let outcome = lint_note(&input, &lc, settings).map_err(|e| StepError::failed(e.to_string()))?;
    let mut out = StepOutcome::default();
    if outcome.text != original {
        ctx.write(&path, outcome.text.into_bytes(), Some(hash))?;
        out.push_output(path);
    }
    Ok(out)
}
