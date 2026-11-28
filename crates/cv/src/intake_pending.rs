//! `cv intake pending [--days N] [--verbose]` (plan `intake-foundations` WP-1): for every intake
//! note created in the last N days (default 30), which stages are done and why the others are
//! not, judged by [`cv_ingest::stage::stage_states`] through the shared facts builder
//! ([`crate::note_facts`], plan `intake-stage-jobs` WP-2) under [`note_facts::ATTEMPT_CAP`].
//!
//! Daemon-free and read-only (the `cv removals export` shape): `index.db` (the current
//! generation), `state.db` and `search.db` are opened read-only and never migrated or created;
//! notes are read from the vault filesystem and the config from `CV_CONFIG_DIR`. A `state.db` without the v22 `stage_outcomes` table
//! (or none at all) has no recorded outcomes (and, when absent, no image records).
//!
//! **Population.** The intake classifier of `steps::intake_kind`: `post-id` → blog, `message-id`
//! → mail, a YouTube `source` → youtube, any other `source` → clip; markdown notes outside
//! `.trash/`, the index's skipped dirs and `_`/`.`-prefixed top folders, whose creation
//! date is inside the window: the frontmatter `created` date (`YYYY-MM-DD` as UTC midnight, or an
//! ISO date-time), else the index `ctime` (a bulk rewrite resets `ctime`, so it is the fallback).
//! The first output line names the vault root read.
//!
//! **Facts**: [`crate::note_facts`] (the one builder the runner's stage guard reads too).
//!
//! **Output.** Aggregate counts per stage and state; `--verbose` adds one line per note with an
//! unsettled stage (kind, path, and each unsettled stage's state).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};
use cv_actions::ConfigSet;
use cv_index::Reader;
use cv_ingest::stage::{IntakeKind, Stage, State};
use cv_search::IndexedHashes;
use cv_state::StateReader;

use crate::config::Config;
use crate::note_facts::{self, ATTEMPT_CAP, FactEnv, text_prop};

/// Usage line(s) for `cv --help` (part of [`crate::jobs::USAGE`]).
pub const USAGE: &str = "  cv intake pending [--days N] [--verbose]   read-only, daemon-free: intake notes of the last N days (default 30) by stage and state; --verbose lists each unfinished note\n";

/// The default window.
pub const DEFAULT_DAYS: u32 = 30;

const DAY_MS: i64 = 86_400_000;

/// The frontmatter key holding a note's creation date (the `lint` step's `created_key` default).
const CREATED_KEY: &str = "created";

