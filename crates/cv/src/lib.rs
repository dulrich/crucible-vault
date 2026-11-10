//! `cv`: the crucible-vault core daemon and its CLI (plan `v0-core-daemon` WP-8).
//!
//! Library + thin `main` so integration tests drive an in-process daemon.

pub mod cli;
pub mod client;
pub mod config;
pub mod daemon;
pub mod jobs;
pub mod lineage;
pub mod lookup;
pub mod media_audit;
pub mod note_id;
pub mod runner;
pub mod search_query;
pub mod server;
pub mod signals;
pub mod spool;
pub mod steps;
pub mod supersede;
pub mod sync;
pub mod uri;
pub mod views;

pub use client::{Client, ClientError};
pub use config::Config;
pub use daemon::{Daemon, DaemonOptions, RunningDaemon, Shared};
pub use runner::Runner;
pub use views::ViewRegistry;

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
