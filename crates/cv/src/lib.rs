//! `cv`: the crucible-vault core daemon and its CLI (plan `v0-core-daemon` WP-8).
//!
//! Library + thin `main` so integration tests drive an in-process daemon.

pub mod cli;
pub mod client;
pub mod config;
pub mod cutover;
pub mod daemon;
pub mod runner;
pub mod server;
pub mod shadow;
pub mod spool;
pub mod steps;
pub mod uri;
pub mod views;

pub use client::{Client, ClientError};
pub use config::Config;
pub use daemon::{Daemon, DaemonOptions, RunningDaemon, Shared};
pub use runner::Runner;
pub use views::{ViewRegistry, ViewService, register_views};

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