/// A creation date as epoch milliseconds: `YYYY-MM-DD` (UTC midnight) optionally followed by
/// `T`/space and `HH:MM[:SS[.fff]]` and `Z` or `±HH[:MM]` (no offset is UTC). `None` for anything
/// else (the caller falls back to the index `ctime`).
fn parse_created_ms(v: &str) -> Option<i64> {
    let v = v.trim().trim_matches(['"', '\'']);
    let num = |s: &str, n: usize| -> Option<i64> {
        (s.len() == n && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    let date = v.get(..10)?;
    let (y, m, d) = (
        num(&date[..4], 4)?,
        num(&date[5..7], 2)?,
        num(&date[8..], 2)?,
    );
    if date.as_bytes()[4] != b'-'
        || date.as_bytes()[7] != b'-'
        || !(1..=12).contains(&m)
        || !(1..=31).contains(&d)
    {
        return None;
    }
    let mut ms = cv_text::days_from_civil(y, m, d) * DAY_MS;
    let rest = &v[10..];
    if rest.is_empty() {
        return Some(ms);
    }
    let rest = rest.strip_prefix(['T', ' '])?;
    let t = rest.get(..5)?;
    let (hh, mm) = (num(&t[..2], 2)?, num(&t[3..], 2)?);
    if t.as_bytes()[2] != b':' || hh > 23 || mm > 59 {
        return None;
    }
    ms += (hh * 60 + mm) * 60_000;
    let mut tail = &rest[5..];
    if let Some(sec) = tail.strip_prefix(':') {
        let s = num(sec.get(..2)?, 2)?;
        if s > 60 {
            return None;
        }
        ms += s * 1000;
        tail = &sec[2..];
        if let Some(frac) = tail.strip_prefix('.') {
            let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return None;
            }
            tail = &frac[digits..];
        }
    }
    match tail {
        "" | "Z" | "z" => Some(ms),
        _ => {
            let sign = match tail.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let off = &tail[1..];
            let (oh, om) = match off.len() {
                2 => (num(off, 2)?, 0),
                4 => (num(&off[..2], 2)?, num(&off[2..], 2)?),
                5 if off.as_bytes()[2] == b':' => (num(&off[..2], 2)?, num(&off[3..], 2)?),
                _ => return None,
            };
            Some(ms - sign * (oh * 60 + om) * 60_000)
        }
    }
}

/// Where the report reads from.
#[derive(Debug, Clone)]
pub struct Sources {
    pub vault: PathBuf,
    pub index_db: PathBuf,
    pub state_db: PathBuf,
    pub search_db: PathBuf,
    /// `CV_CONFIG_DIR` (`auto_file`'s kind rules, the finalize decision, the channel step); a
    /// missing directory is an empty config.
    pub config_dir: PathBuf,
}

/// `cv intake pending …` with parsed options; returns the exit code.
pub fn run(days: u32, verbose: bool) -> i32 {
    // The socket is unused; a missing XDG_RUNTIME_DIR must not stop a read-only report.
    let r = Config::resolve(|k| {
        std::env::var(k)
            .ok()
            .or_else(|| (k == "CV_SOCKET").then(|| "/nonexistent".to_owned()))
    })
    .and_then(|c| {
        let src = Sources {
            vault: c.vault,
            index_db: c.index_db,
            state_db: c.state_db,
            search_db: c.search_db,
            config_dir: c.config_dir,
        };
        report(&src, crate::now_ms(), days, verbose)
    });
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

/// Builds the report text at clock `now_ms`.
pub fn report(src: &Sources, now_ms: i64, days: u32, verbose: bool) -> Result<String> {
    let reader = Reader::open_current(&src.index_db)
        .with_context(|| format!("opening the index at {}", src.index_db.display()))?;
    let state = if src.state_db.is_file() {
        Some(
            StateReader::open(&src.state_db)
                .with_context(|| format!("opening {}", src.state_db.display()))?,
        )
    } else {
        None
    };
    let indexed = IndexedHashes::open_read_only(&src.search_db)
        .with_context(|| format!("opening {}", src.search_db.display()))?;
    let mut config = ConfigSet::new();
    if src.config_dir.is_dir() {
        crate::runner::load_config_dir(&src.config_dir, &mut config);
    }
    let env = FactEnv {
        vault: &src.vault,
        config: &config,
        reader: &reader,
        state: state.as_ref().map(|s| s as &dyn cv_state::IntakeRead),
        indexed: Some(&indexed),
    };

    let q = reader.query();
    let since = now_ms.saturating_sub(i64::from(days).saturating_mul(DAY_MS));
    let mut notes = 0_usize;
    let mut by_kind: BTreeMap<IntakeKind, usize> = BTreeMap::new();
    let mut counts: BTreeMap<(Stage, &'static str), usize> = BTreeMap::new();
    let mut lines = Vec::new();
    for path in note_facts::candidates(&reader)? {
        let Some(file) = q.file(&path)? else { continue };
        let props = q.props(&path)?;
        let created = text_prop(&props, CREATED_KEY)
            .and_then(|v| parse_created_ms(&v))
            .unwrap_or(file.ctime);
        if created < since {
            continue;
        }
        let Some(view) = note_facts::note_view(&env, &path)? else {
            continue;
        };
        let kind = view.kind();
        let states = view.states(ATTEMPT_CAP);
        notes += 1;
        *by_kind.entry(kind).or_default() += 1;
        for (stage, st) in &states {
            *counts.entry((*stage, st.as_str())).or_default() += 1;
        }
        if verbose {
            let open: Vec<String> = states
                .iter()
                .filter(|(_, s)| !s.is_settled())
                .map(|(stage, s)| format!("{}={}", stage.as_str(), describe(s)))
                .collect();
            if !open.is_empty() {
                lines.push(format!("{} {path}: {}", kind.as_str(), open.join(" ")));
            }
        }
    }
    let mut out = format!("vault: {}\n", src.vault.display());
    out.push_str(&render(notes, days, &by_kind, &counts, &lines));
    Ok(out)
}

fn describe(s: &State) -> String {
    match s {
        State::Skipped(r) => format!("skipped({r})"),
        State::Failed(n) => format!("failed({n})"),
        State::Exhausted(n) => format!("exhausted({n})"),
        other => other.as_str().to_owned(),
    }
}

const STATES: [&str; 7] = [
    "done",
    "pending",
    "skipped",
    "unfinished",
    "failed",
    "exhausted",
    "stuck",
];
fn render(
    notes: usize,
    days: u32,
    by_kind: &BTreeMap<IntakeKind, usize>,
    counts: &BTreeMap<(Stage, &'static str), usize>,
    lines: &[String],
) -> String {
    let mut out = format!("intake notes (last {days} days): {notes}");
    for k in IntakeKind::ALL {
        let _ = write!(
            out,
            " {}={}",
            k.as_str(),
            by_kind.get(&k).copied().unwrap_or(0)
        );
    }
    out.push('\n');
    let _ = write!(out, "{:<18}", "stage");
    for s in STATES {
        let _ = write!(out, " {s:>10}");
    }
    out.push('\n');
    for stage in Stage::ALL {
        let _ = write!(out, "{:<18}", stage.as_str());
        for s in STATES {
            let n = counts.get(&(stage, s)).copied().unwrap_or(0);
            let _ = write!(out, " {n:>10}");
        }
        out.push('\n');
    }
    for l in lines {
        out.push_str(l);
        out.push('\n');
    }
    out
}

#[cfg(test)]
#[path = "intake_pending_tests.rs"]
mod tests;
