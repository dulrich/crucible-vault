//! `cv intake pending [--days N] [--verbose]` (plan `intake-foundations` WP-1): for every intake
//! note created in the last N days (default 30), which stages are done and why the others are
//! not, judged by [`cv_ingest::stage::stage_states`].
//!
//! Daemon-free and read-only (the `cv removals export` shape): `index.db` (the current
//! generation), `state.db` and `search.db` are opened read-only and never migrated or created;
//! notes are read from the vault filesystem. A `state.db` without the v22 `stage_outcomes` table
//! (or none at all) has no recorded outcomes (and, when absent, no image records).
//!
//! **Population.** The intake classifier of `steps::intake_kind`: `post-id` → blog, `message-id`
//! → mail, a YouTube `source` → youtube, any other `source` → clip; markdown notes outside
//! `.trash/`, the index's skipped dirs and `_`/`.`-prefixed top folders, whose index `ctime`
//! (creation time, falling back to mtime) is inside the window.
//!
//! **Facts**, per note: remote image embeds (`cv media audit`'s count), the `_MD5` image embeds
//! and their `image_descriptions` records (a `failed` record is attempted), the `yt-metadata`,
//! `transcript-of`, `enriched` and `enrich-miss` properties, the `## Transcript` eligibility
//! (`cv_ingest::transcript`), the `refined` tag, the `yt_clip_tidy` preamble shape, the
//! tracker linkage (a row whose `note_path` is the note → linked; else a row whose `url` is the
//! note's `source`/`post-id`, as written or canonical, or a `youtube` row of its video id →
//! unlinked), the clipper inbox (top folder `Clippings`), and the `index` fact: the note
//! prepared exactly as the search feed prepares it, its hash compared with `search.db`'s
//! ([`cv_search::IndexedHashes`]).
//!
//! **Output.** Aggregate counts per stage and state; `--verbose` adds one line per note with an
//! unsettled stage (kind, path, and each unsettled stage's state).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};
use cv_index::{LinkKind, Reader};
use cv_ingest::stage::{
    ImageRecord, IntakeKind, NoteFacts, Outcome, OutcomeKind, Stage, State, TrackerLink,
    stage_states,
};
use cv_note::FrontmatterDoc;
use cv_search::IndexedHashes;
use cv_search::prepare::localized_image_md5;
use cv_state::{IntakeQuery, StateReader, TrackerItem};
use cv_text::url_canon::{canonicalize_url, extract_video_id_from_url};

use crate::config::Config;
use crate::steps::intake_kind::{self, Kind};
use crate::steps::search::NotePrep;

/// Usage line(s) for `cv --help` (part of [`crate::jobs::USAGE`]).
pub const USAGE: &str = "  cv intake pending [--days N] [--verbose]   read-only, daemon-free: intake notes of the last N days (default 30) by stage and state; --verbose lists each unfinished note\n";

/// The default window.
pub const DEFAULT_DAYS: u32 = 30;

/// The clipper inbox: a clip still here is not filed.
const CLIP_INBOX: &str = "Clippings";

const DAY_MS: i64 = 86_400_000;

/// Where the report reads from.
#[derive(Debug, Clone)]
pub struct Sources {
    pub vault: PathBuf,
    pub index_db: PathBuf,
    pub state_db: PathBuf,
    pub search_db: PathBuf,
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

fn kind_of(k: Kind) -> IntakeKind {
    match k {
        Kind::Blog => IntakeKind::Blog,
        Kind::Mail => IntakeKind::Mail,
        Kind::Youtube => IntakeKind::Youtube,
        Kind::Clip => IntakeKind::Clip,
    }
}

fn in_scope(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        && !cv_index::is_skipped_path(path)
        && path
            .split_once('/')
            .is_none_or(|(top, _)| !top.starts_with(['_', '.']))
        && !path.starts_with(['_', '.'])
}

/// The tracker rows, indexed for the linkage fact.
#[derive(Default)]
struct Tracker {
    linked: BTreeSet<String>,
    by_url: BTreeSet<String>,
    youtube_ids: BTreeSet<String>,
}

impl Tracker {
    fn new(rows: Vec<TrackerItem>) -> Self {
        let mut t = Self::default();
        for r in rows {
            if let Some(p) = r.note_path {
                t.linked.insert(p);
                continue;
            }
            if r.source_kind == "youtube" {
                t.youtube_ids.insert(r.item_id.clone());
            }
            if !r.url.is_empty() {
                if let Some(c) = canonicalize_url(&r.url) {
                    t.by_url.insert(c.canonical);
                }
                t.by_url.insert(r.url);
            }
        }
        t
    }

