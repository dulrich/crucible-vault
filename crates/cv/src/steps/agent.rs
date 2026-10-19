//! Step module `agent`: one provider completion over a templated prompt (plan `v0-core-daemon`
//! WP-12).
//!
//! Ported from Crucible `https://github.com/dulrich/obsidian-crucible/blob/master/src/agents.ts` (`executeAgent`, `resolvePrompt`,
//! `resolveModel`, `enforceNormalFinishReason`). Kept: system + user prompt from inline text or
//! a vault file, the `providerId:modelId` model ref with a per-run override, the
//! `{{value}}`/`{{input}}` input slot, the `agent_model`/`agent_provider` chain vars, and the
//! "non-normal finish reason is an error" rule. Dropped (Obsidian-induced): internal-command
//! registration (`registerAgents`, `crucible:agent:<id>`), the `Notice` spinner, the
//! model-picker modal and binding modes (an action param replaces them, D3), and the
//! `getActiveFile()` fallback (the runner hands the target note in [`ActionContext`]).
//!
//! **Config** — `agents.toml`, one table per agent id:
//!
//! | key | type | meaning |
//! |---|---|---|
//! | `name` | string? | label (diagnostics only) |
//! | `system` / `system_file` | string? | system prompt text, or a vault path read via [`StepCtx::read`] (at most one) |
//! | `prompt` / `prompt_file` | string? | user prompt template, or a vault path (at most one; neither = `{{value}}`) |
//! | `input` | string? | template rendered into `{{value}}`/`{{input}}` (Crucible's `input` arg); absent = the client's `value` |
//! | `model` | string? | default `provider:model` |
//! | `max_tokens` | integer | **required**, > 0 |
//!
//! Templates substitute `{{content}}` (the target note body, frontmatter excluded — Crucible's
//! refine chains passed the note text, never its YAML), `{{selection}}`, `{{value}}`/`{{input}}`,
//! `{{title}}` (note basename), `{{vars.NAME}}`; unknown placeholders are left as written.
//!
//! **Step keys:** `agent` (required), `model` (`provider:model`, or `{param.NAME}` resolved from
//! the run params — a bare provider id there picks the agent's default model when it is on that
//! provider, else the provider's first chat model), `output_var` (default `agent_output`),
//! `max_tokens` (override), `output = "var" | "replace"` (default `var`).
//!
//! **Dispositions** (Crucible `src/types.ts:83-109`, `service_unhealthy`): transport errors, HTTP
//! 5xx/429/408 and a CLI timeout → [`StepError::Deferred`] for 60 s naming the provider id; a
//! missing secret → `Failed{NoApiKey}`; other 4xx, malformed/unsupported responses and any
//! non-`stop` finish (truncation) → `Failed` — a truncated output is never written.
//!
//! **Idempotence (E2):** `output = "var"` writes nothing, so a re-run after a crash calls the
//! provider again (a second paid call, accepted). `output = "replace"` reads the note's hash
//! *before* the provider call and writes with it (I2: an edit during the slow call is a
//! conflict, never a clobber); a re-run replaces the body again with a fresh completion — the
//! shape converges (body = one completion, never two appended) and the write is skipped when
//! the body already equals the completion.

use std::sync::{Arc, OnceLock};

use cv_actions::{ConfigSet, StepKind};
use cv_providers::{
    Capability, CompletionRequest, FinishReason, ProviderConfig, ProviderError, ProviderManager,
    ProvidersConfig, SystemSecrets, parse_model_ref,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{StepCtx, StepError, StepModule, StepOutcome};

/// Retry delay for an unhealthy provider.
pub(crate) const OUTAGE_RETRY_MS: i64 = 60_000;

/// Default var receiving the completion text.
pub(crate) const DEFAULT_OUTPUT_VAR: &str = "agent_output";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum OutputMode {
    #[default]
    Var,
    Replace,
}

/// Parsed `agent` step params.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StepParams {
    agent: String,
    model: Option<String>,
    output_var: Option<String>,
    max_tokens: Option<u32>,
    #[serde(default)]
    output: OutputMode,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentDef {
    #[allow(dead_code)]
    name: Option<String>,
    system: Option<String>,
    system_file: Option<String>,
    prompt: Option<String>,
    prompt_file: Option<String>,
    input: Option<String>,
    model: Option<String>,
    max_tokens: Option<u32>,
}

/// The model key of a step: a literal ref or a `{param.NAME}` template.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelSpec {
    Literal(String),
    Param(String),
}

