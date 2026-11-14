//! `intake_frontmatter_backfill` (plan `sources-publication-model` WP-4): fills two frontmatter
//! keys on existing intake notes, run through the `intake-frontmatter-backfill` action.
//!
//! | key | values | default |
//! |---|---|---|
//! | `dry_run` | count every outcome, write nothing; a job param `dry_run` wins | `false` |
//!
//! A **candidate** is a note under the day folder (`captures.toml` `[periods.day].folder`, as the
//! views resolve it) whose frontmatter has `post-id` or `message-id`. For each candidate:
//!
//! - **`read`**: no `read` key at all gets `read: false` (no cut-off); an existing `read`, true or
//!   false, is never touched.
//! - **`publication`**: a blank or absent one gets the resolver's answer for the note's
//!   `source` and `author` ([`cv_actions::pipelines::SourcesConfig::by_note`]), else the
//!   publication of the source row of a tracker row pointing at the note
//!   ([`super::reconcile_repair::publication_of_row`]); when neither answers it stays absent. A
//!   non-blank `publication` is never overwritten.
//!
//! Both edits of one note are one CAS write against the hash read; a conflict is skipped and
//! counted, never retried. Outcome vars are aggregates only: `candidates`, `read_set`,
//! `publication_set` (notes that got the key), `unchanged` (notes needing no edit), `conflicts`
//! and `dry_run`. A dry run computes the same counts and writes nothing; a second real run
//! changes nothing.

use std::collections::{BTreeMap, BTreeSet};

use cv_actions::{ConfigSet, StepKind};
use cv_note::{FrontmatterDoc, JsValue};
use cv_state::TrackerItem;
use serde::Serialize;

use super::params::{check_keys, get_bool, job_dry_run};
use super::reconcile_repair::publication_of_row;
use super::{StepCtx, StepError, StepModule, StepOutcome};

/// The `intake_frontmatter_backfill` step.
pub(crate) struct IntakeFrontmatterBackfill;

impl StepModule for IntakeFrontmatterBackfill {
    type Params = bool;

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<bool, String> {
        check_keys(t, kind, "intake_frontmatter_backfill")?;
        Ok(get_bool(t, "dry_run")?.unwrap_or(false))
    }

    fn run(dry_run: bool, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run(job_dry_run(ctx, dry_run), ctx)
    }
}

/// Outcome vars (aggregates only).
#[derive(Default, Serialize)]
struct Vars {
    candidates: u32,
    read_set: u32,
    publication_set: u32,
    unchanged: u32,
    conflicts: u32,
    dry_run: bool,
}

/// What one note needs.
#[derive(Debug, PartialEq, Eq)]
struct Edit {
    /// Set `read: false`.
    read: bool,
    /// Set `publication` to this.
    publication: Option<String>,
}

/// The edit `doc` needs (pure): `read: false` when the key is absent; `publication` from
/// `resolved` (computed lazily) when blank. `None` when nothing is needed.
fn plan(
    doc: &FrontmatterDoc,
    resolved: impl FnOnce(&FrontmatterDoc) -> Option<String>,
) -> Option<Edit> {
    let read = matches!(doc.get("read"), Ok(None));
    let publication = if crate::supersede::publication_blank(doc) {
        resolved(doc)
    } else {
        None
    };
    (read || publication.is_some()).then_some(Edit { read, publication })
}

/// Applies `e` to `doc`; `Ok(true)` when the text changed.
fn apply(doc: &mut FrontmatterDoc, e: &Edit) -> Result<bool, cv_note::FmError> {
    let mut changed = false;
    if e.read {
        changed |= doc.set("read", &JsValue::Bool(false))?;
    }
    if let Some(p) = &e.publication {
        changed |= doc.insert_after("author", "publication", &JsValue::Str(p.clone()))?;
    }
    Ok(changed)
}

/// The publication of a note: its `source`/`author` through the resolver, else its tracker rows'.
fn publication_for(
    ctx: &StepCtx<'_>,
    doc: &FrontmatterDoc,
    rows: Option<&Vec<TrackerItem>>,
) -> Option<String> {
    let authors = doc.get_texts("author");
    let authors: Vec<&str> = authors.iter().map(String::as_str).collect();
    let source = doc
        .get("source")
        .ok()
        .flatten()
        .map(|v| cv_note::js_text(&v))
        .unwrap_or_default();
    ctx.config
        .sources()
        .by_note(&authors, &source)
        .map(|p| p.publication().to_owned())
        .or_else(|| rows.and_then(|rs| rs.iter().find_map(|r| publication_of_row(ctx, r))))
}

/// The candidate paths: day-folder notes with `post-id` or `message-id`.
fn candidates(ctx: &StepCtx<'_>) -> Result<BTreeSet<String>, StepError> {
    let reader = ctx.index()?;
    let q = reader.query();
    let day: BTreeSet<String> = q
        .md_files_under(&ctx.config.day_folder())?
        .into_iter()
        .map(|f| f.path)
        .collect();
    let mut out = BTreeSet::new();
    for key in ["post-id", "message-id"] {
        out.extend(
            q.md_paths_with_prop_key(key)?
                .into_iter()
                .filter(|p| day.contains(p)),
        );
    }
    Ok(out)
}

fn run(dry: bool, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let mut v = Vars {
        dry_run: dry,
        ..Vars::default()
    };
    let mut by_note: BTreeMap<String, Vec<TrackerItem>> = BTreeMap::new();
    for r in ctx.state().tracker_items_with_note()? {
        if let Some(p) = r.note_path.clone() {
            by_note.entry(p).or_default().push(r);
        }
    }
    for path in candidates(ctx)? {
        v.candidates += 1;
        let Some((bytes, hash)) = ctx.read(&path)? else {
            v.unchanged += 1;
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            v.unchanged += 1;
            continue;
        };
        let mut doc = FrontmatterDoc::new(text.as_str());
        let rows = by_note.get(&path);
        let Some(edit) = plan(&doc, |d| publication_for(ctx, d, rows)) else {
            v.unchanged += 1;
            continue;
        };
        if !apply(&mut doc, &edit)? {
            v.unchanged += 1;
            continue;
        }
        if !dry {
            match ctx.write(&path, doc.into_text().into_bytes(), Some(hash)) {
                Ok(_) => {}
                Err(e) if e.is_conflict() => {
                    v.conflicts += 1;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        v.read_set += u32::from(edit.read);
        v.publication_set += u32::from(edit.publication.is_some());
    }
    let mut out = StepOutcome::default();
    out.set_vars(&v);
    Ok(out)
}
