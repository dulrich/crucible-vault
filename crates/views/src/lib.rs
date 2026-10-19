//! `views` — daemon-native view definitions (TOML under `_custom/crucible-vault/views/`) and the
//! Bases-subset expression evaluator (plan `v0-editor.md` WP-6), hosted in the daemon through
//! its view-host thread (`crates/cv/src/views.rs` runs [`ViewHost`]).
//!
//! Pure: no iced/tokio/reqwest; the index is only ever opened read-only. Selection narrows by
//! SQL ([`engine`]), everything else evaluates in Rust ([`eval`]). Definition problems become
//! notices in the [`result::ViewResult`], never panics or daemon failures.

pub mod db;
pub mod def;
pub mod engine;
pub mod eval;
pub mod expr;
pub mod result;
pub mod service;
pub mod value;

pub use result::ViewResult;
pub use service::{ViewHost, ViewsError};

#[cfg(test)]
mod tests;