fn model_spec(raw: &str) -> Result<ModelSpec, String> {
    let t = raw.trim();
    if let Some(inner) = t.strip_prefix("{param.").and_then(|r| r.strip_suffix('}')) {
        let ok = !inner.is_empty()
            && inner
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        return if ok {
            Ok(ModelSpec::Param(inner.to_owned()))
        } else {
            Err(format!("agent step: bad model template {raw:?}"))
        };
    }
    if t.contains('{') {
        return Err(format!(
            "agent step: model {raw:?} must be `provider:model` or `{{param.NAME}}`"
        ));
    }
    Ok(ModelSpec::Literal(t.to_owned()))
}

fn params(t: &toml::Table) -> Result<StepParams, String> {
    toml::Value::Table(t.clone())
        .try_into::<StepParams>()
        .map_err(|e| format!("agent step: {}", e.message()))
}

fn agent_def(config: &ConfigSet, id: &str) -> Result<AgentDef, String> {
    let v = config
        .table("agents.toml")
        .get(id)
        .ok_or_else(|| format!("agent step: unknown agent {id:?} (agents.toml)"))?;
    let def = v
        .clone()
        .try_into::<AgentDef>()
        .map_err(|e| format!("agents.toml [{id}]: {}", e.message()))?;
    if def.system.is_some() && def.system_file.is_some() {
        return Err(format!(
            "agents.toml [{id}]: set `system` or `system_file`, not both"
        ));
    }
    if def.prompt.is_some() && def.prompt_file.is_some() {
        return Err(format!(
            "agents.toml [{id}]: set `prompt` or `prompt_file`, not both"
        ));
    }
    if def.max_tokens.unwrap_or(0) == 0 {
        return Err(format!(
            "agents.toml [{id}]: `max_tokens` is required and must be > 0"
        ));
    }
    Ok(def)
}

pub(crate) fn providers(config: &ConfigSet) -> Result<ProvidersConfig, String> {
    let text = toml::to_string(config.table("providers.toml"))
        .map_err(|e| format!("providers.toml: {e}"))?;
    ProvidersConfig::from_toml(&text).map_err(|e| e.to_string())
}

/// Resolves and checks a `provider:model` ref against `providers.toml`.
fn check_ref<'p>(
    provs: &'p ProvidersConfig,
    raw: &str,
) -> Result<(&'p ProviderConfig, String), String> {
    let r = parse_model_ref(Some(raw))
        .ok_or_else(|| format!("agent step: model {raw:?} is not `provider:model`"))?;
    let p = provs
        .get(&r.provider_id)
        .ok_or_else(|| format!("agent step: unknown provider {:?}", r.provider_id))?;
    // CLI providers take any model name their tool accepts; HTTP providers must list it.
    if p.kind != cv_providers::ProviderKind::Cli || !p.models.is_empty() {
        let m = p.model(&r.model_id).ok_or_else(|| {
            format!(
                "agent step: provider {:?} has no model {:?}",
                r.provider_id, r.model_id
            )
        })?;
        if !m.has(Capability::Chat) {
            return Err(format!(
                "agent step: model {:?} of provider {:?} cannot chat",
                r.model_id, r.provider_id
            ));
        }
    }
    Ok((p, r.model_id))
}

/// The `agent` step (a parse failure disarms the action, I5), run through the shared
/// [`ProviderManager`].
pub(crate) struct Agent;

impl StepModule for Agent {
    type Params = StepParams;

    fn parse(_: StepKind, t: &toml::Table, config: &ConfigSet) -> Result<StepParams, String> {
        parse(t, config)
    }

    fn run(p: StepParams, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run(&p, ctx, manager()?)
    }
}

/// `agent` outcome vars (the completion text goes to the step's `output_var`).
#[derive(Serialize)]
struct Vars {
    agent_model: String,
    agent_provider: String,
}

