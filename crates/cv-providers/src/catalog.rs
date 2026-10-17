//! On-demand model listing per provider (plan `round-5-reading-pickers-intake` WP-5).
//!
//! Re-adds the catalog `wire.rs` dropped (inventory #23) as a lookup the editor's model picker
//! asks for — never probed at startup. Ports Crucible `https://github.com/dulrich/obsidian-crucible/blob/master/src/providers/
//! openaiCompatible.ts:195-521` (LM Studio's native `/api/v0/models` at the host root first, then
//! `{base}/models` for every OpenAI-compatible server incl. OpenRouter, whose `name` is the
//! display name), `providers/ollama.ts:143` (`/api/tags`, `name ?? model`) and the display label
//! rule of `settings/modelCapabilities.ts:217-230` (server display name, else the basename of a
//! file-path-shaped id minus `.gguf`). Dropped: the per-model `/api/show` capability probe and the
//! quantization/embedding metadata (the picker needs ids only) and the persisted catalog cache
//! (the daemon caches in memory).

use serde::Deserialize;

use crate::config::{ProviderConfig, ProviderKind};
use crate::web::{FetchError, FetchRequest, WebClient, decode_json};
use crate::wire::base_url;

/// Breaker key for catalog fetches.
pub const SERVICE: &str = "model-catalog";

/// One listed model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogModel {
    /// The id to store in a `provider:model` ref.
    pub id: String,
    /// A friendlier label, when one exists.
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DataList {
    #[serde(default)]
    data: Vec<DataEntry>,
}

#[derive(Debug, Deserialize)]
struct DataEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Tags {
    #[serde(default)]
    models: Vec<TagEntry>,
}

#[derive(Debug, Deserialize)]
struct TagEntry {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

/// Crucible `deriveModelDisplayLabel`: the server's name, else a path-shaped id's basename.
fn display_label(id: &str, server: Option<String>) -> Option<String> {
    if let Some(n) = server.filter(|n| !n.trim().is_empty()) {
        return Some(n);
    }
    if !id.contains('/') || !id.to_ascii_lowercase().ends_with(".gguf") {
        return None;
    }
    let base = id.rsplit('/').next().unwrap_or(id);
    Some(base[..base.len() - ".gguf".len()].to_owned()).filter(|b| !b.is_empty())
}

/// Decodes an OpenAI-shaped `{data: [{id, name?}]}` list (`/models`, LM Studio `/api/v0/models`).
pub fn parse_data_list(body: &[u8]) -> Result<Vec<CatalogModel>, FetchError> {
    let l: DataList = decode_json(SERVICE, body)?;
    Ok(l.data
        .into_iter()
        .filter(|e| !e.id.is_empty())
        .map(|e| CatalogModel {
            display_name: display_label(&e.id, e.name),
            id: e.id,
        })
        .collect())
}

/// Decodes an Ollama `/api/tags` body.
pub fn parse_ollama_tags(body: &[u8]) -> Result<Vec<CatalogModel>, FetchError> {
    let t: Tags = decode_json(SERVICE, body)?;
    Ok(t.models
        .into_iter()
        .filter_map(|e| e.name.or(e.model).filter(|s| !s.is_empty()))
        .map(|id| CatalogModel {
            display_name: display_label(&id, None),
            id,
        })
        .collect())
}

/// Scheme + authority of `base` (Crucible `hostRootUrl`: LM Studio's native API lives there).
fn host_root(base: &str) -> Option<String> {
    let u = reqwest::Url::parse(base).ok()?;
    let mut root = format!("{}://{}", u.scheme(), u.host_str()?);
    if let Some(p) = u.port() {
        root.push_str(&format!(":{p}"));
    }
    Some(root)
}

async fn get(client: &WebClient, url: &str, key: Option<&str>) -> Result<Vec<u8>, FetchError> {
    let mut req = FetchRequest::get(SERVICE, url);
    if let Some(k) = key {
        req.headers
            .push(("authorization".to_owned(), format!("Bearer {k}")));
    }
    client.fetch(req).await
}

/// Lists `p`'s models live. `Ok(None)` = the kind has no listing endpoint (anthropic, google,
/// cli); the caller falls back to the declared models. `key` is the resolved API key, if any.
pub async fn list_models(
    client: &WebClient,
    p: &ProviderConfig,
    key: Option<&str>,
) -> Result<Option<Vec<CatalogModel>>, FetchError> {
    let base = base_url(p);
    match p.kind {
        ProviderKind::OpenaiCompatible => {
            if let Some(root) = host_root(&base)
                && let Ok(body) = get(client, &format!("{root}/api/v0/models"), key).await
                && let Ok(list) = parse_data_list(&body)
                && !list.is_empty()
            {
                return Ok(Some(list));
            }
            let body = get(client, &format!("{base}/models"), key).await?;
            parse_data_list(&body).map(Some)
        }
        ProviderKind::Ollama => {
            let body = get(client, &format!("{base}/api/tags"), None).await?;
            parse_ollama_tags(&body).map(Some)
        }
        ProviderKind::Anthropic | ProviderKind::Google | ProviderKind::Cli => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_and_lmstudio_lists() {
        let b = br#"{"object":"list","data":[{"id":"openai/gpt-5","name":"OpenAI: GPT-5"},{"id":"gemma-4","object":"model"},{"id":"/models/x/bge-m3-f16.gguf"},{"id":""}]}"#;
        let m = parse_data_list(b).unwrap();
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].display_name.as_deref(), Some("OpenAI: GPT-5"));
        assert_eq!(m[1].display_name, None);
        assert_eq!(m[2].display_name.as_deref(), Some("bge-m3-f16"));
    }

    #[test]
    fn parses_ollama_tags() {
        let b = br#"{"models":[{"name":"llama3:8b","model":"llama3:8b"},{"model":"qwen3"},{}]}"#;
        let ids: Vec<String> = parse_ollama_tags(b)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, ["llama3:8b", "qwen3"]);
        assert!(parse_ollama_tags(b"nope").is_err());
    }

    #[test]
    fn host_root_keeps_port() {
        assert_eq!(
            host_root("http://127.0.0.1:1234/v1").as_deref(),
            Some("http://127.0.0.1:1234")
        );
        assert_eq!(
            host_root("https://openrouter.ai/api/v1").as_deref(),
            Some("https://openrouter.ai")
        );
    }
}
