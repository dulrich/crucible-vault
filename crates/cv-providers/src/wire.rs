//! Pure request builders and response parsers for each HTTP provider kind.
//!
//! Carried from Crucible `https://github.com/dulrich/obsidian-crucible/blob/master/src/providers/openaiCompatible.ts:109-229`,
//! `anthropic.ts:12-45`, `google.ts:12-45`, `ollama.ts:36-92` and `shared.ts:137-206`.
//! Changes: `max_tokens` (or the kind's equivalent) is ALWAYS sent (`src/providers/AGENTS.md:18`
//! — Crucible's openai-compatible/ollama `complete()` omitted it); Google's key moves from the
//! `?key=` query string to the `x-goog-api-key` header so it never lands in logged URLs;
//! OpenRouter's attribution headers and the `/api/v0/models` probe/model catalog are dropped
//! (inventory #23). Obsidian's `requestUrl` is replaced by `reqwest` in `crate::manager`.

use serde_json::{Value, json};

use crate::config::{ProviderConfig, ProviderKind};
use crate::error::{ProviderError, Result};

/// A built HTTP request (always POST + JSON).
#[derive(Debug, Clone, PartialEq)]
pub struct WireRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub text: String,
    pub finish_reason: FinishReason,
    pub raw_finish_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Embeddings {
    pub vectors: Vec<Vec<f32>>,
    pub dimensions: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RerankItem {
    pub index: usize,
    pub relevance_score: f64,
}

pub fn base_url(p: &ProviderConfig) -> String {
    let fallback = match p.kind {
        ProviderKind::OpenaiCompatible => "http://localhost:1234/v1",
        ProviderKind::Anthropic => "https://api.anthropic.com",
        ProviderKind::Google => "https://generativelanguage.googleapis.com",
        ProviderKind::Ollama => "http://localhost:11434",
        ProviderKind::Cli => "",
    };
    p.base_url
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .trim_end_matches('/')
        .to_owned()
}

fn json_headers(bearer: Option<&str>) -> Vec<(String, String)> {
    let mut h = vec![("content-type".to_owned(), "application/json".to_owned())];
    // Local servers may run without auth; only send the header when a key is present.
    if let Some(k) = bearer {
        h.push(("authorization".to_owned(), format!("Bearer {k}")));
    }
    h
}

/// One image attached to a completion request (plan `v0-automation-pipelines` WP-6). The caller
/// base64-encodes the bytes; this crate never decodes them, and `Debug` never prints them.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct ImagePart {
    /// MIME type, e.g. `image/png`.
    pub mime: String,
    /// Standard base64 of the image bytes.
    pub base64: String,
}

impl std::fmt::Debug for ImagePart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImagePart")
            .field("mime", &self.mime)
            .field("base64_len", &self.base64.len())
            .finish()
    }
}

/// The messages array of the chat-style kinds. The system message is omitted when images are
/// attached and `system` is empty (Crucible's vision call sends none,
/// `openaiCompatible.ts:277-296`); text-only requests keep their historical shape byte for byte.
fn chat_messages(system: &str, user_msg: Value, has_images: bool) -> Value {
    if has_images && system.is_empty() {
        json!([{"role": "user", "content": user_msg}])
    } else {
        json!([
            {"role": "system", "content": system},
            {"role": "user", "content": user_msg},
        ])
    }
}

/// Optional openai-compatible body fields (round 16 WP-2: Crucible's describe wire,
/// `openaiCompatible.ts:271-296`, plus the measured sampler). Every field unset (the default) adds
/// nothing to the body, so a request that sets none is byte-for-byte the pre-round-16 one. Only
/// the `openai-compatible` kind carries them; other kinds ignore them (their wires have no such
/// fields).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WireExtras {
    /// `reasoning_effort` (e.g. `"none"`).
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u32>,
    pub repeat_penalty: Option<f64>,
    pub repeat_last_n: Option<u32>,
}