/// Parses and checks one step's params against `agents.toml` / `providers.toml`.
pub(crate) fn parse(t: &toml::Table, config: &ConfigSet) -> Result<StepParams, String> {
    let sp = params(t)?;
    let def = agent_def(config, &sp.agent)?;
    if sp.max_tokens == Some(0) {
        return Err("agent step: `max_tokens` must be > 0".into());
    }
    if sp
        .output_var
        .as_deref()
        .is_some_and(|v| v.trim().is_empty())
    {
        return Err("agent step: `output_var` is empty".into());
    }
    let provs = providers(config)?;
    let spec = sp.model.as_deref().map(model_spec).transpose()?;
    match spec {
        Some(ModelSpec::Literal(m)) => {
            check_ref(&provs, &m)?;
        }
        Some(ModelSpec::Param(_)) => {}
        None => {
            let m = def.model.as_deref().ok_or_else(|| {
                format!(
                    "agent step: agent {:?} has no default model and the step sets none",
                    sp.agent
                )
            })?;
            check_ref(&provs, m)?;
        }
    }
    if let Some(m) = &def.model {
        check_ref(&provs, m)?;
    }
    Ok(sp)
}

/// The process-wide manager: completion calls share its per-provider concurrency limiter.
pub(crate) fn manager() -> Result<&'static ProviderManager, StepError> {
    static MANAGER: OnceLock<Result<ProviderManager, String>> = OnceLock::new();
    MANAGER
        .get_or_init(|| {
            let dir = cv_providers::secrets::default_secrets_dir().unwrap_or_default();
            ProviderManager::new(Arc::new(SystemSecrets { dir })).map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| StepError::failed(format!("provider manager: {e}")))
}

/// Pointer identity of the shared manager (for the reuse test).
#[cfg(test)]
pub(crate) fn shared_manager_ptr() -> Result<*const ProviderManager, StepError> {
    manager().map(std::ptr::from_ref)
}

fn var_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Maps a provider failure to the step disposition (Crucible `src/types.ts:83-109`) through
/// the one typed mapping [`StepError::from_failure`] (B8; transient → the 60 s outage defer).
pub(crate) fn provider_failure(provider_id: &str, e: &ProviderError) -> StepError {
    StepError::from_failure(
        provider_id,
        e.failure(),
        OUTAGE_RETRY_MS,
        format!("provider {provider_id:?}: {e}"),
    )
}

/// Resolves the step's model to `(provider, model id)`, checked against `providers.toml`.
fn resolve_model<'p>(
    sp: &StepParams,
    def: &AgentDef,
    provs: &'p ProvidersConfig,
    ctx: &StepCtx<'_>,
) -> Result<(&'p ProviderConfig, String), StepError> {
    let spec = sp
        .model
        .as_deref()
        .map(model_spec)
        .transpose()
        .map_err(StepError::failed)?;
    let raw = match spec {
        Some(ModelSpec::Literal(m)) => m,
        Some(ModelSpec::Param(name)) => {
            if !ctx.action.params.iter().any(|p| p.name == name) {
                return Err(StepError::failed(format!(
                    "agent step: model names undeclared param {name:?}"
                )));
            }
            let v = ctx
                .params
                .get(&name)
                .map(var_text)
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| StepError::failed(format!("agent step: param {name:?} is empty")))?;
            if v.contains(':') {
                v
            } else {
                let pid = v.trim();
                let p = provs.get(pid).ok_or_else(|| {
                    StepError::failed(format!("agent step: unknown provider {pid:?}"))
                })?;
                let default = def
                    .model
                    .as_deref()
                    .and_then(|m| parse_model_ref(Some(m)))
                    .filter(|r| r.provider_id == pid)
                    .map(|r| r.model_id);
                let model = default
                    .or_else(|| {
                        p.models
                            .iter()
                            .find(|m| m.has(Capability::Chat))
                            .map(|m| m.id.clone())
                    })
                    .ok_or_else(|| {
                        StepError::failed(format!("agent step: provider {pid:?} has no chat model"))
                    })?;
                format!("{pid}:{model}")
            }
        }
        None => def
            .model
            .clone()
            .ok_or_else(|| StepError::failed("agent step: no model"))?,
    };
    check_ref(provs, &raw).map_err(StepError::failed)
}

fn load_prompt(
    ctx: &StepCtx<'_>,
    text: Option<&String>,
    file: Option<&String>,
    what: &str,
) -> Result<Option<String>, StepError> {
    if let Some(t) = text {
        return Ok(Some(t.clone()));
    }
    let Some(path) = file else { return Ok(None) };
    let (bytes, _) = ctx.read(path)?.ok_or_else(|| {
        StepError::failed(format!("agent step: {what} prompt file not found: {path}"))
    })?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| StepError::failed(format!("agent step: {what} prompt file is not UTF-8")))
}