    fn link(&self, path: &str, urls: &[String], video_id: Option<&str>) -> TrackerLink {
        if self.linked.contains(path) {
            return TrackerLink::Linked;
        }
        let hit = urls.iter().any(|u| {
            self.by_url.contains(u)
                || canonicalize_url(u).is_some_and(|c| self.by_url.contains(&c.canonical))
        }) || video_id.is_some_and(|v| self.youtube_ids.contains(v));
        if hit {
            TrackerLink::Unlinked
        } else {
            TrackerLink::NoRow
        }
    }
}

fn text_prop(props: &[cv_index::PropRow], key: &str) -> Option<String> {
    props
        .iter()
        .find(|p| p.key == key)
        .filter(|p| !intake_kind::is_blank(&p.value))
        .map(|p| cv_index::prop_text(&p.value))
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
    let tracker = match &state {
        Some(s) => Tracker::new(s.tracker_items(&IntakeQuery::default())?),
        None => Tracker::default(),
    };

    let q = reader.query();
    let post: BTreeSet<String> = q.md_paths_with_prop_key("post-id")?.into_iter().collect();
    let mail: BTreeSet<String> = q
        .md_paths_with_prop_key("message-id")?
        .into_iter()
        .collect();
    let mut source: BTreeMap<String, String> = BTreeMap::new();
    for r in q.md_props_with_key("source")? {
        source.insert(r.path, cv_index::prop_text(&r.value));
    }
    let mut paths: BTreeSet<String> = post.union(&mail).cloned().collect();
    paths.extend(source.keys().cloned());
    let since = now_ms.saturating_sub(i64::from(days).saturating_mul(DAY_MS));

    let vault = src.vault.clone();
    let read = move |p: &str| -> std::io::Result<Option<Vec<u8>>> {
        match std::fs::read(vault.join(p)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    };
    let read_ref = &read;
    let state_ref = state.as_ref();
    let prep = NotePrep::new(
        Box::new(move |p| read_ref(p).map_err(|e| crate::steps::StepError::failed(e.to_string()))),
        &reader,
        Box::new(move |md5| {
            state_ref
                .and_then(|s| s.image_descriptions_for(&[md5.to_owned()]).ok())
                .and_then(|mut v| v.pop())
        }),
    );

    let mut notes = 0_usize;
    let mut by_kind: BTreeMap<IntakeKind, usize> = BTreeMap::new();
    let mut counts: BTreeMap<(Stage, &'static str), usize> = BTreeMap::new();
    let mut lines = Vec::new();
    for path in paths.iter().filter(|p| in_scope(p)) {
        let Some(file) = q.file(path)? else { continue };
        if file.ctime < since {
            continue;
        }
        let Some(bytes) = read(path).with_context(|| format!("reading {path}"))? else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let src_url = source.get(path).cloned().unwrap_or_default();
        let kind = kind_of(intake_kind::classify(
            post.contains(path),
            mail.contains(path),
            &src_url,
        ));
        let props = q.props(path)?;
        let facts = facts(&FactInput {
            path,
            kind,
            text: &text,
            props: &props,
            links: &q.outgoing(path)?,
            state: state.as_ref(),
            tracker: &tracker,
            index_current: prep
                .local_note(path)
                .map_err(|e| anyhow::anyhow!("preparing {path}: {e}"))?
                .is_some_and(|n| indexed.is_current(&n.prepared)),
        })?;
        let outcomes = match (&state, state.as_ref().map(|s| s.note_id_at(path))) {
            (Some(s), Some(Ok(Some(id)))) => parse_outcomes(&s.stage_outcomes_for(&[id])?),
            (_, Some(Err(e))) => return Err(e.into()),
            _ => Vec::new(),
        };
        let states = stage_states(&facts, &outcomes);
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
    Ok(render(notes, days, &by_kind, &counts, &lines))
}

struct FactInput<'a> {
    path: &'a str,
    kind: IntakeKind,
    text: &'a str,
    props: &'a [cv_index::PropRow],
    links: &'a [cv_index::LinkRow],
    state: Option<&'a StateReader>,
    tracker: &'a Tracker,
    index_current: bool,
}

fn facts(i: &FactInput<'_>) -> Result<NoteFacts> {
    let mut f = NoteFacts::new(i.kind);
    let body = FrontmatterDoc::new(i.text).body().to_owned();
    f.awaiting_filing = matches!(i.kind, IntakeKind::Clip | IntakeKind::Youtube)
        && i.path
            .split_once('/')
            .is_some_and(|(top, _)| top == CLIP_INBOX);
    f.remote_images = usize::try_from(crate::media_audit::note_counts(i.text).0).unwrap_or(0);
    let mut md5s = BTreeSet::new();
    for l in i.links {
        if !matches!(l.kind, LinkKind::Embed | LinkKind::MdImage) || l.external {
            continue;
        }
        let target = l.resolved_path.as_deref().unwrap_or(&l.target);
        if crate::media_audit::is_image_file(target)
            && let Some((md5, _)) = localized_image_md5(target)
        {
            md5s.insert(md5);
        }
    }
    let md5s: Vec<String> = md5s.into_iter().collect();
    let records = match i.state {
        Some(s) if !md5s.is_empty() => s.image_descriptions_for(&md5s)?,
        _ => Vec::new(),
    };
    f.images = md5s
        .iter()
        .map(|m| {
            let r = match records.iter().find(|r| &r.md5 == m) {
                None => ImageRecord::Missing,
                Some(r) if r.kind == "failed" => ImageRecord::Failed,
                Some(_) => ImageRecord::Described,
            };
            (m.clone(), r)
        })
        .collect();
    f.yt_metadata_linked = text_prop(i.props, "yt-metadata").is_some();
    f.youtube_preamble = crate::steps::yt_clip_tidy::tidy(i.text).is_some();
    f.transcript_of = text_prop(i.props, "transcript-of").is_some();
    f.transcript_words = match cv_ingest::transcript::eligibility(&body) {
        cv_ingest::transcript::Eligibility::NoSection => None,
        e => Some(e.words()),
    };
    f.refined = crate::steps::transcript_refine::is_refined(i.text);
    f.enriched = text_prop(i.props, "enriched").is_some();
    f.enrich_miss = text_prop(i.props, "enrich-miss")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let source = text_prop(i.props, "source");
    let urls: Vec<String> = source
        .iter()
        .chain(text_prop(i.props, "post-id").iter())
        .cloned()
        .collect();
    let video = text_prop(i.props, "yt-video-id")
        .or_else(|| source.as_deref().and_then(extract_video_id_from_url));
    f.tracker = i.tracker.link(i.path, &urls, video.as_deref());
    f.index_current = i.index_current;
    Ok(f)
}

/// Parses stored rows; an unknown stage or outcome kind is skipped (written by a newer build).
fn parse_outcomes(rows: &[cv_state::StageOutcomeRow]) -> Vec<Outcome> {
    rows.iter()
        .filter_map(|r| {
            Some(Outcome {
                stage: Stage::parse(&r.stage)?,
                kind: OutcomeKind::parse(&r.outcome)?,
                reason: r.reason.clone(),
                attempts: r.attempts,
                fingerprint: r.fingerprint.clone(),
            })
        })
        .collect()
}

fn describe(s: &State) -> String {
    match s {
        State::Skipped(r) => format!("skipped({r})"),
        State::Failed(n) => format!("failed({n})"),
        other => other.as_str().to_owned(),
    }
}

const STATES: [&str; 6] = [
    "done",
    "pending",
    "skipped",
    "unfinished",
    "failed",
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