/// Adds the set `extras` to an openai-compatible request body; a no-op for every other kind and
/// for unset fields.
pub fn apply_extras(kind: ProviderKind, wire: &mut WireRequest, extras: &WireExtras) {
    if kind != ProviderKind::OpenaiCompatible {
        return;
    }
    let Some(body) = wire.body.as_object_mut() else {
        return;
    };
    if let Some(v) = &extras.reasoning_effort {
        body.insert("reasoning_effort".into(), json!(v));
    }
    if let Some(v) = extras.temperature {
        body.insert("temperature".into(), json!(v));
    }
    if let Some(v) = extras.top_p {
        body.insert("top_p".into(), json!(v));
    }
    if let Some(v) = extras.top_k {
        body.insert("top_k".into(), json!(v));
    }
    if let Some(v) = extras.repeat_penalty {
        body.insert("repeat_penalty".into(), json!(v));
    }
    if let Some(v) = extras.repeat_last_n {
        body.insert("repeat_last_n".into(), json!(v));
    }
}

/// Builds a completion request for an HTTP kind. Empty `images` = the text-only wire shape.
pub fn build_completion(
    p: &ProviderConfig,
    api_key: Option<&str>,
    model: &str,
    system: &str,
    user: &str,
    images: &[ImagePart],
    max_tokens: u32,
) -> Result<WireRequest> {
    let base = base_url(p);
    let has_images = !images.is_empty();
    Ok(match p.kind {
        ProviderKind::OpenaiCompatible => {
            let content = if has_images {
                let mut parts = vec![json!({"type": "text", "text": user})];
                parts.extend(images.iter().map(|i| {
                    json!({
                        "type": "image_url",
                        "image_url": {"url": format!("data:{};base64,{}", i.mime, i.base64)},
                    })
                }));
                Value::Array(parts)
            } else {
                Value::String(user.to_owned())
            };
            WireRequest {
                url: format!("{base}/chat/completions"),
                headers: json_headers(api_key),
                body: json!({
                    "model": model,
                    "messages": chat_messages(system, content, has_images),
                    "max_tokens": max_tokens,
                }),
            }
        }
        ProviderKind::Anthropic => {
            let key = api_key.ok_or_else(|| {
                ProviderError::Secret(format!("provider {:?} needs an API key", p.id))
            })?;
            let content = if has_images {
                let mut blocks: Vec<Value> = images
                    .iter()
                    .map(|i| {
                        json!({
                            "type": "image",
                            "source": {"type": "base64", "media_type": i.mime, "data": i.base64},
                        })
                    })
                    .collect();
                blocks.push(json!({"type": "text", "text": user}));
                Value::Array(blocks)
            } else {
                Value::String(user.to_owned())
            };
            let mut body = json!({
                "model": model,
                "system": system,
                "messages": [{"role": "user", "content": content}],
                "max_tokens": max_tokens,
            });
            if has_images
                && system.is_empty()
                && let Some(o) = body.as_object_mut()
            {
                o.remove("system");
            }
            WireRequest {
                url: format!("{base}/v1/messages"),
                headers: vec![
                    ("content-type".into(), "application/json".into()),
                    (
                        p.auth_header.as_deref().unwrap_or("x-api-key").into(),
                        key.to_owned(),
                    ),
                    ("anthropic-version".into(), "2023-06-01".into()),
                ],
                body,
            }
        }
        ProviderKind::Google => {
            let key = api_key.ok_or_else(|| {
                ProviderError::Secret(format!("provider {:?} needs an API key", p.id))
            })?;
            let mut parts = vec![json!({"text": user})];
            parts.extend(
                images
                    .iter()
                    .map(|i| json!({"inline_data": {"mime_type": i.mime, "data": i.base64}})),
            );
            let mut body = json!({
                "contents": [{"role": "user", "parts": parts}],
                "generationConfig": {"maxOutputTokens": max_tokens},
            });
            if !system.is_empty() {
                body["system_instruction"] = json!({"parts": [{"text": system}]});
            }
            WireRequest {
                url: format!("{base}/v1beta/models/{model}:generateContent"),
                headers: vec![
                    ("content-type".into(), "application/json".into()),
                    ("x-goog-api-key".into(), key.to_owned()),
                ],
                body,
            }
        }
        ProviderKind::Ollama => {
            let mut messages = chat_messages(system, Value::String(user.to_owned()), has_images);
            if has_images && let Some(last) = messages.as_array_mut().and_then(|a| a.last_mut()) {
                last["images"] = images
                    .iter()
                    .map(|i| Value::String(i.base64.clone()))
                    .collect();
            }
            WireRequest {
                url: format!("{base}/api/chat"),
                headers: json_headers(None),
                body: json!({
                    "model": model,
                    "messages": messages,
                    "stream": false,
                    // Ollama's name for max_tokens.
                    "options": {"num_predict": max_tokens},
                }),
            }
        }
        ProviderKind::Cli => {
            return Err(ProviderError::Unsupported {
                kind: "cli",
                capability: "build an HTTP request",
            });
        }
    })
}

