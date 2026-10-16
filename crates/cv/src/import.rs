//! `cv import …` (plan `v0-automation-pipelines` WP-3; WP-12 adds `sources` / `ignored`).
//!
//! `cv import link-records --from <jobs.sqlite>`: reads Crucible's frozen `link_records` table
//! (opened `mode=ro&immutable=1`, `cv_state::read_crucible_link_records`) and feeds every row
//! through the `state.db` merge (`StateDb::upsert_link_record`), so a re-import is idempotent and
//! merges with anything scanned since. Refuses while a daemon accepts connections on `CV_SOCKET`
//! (state.db has a single writer — the `cv cutover mark` rule, `cutover.rs`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use cv_state::{LinkRecordInput, QueueConfig, StateDb};

use crate::config::Config;

const USAGE: &str = "usage: cv import link-records --from <jobs.sqlite>\n";

/// `cv import …`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    let r = match (args.first().map(String::as_str), parse_from(args.get(1..))) {
        (Some("link-records"), Some(from)) => {
            Config::from_env().and_then(|c| link_records(&c, &from))
        }
        _ => {
            eprint!("{USAGE}");
            return 2;
        }
    };
    match r {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("cv: {e:#}");
            1
        }
    }
}

fn parse_from(args: Option<&[String]>) -> Option<PathBuf> {
    match args? {
        [flag, path] if flag == "--from" => Some(PathBuf::from(path)),
        _ => None,
    }
}

/// Counts of one import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImportCounts {
    pub read: u64,
    pub created: u64,
    pub updated: u64,
    pub unchanged: u64,
}

/// Imports `from`'s `link_records` into `state_db` (no daemon check; see [`link_records`]).
pub fn import_link_records(state_db: &Path, from: &Path) -> Result<ImportCounts> {
    if !from.is_file() {
        bail!("{} is not a file", from.display());
    }
    let rows = cv_state::read_crucible_link_records(from)
        .with_context(|| format!("reading link_records from {}", from.display()))?;
    let mut db = StateDb::open(state_db, crate::now_ms(), QueueConfig::default())
        .with_context(|| format!("opening {}", state_db.display()))?;
    let mut c = ImportCounts::default();
    for r in rows {
        c.read += 1;
        let o = db.upsert_link_record(&LinkRecordInput::from(r))?;
        match (o.created, o.changed) {
            (true, _) => c.created += 1,
            (false, true) => c.updated += 1,
            (false, false) => c.unchanged += 1,
        }
    }
    Ok(c)
}

/// `cv import link-records --from <path>`: refuses while the daemon socket accepts connections.
pub fn link_records(config: &Config, from: &Path) -> Result<String> {
    if std::os::unix::net::UnixStream::connect(&config.socket).is_ok() {
        bail!(
            "something is accepting connections on {}; stop the daemon first — state.db has a single writer",
            config.socket.display()
        );
    }
    let c = import_link_records(&config.state_db, from)?;
    Ok(format!(
        "read {}, created {}, updated {}, unchanged {}",
        c.read, c.created, c.updated, c.unchanged
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_and_missing_source() {
        let a = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(parse_from(Some(&a(&["--from", "x"]))).is_some());
        assert!(parse_from(Some(&a(&["--from"]))).is_none());
        assert_eq!(run(&a(&["link-records"])), 2);
        assert_eq!(run(&a(&["nope"])), 2);
        let dir = std::env::temp_dir().join(format!("cv-wp3-import-{}", std::process::id()));
        let err = import_link_records(&dir.join("state.db"), &dir.join("missing.sqlite"));
        assert!(err.is_err());
        assert!(
            !dir.join("state.db").exists(),
            "no state.db for a bad source"
        );
    }
}