/// The step body, with the manager injected (tests pass one with a test `SecretSource`).
pub(crate) fn run(
    sp: &StepParams,
    ctx: &mut StepCtx<'_>,
    mgr: &ProviderManager,
) -> Result<StepOutcome, StepError> {
    let def = agent_def(ctx.config, &sp.agent).map_err(StepError::failed)?;
    let provs = providers(ctx.config).map_err(StepError::failed)?;
    let (provider, model) = resolve_model(sp, &def, &provs, ctx)?;
    let provider = provider.clone();

    // The target note, read (with its CAS hash) before the slow provider call (I2).
    let target = ctx.context.active_path.clone();
    let note = match &target {
        Some(p) => ctx.read(p)?,
        None => None,
    };
    if sp.output == OutputMode::Replace && note.is_none() {
        return Err(StepError::failed(
            "agent step: output = \"replace\" needs an existing target note",
        ));
    }
    let note_text = match &note {
        Some((b, _)) => String::from_utf8(b.clone())
            .map_err(|_| StepError::failed("agent step: target note is not UTF-8"))?,
        None => String::new(),
    };
    let (frontmatter, body) = note_text.split_at(cv_note::span(&note_text).map_or(0, |r| r.end));
    let title = target
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let vars = ctx.vars;
    let context = ctx.context;
    let base = |key: &str| -> Option<String> {
        match key {
            "content" => Some(body.to_owned()),
            "selection" => Some(context.selection.clone().unwrap_or_default()),
            "title" => Some(title.clone()),
            _ => key
                .strip_prefix("vars.")
                .map(|n| vars.get(n).map(var_text).unwrap_or_default()),
        }
    };
    // `{{ key }}` placeholders (key trimmed) through the one cv-text engine; unknown keys stay
    // as written. The lookup never errs, so the engine's error arm is unreachable.
    let render = |t: &str, lookup: &dyn Fn(&str) -> Option<String>| {
        cv_text::template::render(t, |k| Ok(lookup(k.trim()))).unwrap_or_default()
    };
    let value = match &def.input {
        Some(t) => render(t, &|k| match k {
            "value" | "input" => Some(context.value.clone().unwrap_or_default()),
            _ => base(k),
        }),
        None => context.value.clone().unwrap_or_default(),
    };
    let lookup = |k: &str| -> Option<String> {
        match k {
            "value" | "input" => Some(value.clone()),
            _ => base(k),
        }
    };
    let system = load_prompt(ctx, def.system.as_ref(), def.system_file.as_ref(), "system")?
        .map(|t| render(&t, &lookup))
        .unwrap_or_default();
    let user_t = load_prompt(ctx, def.prompt.as_ref(), def.prompt_file.as_ref(), "user")?
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "{{value}}".to_owned());
    let user = render(&user_t, &lookup);
    let max_tokens = sp.max_tokens.or(def.max_tokens);

    let handle =
        tokio::runtime::Handle::try_current().map_err(|_| StepError::failed("no async runtime"))?;
    let req = CompletionRequest {
        system,
        user,
        max_tokens,
        images: Vec::new(),
    };
    let completion = handle
        .block_on(mgr.complete(&provider, &model, req))
        .map_err(|e| provider_failure(&provider.id, &e))?;
    if completion.finish_reason != FinishReason::Stop {
        let raw = completion.raw_finish_reason.as_deref().unwrap_or("missing");
        let what = if completion.finish_reason == FinishReason::Length {
            "was truncated (max_tokens reached)"
        } else {
            "finished abnormally"
        };
        return Err(StepError::failed(format!(
            "agent {:?} on {}:{model} {what}: finish reason {raw:?}; output discarded",
            sp.agent, provider.id
        )));
    }

    let mut out = StepOutcome::default();
    out.set_vars(&Vars {
        agent_model: model.clone(),
        agent_provider: provider.id.clone(),
    });
    match sp.output {
        OutputMode::Var => {
            let name = sp
                .output_var
                .clone()
                .unwrap_or_else(|| DEFAULT_OUTPUT_VAR.to_owned());
            out.vars.insert(name, Value::String(completion.text));
        }
        OutputMode::Replace => {
            let (path, (old, hash)) = match (target, note) {
                (Some(p), Some(n)) => (p, n),
                _ => return Err(StepError::failed("agent step: no target note")),
            };
            let mut text = completion.text;
            if !text.ends_with('\n') {
                text.push('\n');
            }
            let new = format!("{frontmatter}{text}").into_bytes();
            if new != old {
                ctx.write(&path, new, Some(hash))?;
            }
            out.output_paths.push(path);
        }
    }
    Ok(out)
}
