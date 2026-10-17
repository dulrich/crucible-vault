//! `cv-providers`: model provider clients for the core daemon (plan `v0-core-daemon.md` WP-4).
//!
//! Kinds: `openai-compatible` (LM Studio, llama.cpp, OpenRouter, inference-server),
//! `anthropic`, `google`, `ollama`, `cli` (Codex). Carried from Crucible
//! `https://github.com/dulrich/obsidian-crucible/blob/master/src/providers/*.ts`; see each module for citations and drops.

pub mod catalog;
pub mod cli;
pub mod config;
pub mod error;
pub mod limiter;
pub mod manager;
pub mod refs;
pub mod secrets;
pub mod web;
pub mod wire;

pub use config::{Capability, ModelConfig, ProviderConfig, ProviderKind, ProvidersConfig};
pub use error::{ProviderError, Result};
pub use manager::{CompletionRequest, ImagePart, ProviderManager};
pub use refs::{ProviderModelRef, parse_model_ref};
pub use secrets::{SecretSource, SystemSecrets};
pub use wire::{Completion, Embeddings, FinishReason, RerankItem};
