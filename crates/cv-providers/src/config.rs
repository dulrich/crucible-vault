//! Typed `providers.toml` (D18) configuration.
//!
//! Shape carried from Crucible's `Provider` settings type (`https://github.com/dulrich/obsidian-crucible/blob/master/src/types.ts:434`,
//! `maxConcurrentRequests`) and `src/providers/AGENTS.md:15` (LM Studio / llama.cpp are the
//! `openai-compatible` kind, never `ollama`). Dropped (Obsidian-induced / inventory #23): the
//! per-model capability matrix UI and catalog cache; Crucible's separate `openai`/`openrouter`
//! kinds collapse into `openai-compatible` with a `base_url`. Secrets are referenced by name
//! only — never inline in config, never read from the vault (D4).
//!
//! ```toml
//! [[provider]]
//! id = "local"
//! kind = "openai-compatible"
//! base_url = "http://127.0.0.1:1234/v1"
//! models = [{ id = "gemma-4" }, { id = "bge-m3", capabilities = ["embed"] }]
//! ```

use std::collections::HashSet;

use serde::Deserialize;

use crate::error::{ProviderError, Result};

/// Default completion cap when neither the request nor the provider sets one
/// (Crucible `anthropic.ts:32`/`google.ts:27` used 4096). `src/providers/AGENTS.md:18`.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;
/// Crucible `cli.ts:34` `CLI_DEFAULT_TIMEOUT_SECONDS`.
pub const CLI_DEFAULT_TIMEOUT_SECONDS: u64 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    OpenaiCompatible,
    Anthropic,
    Google,
    Ollama,
    Cli,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiCompatible => "openai-compatible",
            Self::Anthropic => "anthropic",
            Self::Google => "google",
            Self::Ollama => "ollama",
            Self::Cli => "cli",
        }
    }
}

/// What a model can do. Embedding and rerank are distinct (`src/providers/AGENTS.md:17`): a
/// reranker served through `/embeddings` passes every structural guard, so the capability is
/// declared, never inferred from the endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    Chat,
    Embed,
    Rerank,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    pub id: String,
    /// Display label for pickers; wins over the server's live name for this declared model.
    pub label: Option<String>,
    /// Absent means chat-only (Crucible's legacy default, `providerRefs.test.mjs:182`); an
    /// explicit empty list means no capability.
    pub capabilities: Option<Vec<Capability>>,
}

