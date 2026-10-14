//! The cut-over marker (plan `cut-over-now` WP-1): the state.db-backed [`CutoverMarker`] the live
//! daemon consults, and `cv cutover status|mark`.
//!
//! `mark` is the only code path that creates the marker. It refuses, in order: a missing
//! `--yes`; anything accepting connections on `CV_SOCKET` (fail-closed raw connect, the
//! `shadow import-descriptions` shape — the shadow daemon shares state.db); a `--vault` that is
//! not the protected vault; any unfinished job without `--cancel-unfinished` (audit B1: a leftover
//! shadow job would otherwise be recovered and claimed by the live daemon).

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context, Result, bail};
use cv_state::{QueueConfig, StateDb, StateReader};
use cv_vault::{CutoverMarker, VaultError};

use crate::config::Config;

/// The live daemon's marker: present iff state.db holds a marker whose vault is the canonical
/// protected root.
#[derive(Debug, Clone)]
pub struct StateCutover {
    state: Arc<Mutex<StateDb>>,
    protected: String,
}

impl StateCutover {
    /// Marker over the daemon's shared `state` for `protected_root`.
    #[must_use]
    pub fn new(state: Arc<Mutex<StateDb>>, protected_root: &Path) -> Self {
        Self {
            state,
            protected: canonical(protected_root),
        }
    }
}

impl CutoverMarker for StateCutover {
    fn present(&self) -> cv_vault::Result<bool> {
        let db = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let rec = db.cutover().map_err(VaultError::State)?;
        Ok(rec.is_some_and(|r| r.vault == self.protected))
    }
}

fn canonical(p: &Path) -> String {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .display()
        .to_string()
}

const USAGE: &str =
    "usage: cv cutover status\n       cv cutover mark --vault <path> --yes [--cancel-unfinished]\n";

/// `cv cutover …`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    let r = match args.first().map(String::as_str) {
        Some("status") if args.len() == 1 => Config::from_env().and_then(|c| status(&c)),
        Some("mark") => Config::from_env().and_then(|c| mark(&c, &args[1..])),
        _ => {
            eprint!("{USAGE}");
            return 2;
        }
    };
    match r {
        Ok(text) => {
            print!("{text}");
            0
        }
        Err(e) => {
            eprintln!("cv: {e:#}");
            1
        }
    }
}

/// `cv cutover status`: read-only; never creates state.db.
pub fn status(config: &Config) -> Result<String> {
    if !config.state_db.exists() {
        return Ok(format!(
            "no state.db at {}\nmarker: absent\n",
            config.state_db.display()
        ));
    }
    let r = StateReader::open(&config.state_db)
        .with_context(|| format!("opening {}", config.state_db.display()))?;
    let mut out = format!("state.db: {}\n", config.state_db.display());
    match r.cutover()? {
        Some(m) => out.push_str(&format!(
            "marker: present\n  vault: {}\n  created_at_ms: {}\n  created_by: {}\n",
            m.vault, m.created_at_ms, m.created_by
        )),
        None => out.push_str("marker: absent\n"),
    }
    let jobs = r.unfinished_jobs()?;
    let queued = jobs
        .iter()
        .filter(|j| j.status == cv_state::JobStatus::Queued)
        .count();
    out.push_str(&format!(
        "unfinished jobs: queued {queued}, running {}\n",
        jobs.len() - queued
    ));
    Ok(out)
}

/// `cv cutover mark --vault <path> --yes [--cancel-unfinished]`.
pub fn mark(config: &Config, args: &[String]) -> Result<String> {
    let mut vault = None;
    let mut yes = false;
    let mut cancel = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--vault" => vault = it.next().cloned(),
            "--yes" => yes = true,
            "--cancel-unfinished" => cancel = true,
            other => bail!("unknown argument {other:?}\n{USAGE}"),
        }
    }
    let Some(vault) = vault else {
        bail!("--vault <path> is required\n{USAGE}");
    };
    if !yes {
        bail!("refusing without --yes: the marker lets `CV_MODE=live` write the protected vault");
    }
    if std::os::unix::net::UnixStream::connect(&config.socket).is_ok() {
        bail!(
            "something is accepting connections on {}; stop every daemon first (`systemctl --user stop crucible-vault-shadow`) — state.db has a single writer",
            config.socket.display()
        );
    }
    let protected = std::fs::canonicalize(&config.protected_vault).with_context(|| {
        format!(
            "canonicalizing the protected vault {}",
            config.protected_vault.display()
        )
    })?;
    let given =
        std::fs::canonicalize(&vault).with_context(|| format!("canonicalizing --vault {vault}"))?;
    if given != protected {
        bail!(
            "--vault {} is not the protected vault {}; refusing a mismatched vault",
            given.display(),
            protected.display()
        );
    }
    if let Some(parent) = config.state_db.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut db = StateDb::open(&config.state_db, crate::now_ms(), QueueConfig::default())
        .with_context(|| format!("opening {}", config.state_db.display()))?;
    let mut out = String::new();
    if let Some(m) = db.cutover()? {
        out.push_str(&format!(
            "marker already set: vault {}, created_at_ms {}, created_by {}\n",
            m.vault, m.created_at_ms, m.created_by
        ));
        return Ok(out);
    }
    let unfinished = db.unfinished_jobs()?;
    if !unfinished.is_empty() {
        if !cancel {
            let list: Vec<String> = unfinished
                .iter()
                .map(|j| format!("{} ({}, {})", j.id, j.job_type, j.status.as_str()))
                .collect();
            bail!(
                "{} unfinished job(s) would run live: {}; re-run with --cancel-unfinished",
                unfinished.len(),
                list.join(", ")
            );
        }
        for j in db.cancel_unfinished(crate::now_ms())? {
            out.push_str(&format!(
                "cancelled {} ({}, was {})\n",
                j.id,
                j.job_type,
                j.status.as_str()
            ));
        }
    }
    let actor = format!(
        "cv cutover mark ({})",
        std::env::var("USER").unwrap_or_else(|_| "unknown".to_owned())
    );
    let m = db.set_cutover(&protected.display().to_string(), &actor, crate::now_ms())?;
    out.push_str(&format!(
        "marker set: vault {}, created_at_ms {}, created_by {}\n",
        m.vault, m.created_at_ms, m.created_by
    ));
    Ok(out)
}
