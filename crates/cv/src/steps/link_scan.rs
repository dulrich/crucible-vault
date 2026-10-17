//! `link_scan` (plan `v0-automation-pipelines` WP-3): extract URLs from notes and upsert
//! `link_records` in `state.db` (never writes a note).
//!
//! Port of Crucible `LinkScanWorkflow.run` (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/workflows/
//! LinkScanWorkflow.ts:38-115`) with the per-URL input of `applyLinkToRegistry`
//! (`utils/linkRegistry.ts:36-56`) and `normalizeExclusions`/`isExcluded` (`:69-90`); default
//! exclusions `["_crucible"]` (`src/types.ts:888`). Dropped as Obsidian-induced: the metadata-cache
//! lookup (frontmatter `type` comes from the index), progress reporting and the
//! `link-registry-changed` event. Not carried: the note-stub skip (plan WP-3).
//!
//! | key | values | default |
//! |---|---|---|
//! | `scope` | `"note"` (the run's active note) \| `"vault"` (every indexed markdown note) | `"note"` |
//! | `exclude` | folder prefixes (string or list) | `["_crucible"]` |
//!
//! Outcome vars (counts only): `scanned`, `records`, `created`, `updated`, `unchanged`,
//! `candidates`.

use std::collections::{BTreeMap, BTreeSet};

use cv_actions::{ConfigSet, Step};
use cv_index::Reader;
use cv_state::LinkRecordInput;
use cv_text::url_canon::CanonicalizedUrl;
use serde_json::{Value, json};

use super::materialize::{check_keys, get_str, get_str_list, now, table};
use crate::steps::{StepCtx, StepError, StepOutcome};

const KEYS: &[&str] = &["scope", "exclude"];
const DEFAULT_EXCLUDE: &[&str] = &["_crucible"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Note,
    Vault,
}

struct Params {
    scope: Scope,
    exclude: Vec<String>,
}

fn parse(step: &Step) -> Result<Params, String> {
    let t = table(step);
    check_keys(&t, KEYS, "link_scan")?;
    let scope = match get_str(&t, "scope")?.as_deref() {
        None | Some("note") => Scope::Note,
        Some("vault") => Scope::Vault,
        Some(s) => return Err(format!("link_scan step: `scope` {s:?} is not note|vault")),
    };
    let raw = if t.contains_key("exclude") {
        get_str_list(&t, "exclude")?
    } else {
        DEFAULT_EXCLUDE.iter().map(|s| (*s).to_owned()).collect()
    };
    Ok(Params {
        scope,
        exclude: normalize_exclusions(&raw),
    })
}

/// Crucible `normalizeExclusions`: trim, drop trailing slashes and empties, dedupe.
fn normalize_exclusions(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in raw {
        let t = item.trim().trim_end_matches('/');
        if !t.is_empty() && !out.iter().any(|o| o == t) {
            out.push(t.to_owned());
        }
    }
    out
}

/// Crucible `isExcluded`: `path` is an exclusion or a descendant of one.
fn is_excluded(path: &str, exclusions: &[String]) -> bool {
    exclusions.iter().any(|e| {
        path == e
            || path
                .strip_prefix(e.as_str())
                .is_some_and(|r| r.starts_with('/'))
    })
}

/// `[[path-without-.md]]`.
fn wikilink(path: &str) -> String {
    format!("[[{}]]", path.strip_suffix(".md").unwrap_or(path))
}

/// Validates the step's params (unknown keys disarm, I5).
pub fn validate(step: &Step, _config: &ConfigSet) -> Result<(), String> {
    parse(step).map(|_| ())
}

fn open_reader(ctx: &StepCtx<'_>) -> Result<Reader, StepError> {
    let open = || Reader::open_current(ctx.index_db);
    open()
        .or_else(|_| open())
        .map_err(|e| StepError::failed(format!("index: {e}")))
}

