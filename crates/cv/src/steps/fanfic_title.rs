//! Step module `fanfic_title` (plan `round-23-daemon-memory-mail-enrich-fanfic` WP-5): renames a
//! fanfic note to `<story> - Chapter <n>` through the writer's rename (inbound links follow), using
//! the pure [`cv_ingest::fanfic_title`] detector.
//!
//! | key | values | default |
//! |---|---|---|
//! | `scope` | `"note"` (one note) \| `"backfill"` (every indexed `fanfic`-tagged note) | `"note"` |
//! | `kind` | note scope: the filed kind (usually `"{param.kind}"`); anything but `fanfic` makes the step a no-op | — (always runs) |
//! | `path` | note scope: the note (placeholders ok; `{vars.moved_path}` after `move`) | the active note |
//! | `dry_run` | backfill only: count, write nothing; a job param `dry_run` wins | `true` |
//!
//! **Note scope** (in `file-as-kind`, after `move`): a note already in the target shape, an
//! undetectable one, or one whose target name exists is left as is and counted (`fanfic_already`,
//! `fanfic_undetected`, `fanfic_skipped_collision`; `fanfic_renamed` = 1 on a rename). A rename
//! sets `moved_path` / `moved_from` / `open_path` like `move`, so later steps (and the runner's
//! active-path follow) see the new path.
//!
//! **Backfill scope** walks the notes the index tags `fanfic` (outside system folders, in path
//! order) and applies the same rule. Outcome vars: `scanned`, `already`, `renamed` (real run),
//! `would_rename` (dry run), `collision`, `undetected`, `conflict`, `dry_run`. A second run renames
//! nothing (renamed notes are in the target shape).

use std::collections::BTreeSet;

use cv_actions::{ConfigSet, StepKind};
use cv_ingest::fanfic_title::{detect, is_target_shape, target_name};
use cv_note::FrontmatterDoc;
use cv_text::vpath;
use cv_vault::VaultError;
use serde::Serialize;

use super::file::target_path;
use super::materialize::join;
use super::params::{check_keys, check_placeholders, expand, get_bool, get_str};
use super::{StepCtx, StepError, StepModule, StepOutcome, job_flag};

/// The kind this step acts on.
const KIND: &str = "fanfic";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Note,
    Backfill,
}

/// Parsed step params.
#[derive(Debug, Clone)]
pub(crate) struct Params {
    scope: Scope,
    kind: Option<String>,
    path: Option<String>,
    dry_run: bool,
}

/// The `fanfic_title` step.
pub(crate) struct FanficTitle;

impl StepModule for FanficTitle {
    type Params = Params;

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<Params, String> {
        check_keys(t, kind, "fanfic_title")?;
        let scope = match get_str(t, "scope")?.as_deref() {
            None | Some("note") => Scope::Note,
            Some("backfill") => Scope::Backfill,
            Some(s) => {
                return Err(format!(
                    "fanfic_title step: `scope` {s:?} is not note|backfill"
                ));
            }
        };
        let (kind_p, path) = (get_str(t, "kind")?, get_str(t, "path")?);
        let dry_run = get_bool(t, "dry_run")?;
        if scope == Scope::Backfill && (kind_p.is_some() || path.is_some()) {
            return Err("fanfic_title step: `kind`/`path` need scope = \"note\"".into());
        }
        if scope == Scope::Note && dry_run.is_some() {
            return Err("fanfic_title step: `dry_run` needs scope = \"backfill\"".into());
        }
        for s in kind_p.iter().chain(path.iter()) {
            check_placeholders(s)?;
        }
        Ok(Params {
            scope,
            kind: kind_p,
            path,
            dry_run: dry_run.unwrap_or(true),
        })
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        match p.scope {
            Scope::Note => run_note(&p, ctx),
            Scope::Backfill => backfill(job_flag(ctx, "dry_run").unwrap_or(p.dry_run), ctx),
        }
    }
}

/// What the rule says about one note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Plan {
    /// Already `<story> - Chapter <n>`.
    Already,
    /// No shape detected.
    Undetected,
    /// Rename to this path (same folder).
    Rename(String),
}

