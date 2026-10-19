//! `providers.toml` schema.

use super::*;

// ---------------------------------------------------------------------------------------------
// providers.toml — `cv_providers::config::ProvidersConfig`
// ---------------------------------------------------------------------------------------------

/// `ModelConfig`.
pub static MODEL: Record = Record {
    name: "model",
    fields: &[
        field("id", "Id", "Model id", Node::Str).req(),
        field("label", "Label", "Display name in model pickers", Node::Str),
        field(
            "capabilities",
            "Capabilities",
            "Absent = chat only",
            Node::StrList {
                scalar_ok: false,
                choices: &["chat", "embed", "rerank"],
            },
        ),
    ],
    rules: &[],
};

/// `ProviderConfig`. Secrets are referenced by name only.
pub static PROVIDER: Record = Record {
    name: "provider",
    fields: &[
        field("id", "Id", "Non-empty, no ':'", Node::Str).req(),
        field(
            "kind",
            "Kind",
            "Provider kind",
            Node::Enum(&["openai-compatible", "anthropic", "google", "ollama", "cli"]),
        )
        .req(),
        field("name", "Name", "Label", Node::Str),
        field(
            "base_url",
            "Base URL",
            "API base; default per kind",
            Node::Str,
        ),
        field(
            "api_key_env",
            "API key env var",
            "Env var name (never the key)",
            Node::Str,
        ),
        field(
            "api_key_file",
            "API key file",
            "File name under the secrets dir",
            Node::Str,
        ),
        field(
            "max_concurrent_requests",
            "Max concurrent",
            "0/absent = default",
            U32,
        ),
        field(
            "local",
            "Local",
            "Overrides the local-default heuristic",
            Node::Bool,
        ),
        field("max_tokens", "Max tokens", "Provider completion cap", U32),
        field(
            "command",
            "Command",
            "CLI: the executable (required for kind = cli)",
            Node::Str,
        ),
        field("args", "Args", "CLI: prepended args", STR_LIST),
        field("extra_args", "Extra args", "CLI: appended args", STR_LIST),
        field("cwd", "Working dir", "CLI working directory", Node::Str),
        field(
            "timeout_seconds",
            "Timeout (s)",
            "Request timeout: CLI and HTTP providers (HTTP default 600)",
            UINT,
        ),
        field(
            "models",
            "Models",
            "Served models",
            Node::List(&Node::Record(&MODEL)),
        ),
    ],
    rules: &[],
};

/// `providers.toml`.
pub static PROVIDERS_FILE: Record = Record {
    name: "providers.toml",
    fields: &[field(
        "provider",
        "Providers",
        "One [[provider]] each",
        Node::List(&Node::Record(&PROVIDER)),
    )],
    rules: &[],
};
