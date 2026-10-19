//! Step module `lint`: run `cv_lint::lint_note` on one note and write only when it changed.
//!
//! Ported from Crucible's `obsidian-crucible:lint-note` command (`src/lint.ts`, settings
//! `lintFrontmatterInsert`, `lintCreatedKey`, `lintModifiedKey`, `lintBlankLineAfterYaml`,
//! `lintYamlKeyPriority`, `lintStepEnabled`) and `lint-cleanup-transcript` (the "Ingest as Post"
//! chain's transcript cleanup, `cv_lint::cleanup_youtube_transcript`). Dropped
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
//! Defaults are `cv_lint::LintSettings::default()` (Crucible's defaults).

use cv_actions::{ConfigSet, StepKind};
use cv_lint::{LintContext, LintSettings, cleanup_youtube_transcript, lint_note};

use super::file::{step_kind, target_path};
use cv_text::vpath::stem;

use super::materialize::now;
use super::params::{check_keys, check_placeholders, get_bool, get_str, get_str_list};
use super::{StepCtx, StepError, StepModule, StepOutcome};

fn settings(t: &toml::Table) -> Result<LintSettings, String> {
    let mut s = LintSettings::default();
    if let Some(v) = get_str(t, "frontmatter_insert")? {
        s.frontmatter_insert = v;
    }
    if let Some(v) = get_str(t, "created_key")? {
        s.created_key = v;
    }
    if let Some(v) = get_str(t, "modified_key")? {
        s.modified_key = v;
    }
    if let Some(v) = get_bool(t, "blank_line_after_yaml")? {
        s.blank_line_after_yaml = v;
    }
    s.yaml_key_priority = get_str_list(t, "yaml_key_priority")?;
    match t.get("steps") {
        None => {}
        Some(toml::Value::Table(m)) => {
            for (k, v) in m {
                let b = v
                    .as_bool()
                    .ok_or_else(|| format!("lint `steps.{k}` must be a boolean"))?;
                if !cv_lint::LINT_STEPS.iter().any(|st| st.id == k) {
                    return Err(format!("lint: unknown step id {k:?}"));
                }
                s.step_enabled.insert(k.clone(), b);
            }
        }
        Some(_) => return Err("lint `steps` must be a table".into()),
    }
    Ok(s)
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
        cleanup |= get_bool(def, "cleanup_transcript")
            .map_err(StepError::failed)?
            .unwrap_or(false);
    }
    let path = target_path(ctx, p.path.as_deref())?;
    let (bytes, hash) = ctx
        .read(&path)?
        .ok_or_else(|| StepError::failed("lint target note not found"))?;
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
