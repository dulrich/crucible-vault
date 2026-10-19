//! `cv-actions`: the pure action model and its TOML config (plan `v0-core-daemon` WP-9).
//!
//! An action is an id, a label, a typed step list, starters, a resource-key derivation and a
//! lane (D3). Sync and I/O-free: the [`ConfigSet`] loader takes file contents, never paths, and
//! step validation is injected by the caller (`cv`'s `steps::validate`).
//!
//! This replaces Crucible's chains (`https://github.com/dulrich/obsidian-crucible/blob/master/src/chains.ts`), the internal-command
//! registry and `registerCrucibleCommand`/hidden commands (`src/internalCommands.ts`), the
//! `mutating` flag, and the trigger registry (`src/orchestration/TriggerRegistry.ts`,
//! `src/triggers/`). All of that is Obsidian's command model (inventory §3) and is dropped:
//! a chain becomes an action's step list (sub-chains → `run_action` steps, [`Step::run_action`]), a command
//! becomes a [`Starter::Command`] reached over the `run_action` RPC, the `mutating` flag becomes
//! [`Step::mutates_note`] + [`ResourceSpec`] (every run is a queue job serialized by resource
//! keys), and a trigger becomes a [`Starter::Event`] with a mandatory selector.

mod config;
mod model;
pub mod pipelines;

pub use config::{CONFIG_FILES, ConfigSet, StepValidator};
pub use cv_protocol::{ParamKind, ParamSpec};
pub use model::{
    Action, ChangeKindSel, LaneChoice, OriginSel, PropSelector, ResourceSpec, RunAction, Schedule,
    Selector, Starter, Step, StepKind,
};
pub use pipelines::{PipelinesConfig, SourcesConfig};