/// Executes the step.
pub fn execute(step: &Step, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let p = parse(step).map_err(StepError::failed)?;
    let reader = open_reader(ctx)?;
    let q = reader.query();
    let link_record_notes: BTreeSet<String> = q
        .files_with_prop("type", &json!("link-record"))
        .map_err(|e| StepError::failed(format!("index: {e}")))?
        .into_iter()
        .map(|r| r.path)
        .collect();
    let candidates: Vec<String> = match p.scope {
        Scope::Note => {
            let path = ctx.context.active_path.clone().ok_or_else(|| {
                StepError::failed("link_scan step: scope = note needs an active note")
            })?;
            // A folder-selector event also fires for attachments; their bytes (XMP etc.)
            // carry URLs that are not links, so only notes are scanned.
            if path.ends_with(".md") {
                vec![path]
            } else {
                Vec::new()
            }
        }
        Scope::Vault => q
            .file_paths()
            .map_err(|e| StepError::failed(format!("index: {e}")))?
            .into_iter()
            .map(|(_, path)| path)
            .filter(|path| path.ends_with(".md"))
            .collect(),
    };
    drop(reader);

    // Aggregate per canonical URL across notes first (Crucible `aggregate`).
    let mut aggregate: BTreeMap<String, (CanonicalizedUrl, Vec<String>)> = BTreeMap::new();
    let mut scanned = 0u64;
    for path in &candidates {
        if is_excluded(path, &p.exclude) || link_record_notes.contains(path) {
            continue;
        }
        let Some((bytes, _)) = ctx.read(path)? else {
            if p.scope == Scope::Note {
                return Err(StepError::failed("link_scan step: active note not found"));
            }
            continue;
        };
        scanned += 1;
        let text = String::from_utf8_lossy(&bytes);
        let link = wikilink(path);
        for canon in cv_ingest::urls::extract_canonical_urls(&text) {
            let entry = aggregate
                .entry(canon.canonical.clone())
                .or_insert_with(|| (canon, Vec::new()));
            if !entry.1.contains(&link) {
                entry.1.push(link.clone());
            }
        }
    }

    let today = now().date().format("%Y-%m-%d").to_string();
    let (mut created, mut updated, mut unchanged, mut flagged) = (0u64, 0u64, 0u64, 0u64);
    for (canon, notes) in aggregate.values() {
        let input = LinkRecordInput {
            canonical_url: canon.canonical.clone(),
            url: canon.url.clone(),
            domain: Some(canon.domain.clone()),
            source_notes: notes.clone(),
            first_seen: Some(today.clone()),
            last_seen: Some(today.clone()),
            state: Some("pending".into()),
            discovery_method: Some("scan".into()),
            tracked_source: Some(if canon.tracked_source.is_some() {
                json!("candidate")
            } else {
                Value::Bool(false)
            }),
            tracked_source_type: canon.tracked_source.as_ref().map(|h| h.kind.clone()),
            yt_video_id: canon.youtube_video_id.clone(),
            x_status_id: canon.x_status_id.clone(),
            ..LinkRecordInput::default()
        };
        // Hold the state lock per upsert only, never across note reads.
        let outcome = {
            let mut db = ctx
                .state
                .lock()
                .map_err(|_| StepError::failed("state.db lock poisoned"))?;
            db.upsert_link_record(&input)
                .map_err(|e| StepError::failed(format!("state.db: {e}")))?
        };
        match (outcome.created, outcome.changed) {
            (true, _) => created += 1,
            (false, true) => updated += 1,
            (false, false) => unchanged += 1,
        }
        if outcome.candidate_flagged {
            flagged += 1;
        }
    }
    tracing::info!(
        scanned,
        records = aggregate.len(),
        created,
        updated,
        unchanged,
        candidates = flagged,
        "link_scan"
    );
    let mut vars = serde_json::Map::new();
    vars.insert("scanned".into(), json!(scanned));
    vars.insert("records".into(), json!(aggregate.len()));
    vars.insert("created".into(), json!(created));
    vars.insert("updated".into(), json!(updated));
    vars.insert("unchanged".into(), json!(unchanged));
    vars.insert("candidates".into(), json!(flagged));
    Ok(StepOutcome {
        vars,
        ..StepOutcome::default()
    })
}

#[cfg(test)]
#[path = "link_scan_tests.rs"]
mod tests;