fn raw_reason(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn finish(raw: Option<&str>) -> FinishReason {
    match raw {
        Some("stop" | "end_turn" | "STOP" | "stop_sequence") => FinishReason::Stop,
        Some("length" | "max_tokens" | "MAX_TOKENS") => FinishReason::Length,
        _ => FinishReason::Other,
    }
}

/// Parses a completion response for an HTTP kind.
pub fn parse_completion(kind: ProviderKind, v: &Value) -> Result<Completion> {
    let (text, raw) = match kind {
        ProviderKind::OpenaiCompatible => {
            let choice = v
                .get("choices")
                .and_then(|c| c.get(0))
                .ok_or_else(|| ProviderError::Malformed("API returned no choices".into()))?;
            (
                choice["message"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                raw_reason(choice.get("finish_reason")),
            )
        }
        ProviderKind::Anthropic => {
            let blocks = v.get("content").and_then(Value::as_array);
            let text = blocks
                .into_iter()
                .flatten()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<String>();
            (text, raw_reason(v.get("stop_reason")))
        }
        ProviderKind::Google => {
            let cand = v
                .get("candidates")
                .and_then(|c| c.get(0))
                .ok_or_else(|| ProviderError::Malformed("API returned no candidates".into()))?;
            let text = cand["content"]["parts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<String>();
            (text, raw_reason(cand.get("finishReason")))
        }
        ProviderKind::Ollama => (
            v["message"]["content"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            raw_reason(v.get("done_reason")),
        ),
        ProviderKind::Cli => {
            return Err(ProviderError::Unsupported {
                kind: "cli",
                capability: "parse an HTTP response",
            });
        }
    };
    Ok(Completion {
        text,
        finish_reason: finish(raw.as_deref()),
        raw_finish_reason: raw,
    })
}

/// Embedding request (openai-compatible `/embeddings`, ollama `/api/embed`).
pub fn build_embed(
    p: &ProviderConfig,
    api_key: Option<&str>,
    model: &str,
    inputs: &[String],
) -> Result<WireRequest> {
    let base = base_url(p);
    match p.kind {
        ProviderKind::OpenaiCompatible => Ok(WireRequest {
            url: format!("{base}/embeddings"),
            headers: json_headers(api_key),
            body: json!({"model": model, "input": inputs}),
        }),
        ProviderKind::Ollama => Ok(WireRequest {
            url: format!("{base}/api/embed"),
            headers: json_headers(None),
            body: json!({"model": model, "input": inputs}),
        }),
        k => Err(ProviderError::Unsupported {
            kind: k.as_str(),
            capability: "generate embeddings",
        }),
    }
}

fn vector(v: &Value) -> Result<Vec<f32>> {
    v.as_array()
        .ok_or_else(|| ProviderError::Malformed("embedding is not an array".into()))?
        .iter()
        .map(|x| {
            x.as_f64()
                .map(|f| f as f32)
                .ok_or_else(|| ProviderError::Malformed("non-numeric embedding value".into()))
        })
        .collect()
}

pub fn parse_embed(kind: ProviderKind, v: &Value, input_count: usize) -> Result<Embeddings> {
    let vectors: Vec<Vec<f32>> = match kind {
        ProviderKind::OpenaiCompatible => {
            let mut rows: Vec<&Value> = v
                .get("data")
                .and_then(Value::as_array)
                .map(|a| a.iter().collect())
                .unwrap_or_default();
            rows.sort_by_key(|r| r.get("index").and_then(Value::as_u64).unwrap_or(0));
            rows.iter()
                .map(|r| vector(&r["embedding"]))
                .collect::<Result<_>>()?
        }
        ProviderKind::Ollama => {
            if let Some(a) = v.get("embeddings").and_then(Value::as_array) {
                a.iter().map(vector).collect::<Result<_>>()?
            } else if let Some(e) = v.get("embedding") {
                vec![vector(e)?]
            } else {
                Vec::new()
            }
        }
        k => {
            return Err(ProviderError::Unsupported {
                kind: k.as_str(),
                capability: "generate embeddings",
            });
        }
    };
    if vectors.len() != input_count {
        return Err(ProviderError::Malformed(format!(
            "embeddings API returned {} embeddings for {input_count} inputs",
            vectors.len()
        )));
    }
    let dimensions = vectors.first().map(Vec::len);
    Ok(Embeddings {
        vectors,
        dimensions,
    })
}

/// Native rerank (openai-compatible / llama.cpp `/rerank`). Other kinds use the
/// completion-based fallback.
pub fn build_rerank(
    p: &ProviderConfig,
    api_key: Option<&str>,
    model: &str,
    query: &str,
    documents: &[String],
) -> WireRequest {
    WireRequest {
        url: format!("{}/rerank", base_url(p)),
        headers: json_headers(api_key),
        body: json!({"model": model, "query": query, "documents": documents}),
    }
}

/// Strict rerank normalization (`shared.ts:137-163`): a missing results array, wrong count,
/// out-of-range/duplicate index or non-numeric score throws rather than degrading.
pub fn normalize_rerank(v: &Value, document_count: usize) -> Result<Vec<RerankItem>> {
    let bad = |m: String| Err(ProviderError::Malformed(m));
    let Some(results) = v.get("results").and_then(Value::as_array) else {
        return bad("Rerank response did not include a results array".into());
    };
    if results.len() != document_count {
        return bad(format!(
            "Rerank response returned {} results for {document_count} documents",
            results.len()
        ));
    }
    let mut seen = vec![false; document_count];
    let mut out = Vec::with_capacity(document_count);
    for e in results {
        if !e.is_object() {
            return bad("Rerank response contained a malformed result entry".into());
        }
        let index = match e.get("index").and_then(Value::as_u64) {
            Some(i) if (i as usize) < document_count => i as usize,
            _ => {
                return bad(format!(
                    "Rerank response contained an out-of-range or non-integer index: {}",
                    e.get("index").unwrap_or(&Value::Null)
                ));
            }
        };
        if seen[index] {
            return bad(format!(
                "Rerank response contained a duplicate index: {index}"
            ));
        }
        let Some(score) = e
            .get("relevance_score")
            .and_then(Value::as_f64)
            .filter(|f| f.is_finite())
        else {
            return bad(format!(
                "Rerank response contained a non-numeric relevance_score for index {index}"
            ));
        };
        seen[index] = true;
        out.push(RerankItem {
            index,
            relevance_score: score,
        });
    }
    Ok(out)
}

/// `shared.ts:171-178`.
pub const RERANK_FALLBACK_SYSTEM_PROMPT: &str = "You are a relevance-scoring function for a personal knowledge-base search result set.\nYou will be given a query and a numbered list of documents (snippets, not full notes).\nScore how relevant each document is to the query, from 0.0 (irrelevant) to 1.0 (highly relevant).\nRespond with ONLY compact JSON of this exact shape, no other text, no markdown fences:\n{\"results\":[{\"index\":0,\"relevance_score\":0.0},{\"index\":1,\"relevance_score\":0.0}]}\nInclude exactly one entry per document, covering every index from 0 to N-1 exactly once, in any order.";

pub fn rerank_fallback_user_prompt(query: &str, documents: &[String]) -> String {
    let numbered: Vec<String> = documents
        .iter()
        .enumerate()
        .map(|(i, d)| format!("[{i}] {d}"))
        .collect();
    format!("Query: {query}\n\nDocuments:\n{}", numbered.join("\n\n"))
}

/// Parses the fallback completion text: raw, then a bare `{...}` slice, then a fenced block
/// (`shared.ts:192-206`). Never fabricates scores.
pub fn parse_rerank_completion_text(raw: &str, document_count: usize) -> Result<Vec<RerankItem>> {
    let slice = match (raw.find('{'), raw.rfind('}')) {
        (Some(a), Some(b)) if b > a => Some(&raw[a..=b]),
        _ => None,
    };
    let fenced = raw
        .split("```")
        .nth(1)
        .map(|s| s.strip_prefix("json").unwrap_or(s).trim());
    let mut last = "empty response".to_owned();
    for cand in [Some(raw.trim()), slice, fenced]
        .into_iter()
        .flatten()
        .filter(|s| !s.trim().is_empty())
    {
        match serde_json::from_str::<Value>(cand) {
            Ok(v) => match normalize_rerank(&v, document_count) {
                Ok(r) => return Ok(r),
                Err(e) => last = e.to_string(),
            },
            Err(e) => last = e.to_string(),
        }
    }
    Err(ProviderError::Malformed(format!(
        "Rerank fallback response was not usable: {last}"
    )))
}

#[cfg(test)]
mod tests {
    //! Rerank vectors ported from `https://github.com/dulrich/obsidian-crucible/blob/master/tests/providerRerank.test.mjs:73-240`.
    use super::*;

    #[test]
    fn rerank_maps_by_index_not_position() {
        let v = json!({"results": [
            {"index": 2, "relevance_score": 0.9},
            {"index": 0, "relevance_score": 0.1},
            {"index": 1, "relevance_score": 0.5},
        ]});
        let r = normalize_rerank(&v, 3).unwrap();
        let get = |i| r.iter().find(|x| x.index == i).unwrap().relevance_score;
        assert_eq!((get(0), get(1), get(2)), (0.1, 0.5, 0.9));
    }

    #[test]
    fn rerank_rejects_malformed() {
        let cases = [
            (json!({"object": "rerank"}), 2, "results array"),
            (
                json!({"results": [{"index": 0, "relevance_score": "high"}, {"index": 1, "relevance_score": 0.5}]}),
                2,
                "non-numeric relevance_score",
            ),
            (
                json!({"results": [{"index": 0, "relevance_score": 0.7}]}),
                3,
                "returned 1 results for 3 documents",
            ),
            (
                json!({"results": [{"index": 0, "relevance_score": 0.7}, {"index": 0, "relevance_score": 0.2}]}),
                2,
                "duplicate index",
            ),
            (
                json!({"results": [{"index": 5, "relevance_score": 0.7}]}),
                1,
                "out-of-range",
            ),
        ];
        for (v, n, needle) in cases {
            let e = normalize_rerank(&v, n).unwrap_err().to_string();
            assert!(e.contains(needle), "{e} !~ {needle}");
        }
    }

    #[test]
    fn fallback_text_parses_bare_slice_and_fenced_or_fails_precisely() {
        let ok =
            r#"{"results":[{"index":0,"relevance_score":0.2},{"index":1,"relevance_score":0.8}]}"#;
        for t in [
            ok.to_owned(),
            format!("Sure: {ok} done"),
            format!("```json\n{ok}\n```"),
        ] {
            assert_eq!(parse_rerank_completion_text(&t, 2).unwrap().len(), 2);
        }
        let e = parse_rerank_completion_text("I cannot rank these, sorry.", 1)
            .unwrap_err()
            .to_string();
        assert!(e.contains("Rerank fallback response was not usable"));
    }

    #[test]
    fn finish_reasons_normalize() {
        let c = parse_completion(
            ProviderKind::Anthropic,
            &json!({"content": [{"text": "a"}, {"text": "b"}], "stop_reason": "max_tokens"}),
        )
        .unwrap();
        assert_eq!(c.text, "ab");
        assert_eq!(c.finish_reason, FinishReason::Length);
        assert!(parse_completion(ProviderKind::OpenaiCompatible, &json!({"choices": []})).is_err());
    }
}

#[cfg(test)]
mod image_wire_tests {
    //! Plan `v0-automation-pipelines` WP-6: multimodal wire shapes per kind; text-only requests
    //! keep the pre-WP-6 shape byte for byte.
    use super::*;
    use crate::config::ProvidersConfig;

    fn prov(kind: &str) -> ProviderConfig {
        ProvidersConfig::from_toml(&format!(
            "[[provider]]\nid = \"p\"\nkind = \"{kind}\"\nbase_url = \"http://h\"\n"
        ))
        .unwrap()
        .providers
        .remove(0)
    }

    fn img() -> Vec<ImagePart> {
        vec![ImagePart {
            mime: "image/png".into(),
            base64: "QUJD".into(),
        }]
    }

    fn body(kind: &str, system: &str, images: &[ImagePart]) -> Value {
        build_completion(&prov(kind), Some("k"), "m", system, "U", images, 7)
            .unwrap()
            .body
    }

    fn with_extras(kind: &str, extras: &WireExtras) -> Value {
        let p = prov(kind);
        let mut w = build_completion(&p, Some("k"), "m", "S", "U", &[], 7).unwrap();
        apply_extras(p.kind, &mut w, extras);
        w.body
    }

    #[test]
    fn anthropic_auth_header_replaces_x_api_key() {
        let names = |p: &ProviderConfig| {
            build_completion(p, Some("k"), "m", "S", "U", &[], 7)
                .unwrap()
                .headers
                .into_iter()
                .map(|(n, _)| n)
                .collect::<Vec<String>>()
        };
        let default = names(&prov("anthropic"));
        assert_eq!(default, ["content-type", "x-api-key", "anthropic-version"]);
        let mut p = prov("anthropic");
        p.auth_header = Some("x-gateway-key".into());
        let w = build_completion(&p, Some("k"), "m", "S", "U", &[], 7).unwrap();
        assert!(
            w.headers
                .iter()
                .any(|(n, v)| n == "x-gateway-key" && v == "k")
        );
        assert!(!w.headers.iter().any(|(n, _)| n == "x-api-key"));
        assert_eq!(
            names(&p),
            ["content-type", "x-gateway-key", "anthropic-version"]
        );
    }

    #[test]
    fn unset_extras_leave_every_kind_byte_identical() {
        for kind in ["openai-compatible", "anthropic", "google", "ollama"] {
            let p = prov(kind);
            let plain = build_completion(&p, Some("k"), "m", "S", "U", &img(), 7).unwrap();
            let mut with = plain.clone();
            apply_extras(p.kind, &mut with, &WireExtras::default());
            assert_eq!(with.body.to_string(), plain.body.to_string(), "{kind}");
        }
        // The pre-round-16 openai-compatible text-only JSON, literally.
        assert_eq!(
            with_extras("openai-compatible", &WireExtras::default()).to_string(),
            json!({"model": "m", "messages": [{"role": "system", "content": "S"}, {"role": "user", "content": "U"}], "max_tokens": 7}).to_string()
        );
    }

    #[test]
    fn set_extras_land_only_on_openai_compatible() {
        let e = WireExtras {
            reasoning_effort: Some("none".into()),
            temperature: Some(1.0),
            top_p: Some(0.95),
            top_k: Some(64),
            repeat_penalty: Some(1.15),
            repeat_last_n: Some(256),
        };
        let b = with_extras("openai-compatible", &e);
        assert_eq!(b["reasoning_effort"], "none");
        assert_eq!(b["temperature"], 1.0);
        assert_eq!(b["top_p"], 0.95);
        assert_eq!(b["top_k"], 64);
        assert_eq!(b["repeat_penalty"], 1.15);
        assert_eq!(b["repeat_last_n"], 256);
        for kind in ["anthropic", "google", "ollama"] {
            assert_eq!(
                with_extras(kind, &e).to_string(),
                with_extras(kind, &WireExtras::default()).to_string(),
                "{kind}"
            );
        }
    }

    #[test]
    fn text_only_shapes_unchanged() {
        let chat = json!([{"role": "system", "content": "S"}, {"role": "user", "content": "U"}]);
        assert_eq!(
            body("openai-compatible", "S", &[]).to_string(),
            json!({"model": "m", "messages": chat, "max_tokens": 7}).to_string()
        );
        assert_eq!(
            body("anthropic", "S", &[]).to_string(),
            json!({"model": "m", "system": "S", "messages": [{"role": "user", "content": "U"}], "max_tokens": 7}).to_string()
        );
        assert_eq!(
            body("google", "", &[]).to_string(),
            json!({"contents": [{"role": "user", "parts": [{"text": "U"}]}], "generationConfig": {"maxOutputTokens": 7}}).to_string()
        );
        assert_eq!(
            body("ollama", "S", &[]).to_string(),
            json!({"model": "m", "messages": chat, "stream": false, "options": {"num_predict": 7}})
                .to_string()
        );
    }

    #[test]
    fn image_shapes_per_kind() {
        assert_eq!(
            body("openai-compatible", "", &img())["messages"],
            json!([{"role": "user", "content": [
                {"type": "text", "text": "U"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,QUJD"}},
            ]}])
        );
        let a = body("anthropic", "", &img());
        assert!(a.get("system").is_none());
        assert_eq!(
            a["messages"][0]["content"],
            json!([
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}},
                {"type": "text", "text": "U"},
            ])
        );
        assert_eq!(
            body("google", "", &img())["contents"][0]["parts"],
            json!([{"text": "U"}, {"inline_data": {"mime_type": "image/png", "data": "QUJD"}}])
        );
        assert_eq!(
            body("ollama", "", &img())["messages"],
            json!([{"role": "user", "content": "U", "images": ["QUJD"]}])
        );
        assert!(!format!("{:?}", img()[0]).contains("QUJD"));
    }
}
