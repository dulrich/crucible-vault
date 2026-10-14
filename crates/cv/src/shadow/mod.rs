//! `cv shadow …` (plan `v0-core-daemon` WP-14): the soak tooling.
//!
//! - `report` — aggregate would-be writes from `state.db` `shadow_writes` ([`report`]);
//! - `parity` — evaluates the plan's shadow parity criteria P1–P7 ([`parity`]);
//! - `replay` — runs one action against a user scratch copy of the vault ([`replay`]);
//! - `import-descriptions` — one-time import of Crucible's image descriptions ([`import`]).
//!
//! `report` and `parity` print aggregate JSON only (counts, rates, durations, sizes, verdicts —
//! never a note path, title, frontmatter value or note text) and never write: `state.db` is
//! opened through [`cv_state::StateReader`], the companion through
//! [`cv_search::CompanionReader`].
//!
//! Crucible reference: Crucible has no shadow mode — the closest analogue is its search
//! reconcile dry-run (`https://github.com/dulrich/obsidian-crucible/blob/master/src/search/SearchIndexWorkflow.ts`, the
//! `/v1/paths` hash comparison) which the P3 criterion reuses through `cv_search::compare`. The
//! Obsidian-induced part (driving the comparison from `metadataCache` inside the plugin) is
//! dropped: hashes come from `index.db` and direct reads.

pub mod import;
pub mod parity;
pub mod probe;
pub mod replay;
pub mod report;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value};

use crate::config::Config;

/// `cv shadow` usage.
pub const USAGE: &str = "usage:\n  cv shadow report [--since <rfc3339|ms>]\n  cv shadow parity [--soak-start <rfc3339|ms>] [--skip p1,p3,...]\n  cv shadow replay <action-id> --corpus <dir> [--param k=v ...] [--active <path>]\n  cv shadow import-descriptions <dir>\n";

/// Runs `cv shadow <args>`; returns the exit code (0 ok, 1 harness error, 2 usage).
pub fn run(args: &[String]) -> i32 {
    let r = match args.first().map(String::as_str) {
        Some("report") => cmd_report(&args[1..]),
        Some("parity") => cmd_parity(&args[1..]),
        Some("replay") => cmd_replay(&args[1..]),
        Some("import-descriptions") if args.len() == 2 => cmd_import(&args[1]),
        _ => {
            eprint!("{USAGE}");
            return 2;
        }
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("cv shadow: {e:#}");
            1
        }
    }
}

/// Parses an RFC 3339 timestamp or integer epoch milliseconds.
pub fn parse_time_ms(s: &str) -> Result<i64> {
    if let Ok(ms) = s.trim().parse::<i64>() {
        return Ok(ms);
    }
    chrono::DateTime::parse_from_rfc3339(s.trim())
        .map(|d| d.timestamp_millis())
        .map_err(|e| anyhow!("{s:?} is neither RFC 3339 nor epoch ms: {e}"))
}

/// `DEFAULT_REPO_ROOTS` as paths.
pub fn default_repo_roots() -> Vec<PathBuf> {
    cv_vault::DEFAULT_REPO_ROOTS
        .iter()
        .map(PathBuf::from)
        .collect()
}

fn value_of<'a>(args: &'a [String], i: &mut usize, flag: &str) -> Result<&'a str> {
    *i += 1;
    args.get(*i)
        .map(String::as_str)
        .ok_or_else(|| anyhow!("{flag} needs a value"))
}

fn cmd_report(args: &[String]) -> Result<()> {
    let mut since = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--since" => since = Some(parse_time_ms(value_of(args, &mut i, "--since")?)?),
            other => bail!("unknown argument {other:?}\n{USAGE}"),
        }
        i += 1;
    }
    let config = Config::from_env()?;
    let reader = cv_state::StateReader::open(&config.state_db)
        .with_context(|| format!("opening {} read-only", config.state_db.display()))?;
    let r = report::build(&reader, since)?;
    println!("{}", serde_json::to_string_pretty(&r.to_json())?);
    Ok(())
}

fn cmd_parity(args: &[String]) -> Result<()> {
    let mut soak_start = None;
    let mut skip = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--soak-start" => {
                soak_start = Some(parse_time_ms(value_of(args, &mut i, "--soak-start")?)?);
            }
            "--skip" => {
                skip.extend(
                    value_of(args, &mut i, "--skip")?
                        .split(',')
                        .map(|s| s.trim().to_ascii_uppercase())
                        .filter(|s| !s.is_empty()),
                );
            }
            other => bail!("unknown argument {other:?}\n{USAGE}"),
        }
        i += 1;
    }
    let config = Config::from_env()?;
    let env_or = |k: &str, d: &str| {
        std::env::var(k)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| d.to_owned())
    };
    let opts = parity::ParityOptions {
        search_url: env_or("CV_SEARCH_URL", cv_search::DEFAULT_COMPANION_URL),
        vault_id: env_or("CV_SEARCH_VAULT_ID", crate::steps::search::DEFAULT_VAULT_ID),
        soak_start_ms: soak_start,
        skip,
        repo_roots: default_repo_roots(),
        now_ms: crate::now_ms(),
    };
    let out = parity::run(&config, &opts, &probe::SystemdProbe)?;
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn cmd_replay(args: &[String]) -> Result<()> {
    let action = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .ok_or_else(|| anyhow!("replay needs an action id\n{USAGE}"))?
        .clone();
    let mut corpus = None;
    let mut active = None;
    let mut params = Map::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus" => corpus = Some(PathBuf::from(value_of(args, &mut i, "--corpus")?)),
            "--active" => active = Some(value_of(args, &mut i, "--active")?.to_owned()),
            "--param" => {
                let kv = value_of(args, &mut i, "--param")?;
                let (k, v) = kv
                    .split_once('=')
                    .ok_or_else(|| anyhow!("--param needs k=v, got {kv:?}"))?;
                params.insert(k.to_owned(), Value::String(v.to_owned()));
            }
            other => bail!("unknown argument {other:?}\n{USAGE}"),
        }
        i += 1;
    }
    let corpus = corpus.ok_or_else(|| anyhow!("replay needs --corpus <dir>"))?;
    let config = Config::from_env()?;
    let req = replay::ReplayRequest {
        action_id: action,
        corpus,
        params,
        active_path: active,
        repo_roots: default_repo_roots(),
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building the tokio runtime")?;
    let out = rt.block_on(replay::run(&config, &req))?;
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn cmd_import(dir: &str) -> Result<()> {
    let config = Config::from_env()?;
    let s = import::run(&config, std::path::Path::new(dir))?;
    println!("{}", serde_json::to_string_pretty(&s)?);
    Ok(())
}
