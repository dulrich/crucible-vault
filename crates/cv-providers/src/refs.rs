//! `"providerId:modelId"` model-ref and agent-binding contract.
//!
//! Carried from Crucible `https://github.com/dulrich/obsidian-crucible/blob/master/src/providerModelContract.ts:1-204`
//! (`src/providers/AGENTS.md:19`, rem-R2): the single ref/binding parser, kept a
//! dependency-free leaf — this module imports nothing from the rest of the crate (only `std`
//! and `serde_json` for reading untyped config), so any consumer may use it without pulling in
//! the HTTP clients. Nothing Obsidian-induced to drop: the contract is pure data.

use serde_json::Value;

/// Separator between provider id and model id. Splits on the FIRST occurrence, so a model id
/// may itself contain `:` (e.g. `gemma-4-12b:q8_0`).
pub const MODEL_REF_SEPARATOR: char = ':';

/// A single (provider, model) pair. Ids are opaque strings owned by the provider config.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ProviderModelRef {
    pub provider_id: String,
    pub model_id: String,
}

impl ProviderModelRef {
    pub fn new(provider_id: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            model_id: model_id.into(),
        }
    }

    /// Whether both halves are filled in.
    pub fn is_complete(&self) -> bool {
        !self.provider_id.is_empty() && !self.model_id.is_empty()
    }
}

impl std::fmt::Display for ProviderModelRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{MODEL_REF_SEPARATOR}{}",
            self.provider_id, self.model_id
        )
    }
}

/// The string form, as stored in action steps (`formatModelRef`).
pub fn format_model_ref(r: &ProviderModelRef) -> String {
    r.to_string()
}

/// Parses `"providerId:modelId"`; every incomplete form (no separator, an empty or
/// whitespace-only half) is rejected rather than returning a partial ref (`parseModelRef`).
pub fn parse_model_ref(raw: Option<&str>) -> Option<ProviderModelRef> {
    let trimmed = raw?.trim();
    let (p, m) = trimmed.split_once(MODEL_REF_SEPARATOR)?;
    let (p, m) = (p.trim(), m.trim());
    if p.is_empty() || m.is_empty() {
        return None;
    }
    Some(ProviderModelRef::new(p, m))
}

/// How an agent picks its model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentModelBinding {
    Pinned(ProviderModelRef),
    Constrained(Vec<ProviderModelRef>),
    Runtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentBindingMode {
    Pinned,
    Constrained,
    Runtime,
}

/// A fresh empty variant carrying only its own payload, so a mode change drops stale data.
pub fn binding_for_mode(mode: AgentBindingMode) -> AgentModelBinding {
    match mode {
        AgentBindingMode::Pinned => AgentModelBinding::Pinned(ProviderModelRef::default()),
        AgentBindingMode::Constrained => AgentModelBinding::Constrained(Vec::new()),
        AgentBindingMode::Runtime => AgentModelBinding::Runtime,
    }
}

/// The refs a binding names: a complete pinned ref, the whole allowlist, or nothing.
pub fn binding_model_refs(b: &AgentModelBinding) -> Vec<ProviderModelRef> {
    match b {
        AgentModelBinding::Pinned(r) if r.is_complete() => vec![r.clone()],
        AgentModelBinding::Pinned(_) | AgentModelBinding::Runtime => Vec::new(),
        AgentModelBinding::Constrained(allow) => allow.clone(),
    }
}

fn read_ref(raw: Option<&Value>) -> ProviderModelRef {
    match raw {
        Some(Value::String(s)) => parse_model_ref(Some(s)).unwrap_or_default(),
        Some(Value::Object(o)) => {
            let field = |k: &str| {
                o.get(k)
                    .and_then(Value::as_str)
                    .map(|s| s.trim().to_owned())
                    .unwrap_or_default()
            };
            ProviderModelRef::new(field("providerId"), field("modelId"))
        }
        _ => ProviderModelRef::default(),
    }
}