impl ModelConfig {
    pub fn has(&self, c: Capability) -> bool {
        match &self.capabilities {
            None => c == Capability::Chat,
            Some(list) => list.contains(&c),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub id: String,
    pub kind: ProviderKind,
    pub name: Option<String>,
    /// API base. Defaults per kind (see `crate::http`).
    pub base_url: Option<String>,
    /// Env var holding the API key.
    pub api_key_env: Option<String>,
    /// File name (no path separators) under the secrets dir holding the API key.
    pub api_key_file: Option<String>,
    /// `anthropic` only: header name carrying the key instead of `x-api-key` (e.g. the inference
    /// router's `x-gateway-key`). Absent = `x-api-key`.
    pub auth_header: Option<String>,
    /// Explicit completion concurrency cap; `0`/absent falls back to the default.
    pub max_concurrent_requests: Option<u32>,
    /// Overrides the local-default heuristic for the concurrency limit.
    pub local: Option<bool>,
    /// Provider-level completion cap (request value wins, then this, then 4096).
    pub max_tokens: Option<u32>,
    /// CLI: the executable (e.g. `codex`).
    pub command: Option<String>,
    /// CLI: args prepended before the generated invocation.
    #[serde(default)]
    pub args: Vec<String>,
    /// CLI: extra args appended after the model flag (Crucible `extraArgs`).
    #[serde(default)]
    pub extra_args: Vec<String>,
    pub cwd: Option<String>,
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub models: Vec<ModelConfig>,
}

impl ProviderConfig {
    pub fn model(&self, id: &str) -> Option<&ModelConfig> {
        self.models.iter().find(|m| m.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvidersConfig {
    #[serde(default, rename = "provider")]
    pub providers: Vec<ProviderConfig>,
}

impl ProvidersConfig {
    /// Parses and validates `providers.toml` text.
    pub fn from_toml(text: &str) -> Result<Self> {
        let cfg: Self = toml::from_str(text).map_err(|e| ProviderError::Config(e.to_string()))?;
        let mut seen = HashSet::new();
        for p in &cfg.providers {
            if p.id.trim().is_empty() || p.id.contains(':') {
                return Err(ProviderError::Config(format!(
                    "provider id {:?} must be non-empty and contain no ':'",
                    p.id
                )));
            }
            if !seen.insert(p.id.as_str()) {
                return Err(ProviderError::Config(format!(
                    "duplicate provider id {:?}",
                    p.id
                )));
            }
            if p.kind == ProviderKind::Cli && p.command.as_deref().is_none_or(str::is_empty) {
                return Err(ProviderError::Config(format!(
                    "cli provider {:?} needs `command`",
                    p.id
                )));
            }
            if let Some(h) = &p.auth_header {
                if p.kind != ProviderKind::Anthropic {
                    return Err(ProviderError::Config(format!(
                        "provider {:?}: `auth_header` is only supported on kind \"anthropic\"",
                        p.id
                    )));
                }
                if h.is_empty() || !h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                    return Err(ProviderError::Config(format!(
                        "provider {:?}: `auth_header` must be a non-empty header name",
                        p.id
                    )));
                }
            }
        }
        Ok(cfg)
    }

    pub fn get(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_model_label() {
        let cfg = ProvidersConfig::from_toml(
            r#"
[[provider]]
id = "local"
kind = "openai-compatible"
models = [{ id = "gemma-4", label = "Gemma 4" }, { id = "bge", capabilities = ["embed"] }]
"#,
        )
        .unwrap();
        let local = cfg.get("local").unwrap();
        assert_eq!(
            local.model("gemma-4").unwrap().label.as_deref(),
            Some("Gemma 4")
        );
        assert_eq!(local.model("bge").unwrap().label, None);
        let typo = "[[provider]]\nid = \"x\"\nkind = \"ollama\"\nmodels = [{ id = \"m\", lable = \"t\" }]\n";
        assert!(ProvidersConfig::from_toml(typo).is_err());
    }

    #[test]
    fn parses_typed_providers() {
        let cfg = ProvidersConfig::from_toml(
            r#"
[[provider]]
id = "local"
kind = "openai-compatible"
base_url = "http://127.0.0.1:1234/v1"
models = [{ id = "gemma-4" }, { id = "bge", capabilities = ["embed"] }, { id = "none", capabilities = [] }]

[[provider]]
id = "claude"
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
max_concurrent_requests = 2
auth_header = "x-gateway-key"

[[provider]]
id = "codex"
kind = "cli"
command = "codex"
"#,
        )
        .unwrap();
        assert_eq!(cfg.providers.len(), 3);
        let local = cfg.get("local").unwrap();
        assert_eq!(local.kind, ProviderKind::OpenaiCompatible);
        // providerRefs.test.mjs:182-195: undefined capabilities = chat; [] = nothing.
        assert!(local.model("gemma-4").unwrap().has(Capability::Chat));
        assert!(!local.model("bge").unwrap().has(Capability::Chat));
        assert!(local.model("bge").unwrap().has(Capability::Embed));
        assert!(!local.model("bge").unwrap().has(Capability::Rerank));
        assert!(!local.model("none").unwrap().has(Capability::Chat));
        assert_eq!(cfg.get("claude").unwrap().max_concurrent_requests, Some(2));
        assert_eq!(
            cfg.get("claude").unwrap().auth_header.as_deref(),
            Some("x-gateway-key")
        );
        assert_eq!(local.auth_header, None);
    }

    #[test]
    fn rejects_bad_configs() {
        for bad in [
            "[[provider]]\nid = \"a\"\nkind = \"bogus\"",
            "[[provider]]\nid = \"a\"\nkind = \"ollama\"\napi_key = \"inline\"",
            "[[provider]]\nid = \"a\"\nkind = \"ollama\"\n[[provider]]\nid = \"a\"\nkind = \"google\"",
            "[[provider]]\nid = \"a:b\"\nkind = \"ollama\"",
            "[[provider]]\nid = \"c\"\nkind = \"cli\"",
            "[[provider]]\nid = \"a\"\nkind = \"anthropic\"\nauth_header = \"\"",
            "[[provider]]\nid = \"a\"\nkind = \"anthropic\"\nauth_header = \"x gateway\"",
            "[[provider]]\nid = \"a\"\nkind = \"openai-compatible\"\nauth_header = \"x-gateway-key\"",
        ] {
            assert!(
                matches!(
                    ProvidersConfig::from_toml(bad),
                    Err(ProviderError::Config(_))
                ),
                "{bad}"
            );
        }
    }
}
