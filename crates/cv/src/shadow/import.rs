//! `cv shadow import-descriptions <dir>`: the one-time import of Crucible's
//! `image-descriptions/*.json` records into `state.db` (WP-13 follow-up).
//!
//! Source: `https://github.com/dulrich/obsidian-crucible/blob/master/src/search/imageDescriptionStore.ts` (records keyed by the file
//! basename `<md5>.json`); the import itself is `cv_state::StateDb::import_image_descriptions_dir`.
//! Dropped (Obsidian-induced): the plugin-data-dir storage — the records move into SQLite once.
//!
//! The daemon is the single writer of `state.db` (I2): the import refuses while a daemon answers
//! on `CV_SOCKET`. It reads `<dir>` only.

use std::path::Path;

use anyhow::{Context, Result, bail};
use cv_state::{QueueConfig, StateDb};
use serde_json::{Value, json};

use crate::config::Config;

/// Imports `dir` into `config.state_db`; returns the `ImportSummary` as JSON.
pub fn run(config: &Config, dir: &Path) -> Result<Value> {
    if std::os::unix::net::UnixStream::connect(&config.socket).is_ok() {
        bail!(
            "a daemon is answering on {}; stop it first (`systemctl --user stop crucible-vault-shadow`) — state.db has a single writer",
            config.socket.display()
        );
    }
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    if let Some(parent) = config.state_db.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut db = StateDb::open(&config.state_db, crate::now_ms(), QueueConfig::default())
        .with_context(|| format!("opening {}", config.state_db.display()))?;
    let s = db
        .import_image_descriptions_dir(dir)
        .context("importing image descriptions")?;
    Ok(json!({
        "read": s.read,
        "imported": s.imported,
        "skipped": s.skipped,
        "errors": s.errors,
    }))
}