fn read_allow(raw: Option<&Value>) -> Vec<ProviderModelRef> {
    let Some(Value::Array(items)) = raw else {
        return Vec::new();
    };
    let mut out: Vec<ProviderModelRef> = Vec::new();
    for item in items {
        let r = read_ref(Some(item));
        if r.is_complete() && !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

/// Normalizes an untyped stored binding (`normalizeAgentBinding`): valid variants pass through,
/// stale cross-variant payload is dropped, an unknown mode is recovered from a coherent payload
/// (pinned first, then allowlist), anything else is `Runtime`. Idempotent.
pub fn normalize_agent_binding(raw: &Value) -> AgentModelBinding {
    let Value::Object(o) = raw else {
        return AgentModelBinding::Runtime;
    };
    match o.get("mode").and_then(Value::as_str) {
        Some("pinned") => AgentModelBinding::Pinned(read_ref(o.get("pinned"))),
        Some("constrained") => AgentModelBinding::Constrained(read_allow(o.get("allow"))),
        Some("runtime") => AgentModelBinding::Runtime,
        _ => {
            let pinned = read_ref(o.get("pinned"));
            if pinned.is_complete() {
                return AgentModelBinding::Pinned(pinned);
            }
            let allow = read_allow(o.get("allow"));
            if allow.is_empty() {
                AgentModelBinding::Runtime
            } else {
                AgentModelBinding::Constrained(allow)
            }
        }
    }
}

impl AgentModelBinding {
    /// Serializes back to the untyped stored shape (so normalize can be shown idempotent).
    pub fn to_value(&self) -> Value {
        let r = |r: &ProviderModelRef| serde_json::json!({"providerId": r.provider_id, "modelId": r.model_id});
        match self {
            Self::Pinned(p) => serde_json::json!({"mode": "pinned", "pinned": r(p)}),
            Self::Constrained(a) => {
                serde_json::json!({"mode": "constrained", "allow": a.iter().map(r).collect::<Vec<_>>()})
            }
            Self::Runtime => serde_json::json!({"mode": "runtime"}),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Ported from `https://github.com/dulrich/obsidian-crucible/blob/master/tests/providerRefs.test.mjs:227-420`.
    use super::*;
    use serde_json::json;

    fn r(p: &str, m: &str) -> ProviderModelRef {
        ProviderModelRef::new(p, m)
    }

    #[test]
    fn parse_splits_well_formed_on_first_separator_and_trims() {
        assert_eq!(parse_model_ref(Some("p1:m1")), Some(r("p1", "m1")));
        assert_eq!(
            parse_model_ref(Some("p1:gemma-4-12b:q8_0")),
            Some(r("p1", "gemma-4-12b:q8_0"))
        );
        assert_eq!(parse_model_ref(Some("  p1 : m1  ")), Some(r("p1", "m1")));
    }

    #[test]
    fn parse_rejects_every_incomplete_form() {
        for raw in ["p1", ":m1", "p1:", ":", "   ", ""] {
            assert_eq!(parse_model_ref(Some(raw)), None, "{raw:?}");
        }
        assert_eq!(parse_model_ref(None), None);
    }

    #[test]
    fn format_round_trips() {
        let x = r("local", "openai/text-embedding-3-small");
        assert_eq!(format_model_ref(&x), "local:openai/text-embedding-3-small");
        assert_eq!(parse_model_ref(Some(&format_model_ref(&x))), Some(x));
    }

    #[test]
    fn completeness_and_equality() {
        assert!(r("p1", "m1").is_complete());
        assert!(!r("p1", "").is_complete());
        assert!(!r("", "m1").is_complete());
        assert!(!r("", "").is_complete());
        assert_ne!(r("p1", "m1"), r("p1", "m2"));
        assert_ne!(r("p1", "m1"), r("p2", "m1"));
    }

    #[test]
    fn binding_for_mode_and_refs() {
        assert_eq!(
            binding_for_mode(AgentBindingMode::Pinned),
            AgentModelBinding::Pinned(r("", ""))
        );
        assert_eq!(
            binding_for_mode(AgentBindingMode::Constrained),
            AgentModelBinding::Constrained(vec![])
        );
        assert_eq!(
            binding_for_mode(AgentBindingMode::Runtime),
            AgentModelBinding::Runtime
        );
        let (a, b) = (r("p1", "m1"), r("p2", "m2"));
        assert_eq!(
            binding_model_refs(&AgentModelBinding::Pinned(a.clone())),
            vec![a.clone()]
        );
        assert_eq!(
            binding_model_refs(&AgentModelBinding::Constrained(vec![a.clone(), b.clone()])),
            vec![a, b]
        );
        assert!(binding_model_refs(&AgentModelBinding::Runtime).is_empty());
        assert!(binding_model_refs(&AgentModelBinding::Pinned(r("p1", ""))).is_empty());
    }

    #[test]
    fn normalize_passes_valid_variants_and_is_idempotent() {
        for b in [
            AgentModelBinding::Pinned(r("p1", "m1")),
            AgentModelBinding::Constrained(vec![r("p1", "m1")]),
            AgentModelBinding::Runtime,
        ] {
            assert_eq!(normalize_agent_binding(&b.to_value()), b);
        }
        for raw in [
            json!({"mode": "legacy", "allow": ["p1:m1", "p1:m1"]}),
            json!({"pinned": "p1:m1"}),
            json!(7),
        ] {
            let once = normalize_agent_binding(&raw);
            assert_eq!(normalize_agent_binding(&once.to_value()), once);
        }
    }

    #[test]
    fn normalize_preserves_half_pinned_and_drops_stale_payload() {
        assert_eq!(
            normalize_agent_binding(
                &json!({"mode": "pinned", "pinned": {"providerId": "p1", "modelId": ""}})
            ),
            AgentModelBinding::Pinned(r("p1", ""))
        );
        assert_eq!(
            normalize_agent_binding(
                &json!({"mode": "runtime", "pinned": {"providerId": "p1", "modelId": "m1"}})
            ),
            AgentModelBinding::Runtime
        );
        assert_eq!(
            normalize_agent_binding(
                &json!({"mode": "pinned", "pinned": {"providerId": "p1", "modelId": "m1"}, "allow": ["p2:m2"]})
            ),
            AgentModelBinding::Pinned(r("p1", "m1"))
        );
    }

    #[test]
    fn normalize_fills_missing_payloads() {
        assert_eq!(
            normalize_agent_binding(&json!({"mode": "pinned"})),
            AgentModelBinding::Pinned(r("", ""))
        );
        assert_eq!(
            normalize_agent_binding(&json!({"mode": "pinned", "pinned": null})),
            AgentModelBinding::Pinned(r("", ""))
        );
        for allow in [json!(null), json!("p1:m1")] {
            assert_eq!(
                normalize_agent_binding(&json!({"mode": "constrained", "allow": allow})),
                AgentModelBinding::Constrained(vec![])
            );
        }
        assert_eq!(
            normalize_agent_binding(&json!({"mode": "constrained"})),
            AgentModelBinding::Constrained(vec![])
        );
    }

    #[test]
    fn allowlist_drops_junk_reads_strings_and_dedupes_first_wins() {
        assert_eq!(
            normalize_agent_binding(&json!({"mode": "constrained", "allow": [
                {"providerId": "p1", "modelId": "m1"}, {"providerId": "p1"}, null, 5, "nocolon",
                {"providerId": "p2", "modelId": "m2"}
            ]})),
            AgentModelBinding::Constrained(vec![r("p1", "m1"), r("p2", "m2")])
        );
        assert_eq!(
            normalize_agent_binding(
                &json!({"mode": "constrained", "allow": ["p1:m1", {"providerId": "p1", "modelId": "m1"}, "p2:m2"]})
            ),
            AgentModelBinding::Constrained(vec![r("p1", "m1"), r("p2", "m2")])
        );
    }

    #[test]
    fn string_pinned_non_string_fields_and_trim() {
        assert_eq!(
            normalize_agent_binding(&json!({"mode": "pinned", "pinned": "p1:m1"})),
            AgentModelBinding::Pinned(r("p1", "m1"))
        );
        assert_eq!(
            normalize_agent_binding(
                &json!({"mode": "pinned", "pinned": {"providerId": 7, "modelId": "  m1  "}})
            ),
            AgentModelBinding::Pinned(r("", "m1"))
        );
    }

    #[test]
    fn unknown_mode_recovers_from_coherent_payload() {
        assert_eq!(
            normalize_agent_binding(
                &json!({"mode": "legacy-auto", "pinned": {"providerId": "p1", "modelId": "m1"}})
            ),
            AgentModelBinding::Pinned(r("p1", "m1"))
        );
        assert_eq!(
            normalize_agent_binding(&json!({"allow": [{"providerId": "p1", "modelId": "m1"}]})),
            AgentModelBinding::Constrained(vec![r("p1", "m1")])
        );
        assert_eq!(
            normalize_agent_binding(
                &json!({"pinned": {"providerId": "p1", "modelId": "m1"}, "allow": [{"providerId": "p2", "modelId": "m2"}]})
            ),
            AgentModelBinding::Pinned(r("p1", "m1"))
        );
        assert_eq!(
            normalize_agent_binding(&json!({})),
            AgentModelBinding::Runtime
        );
        assert_eq!(
            normalize_agent_binding(&json!({"mode": "pinned:", "pinned": {"providerId": "p1"}})),
            AgentModelBinding::Runtime
        );
        for raw in [
            json!(null),
            json!(0),
            json!("pinned"),
            json!([1]),
            json!(true),
        ] {
            assert_eq!(normalize_agent_binding(&raw), AgentModelBinding::Runtime);
        }
    }

    #[test]
    fn module_is_a_leaf() {
        // Leaf-ness guard (providerRefs.test.mjs:220): no intra-crate imports.
        let src = include_str!("refs.rs");
        let needle = ["use ", "crate::"].concat();
        assert!(!src.contains(&needle));
        let needle2 = ["super::", "super"].concat();
        assert!(!src.contains(&needle2));
    }
}