/// The pure rule: `path` and the note's `text` → what to do.
pub(crate) fn plan(path: &str, text: &str) -> Plan {
    let title = vpath::stem(path);
    if is_target_shape(title) {
        return Plan::Already;
    }
    let doc = FrontmatterDoc::new(text);
    match detect(title, doc.body()) {
        None => Plan::Undetected,
        Some(t) => Plan::Rename(join(
            vpath::parent(path),
            &format!("{}.md", target_name(&t)),
        )),
    }
}

/// How one rename attempt ended.
enum Attempt {
    Renamed { rewritten: Vec<String> },
    Collision,
}

/// Renames `src` to `dst` through the writer (links rewritten); a taken target is a collision.
fn rename(ctx: &mut StepCtx<'_>, src: &str, dst: &str) -> Result<Attempt, StepError> {
    if ctx.read(dst)?.is_some() {
        return Ok(Attempt::Collision);
    }
    let Some((_, hash)) = ctx.read(src)? else {
        return Err(StepError::NoteGone {
            path: src.to_owned(),
        });
    };
    let (s, d) = (src.to_owned(), dst.to_owned());
    match ctx.with_writer(&[src, dst], move |vw| vw.rename(&s, &d, hash))? {
        Ok(r) => Ok(Attempt::Renamed {
            rewritten: r.rewritten,
        }),
        Err(VaultError::Exists(_)) => Ok(Attempt::Collision),
        Err(e) => Err(e.into()),
    }
}

/// Note-scope outcome vars.
#[derive(Serialize, Default)]
struct NoteVars {
    fanfic_renamed: u32,
    fanfic_skipped_collision: u32,
    fanfic_undetected: u32,
    fanfic_already: u32,
}

/// The vars of a rename (`moved_from` = [`super::MOVED_FROM`]).
#[derive(Serialize)]
struct MovedVars {
    moved_path: String,
    moved_from: String,
}

fn run_note(p: &Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let mut out = StepOutcome::default();
    if let Some(k) = &p.kind
        && expand(ctx, k)?.trim() != KIND
    {
        return Ok(out);
    }
    let src = target_path(ctx, p.path.as_deref())?;
    let (bytes, _) = ctx.read_required(&src)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let mut v = NoteVars::default();
    match plan(&src, &text) {
        Plan::Already => v.fanfic_already = 1,
        Plan::Undetected => v.fanfic_undetected = 1,
        Plan::Rename(dst) => match rename(ctx, &src, &dst)? {
            Attempt::Collision => v.fanfic_skipped_collision = 1,
            Attempt::Renamed { rewritten } => {
                v.fanfic_renamed = 1;
                out.set_vars(&MovedVars {
                    moved_path: dst.clone(),
                    moved_from: src,
                });
                out.open_path = Some(dst.clone());
                out.push_output(dst);
                for r in rewritten {
                    out.push_output(r);
                }
            }
        },
    }
    out.set_vars(&v);
    Ok(out)
}

#[derive(Serialize, Default)]
struct BackfillVars {
    scanned: u32,
    already: u32,
    renamed: u32,
    would_rename: u32,
    collision: u32,
    undetected: u32,
    conflict: u32,
    dry_run: bool,
}

fn backfill(dry_run: bool, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let paths: BTreeSet<String> = ctx
        .index()?
        .query()
        .tagged(KIND)?
        .into_iter()
        .map(|r| r.path)
        .filter(|p| p.ends_with(".md") && vpath::is_user_path(p))
        .collect();
    let mut v = BackfillVars {
        dry_run,
        ..BackfillVars::default()
    };
    let mut out = StepOutcome::default();
    for path in &paths {
        let Some((bytes, _)) = ctx.read(path)? else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        v.scanned += 1;
        match plan(path, &text) {
            Plan::Already => v.already += 1,
            Plan::Undetected => v.undetected += 1,
            Plan::Rename(dst) if dry_run => {
                if ctx.read(&dst)?.is_some() {
                    v.collision += 1;
                } else {
                    v.would_rename += 1;
                }
            }
            Plan::Rename(dst) => match rename(ctx, path, &dst) {
                Ok(Attempt::Collision) => v.collision += 1,
                Ok(Attempt::Renamed { rewritten }) => {
                    v.renamed += 1;
                    out.push_output(dst);
                    for r in rewritten {
                        out.push_output(r);
                    }
                }
                Err(e) if e.is_conflict() => v.conflict += 1,
                Err(e) => return Err(e),
            },
        }
    }
    out.set_vars(&v);
    Ok(out)
}
