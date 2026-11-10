//! Step module `fanfic_title` (plan `round-23-daemon-memory-mail-enrich-fanfic` WP-5): renames a
//! serial-fiction chapter note (the configured kind, e.g. `fanfic`) to `<story> - Chapter <n>` through the writer's rename (inbound links follow), using
//! the pure [`cv_ingest::fanfic_title`] detector.
//!
//! | key | values | default |
//! |---|---|---|
//! | `scope` | `"note"` (one note) \| `"backfill"` (every indexed note tagged `kind`) | `"note"` |
//! | `match_kind` | note scope, **required**: the kind this step renames (e.g. `"fanfic"`) | — |
//! | `kind` | note scope: the filed kind (usually `"{param.kind}"`); any kind but `match_kind` makes the step a no-op; backfill, **required**: the kind (tag) whose notes are renamed | note: — (always runs) |
//! | `path` | note scope: the note (placeholders ok; `{vars.moved_path}` after `move`) | the active note |
//! | `dry_run` | backfill only: count, rename nothing (writes only the review note below); a job param `dry_run` wins | `true` |
//!
//! **Note scope** (in `file-as-kind`, after `move`): a note already in the target shape, an
//! undetectable one, or one whose target name exists is left as is and counted (`fanfic_already`,
//! `fanfic_undetected`, `fanfic_skipped_collision`; `fanfic_renamed` = 1 on a rename). A rename also sets
//! the note's frontmatter `title` to the new name, and a note already named right whose `title`
//! differs has it set (`fanfic_retitled` = 1); a note without a `title` is not given one. Counter names keep
//! their `fanfic_` prefix whatever the kind (telemetry continuity, plan public-release WP-4). A rename
//! sets `moved_path` / `moved_from` / `open_path` like `move`, so later steps (and the runner's
//! active-path follow) see the new path.
//!
//! **Backfill scope** walks the notes the index tags `kind` (outside system folders, in path
//! order) and applies the same rule, chain-safe (WP-8): every note is planned first, then
//! [`order_renames`] withholds duplicate targets, cycles and targets held by a note that does not
//! move (all counted as `collision`) and runs the rest holder-first, so a down-shifted run of
//! chapters renames cleanly. The target's story is the matched `[[serial]]` row's publication,
//! else the note's `publication` frontmatter, else the title's; a note is `already` only when its
//! name equals the target. Outcome vars: `scanned`, `already`, `renamed` (real run),
//! `would_rename` (dry run), `retitled` / `would_retitle` (already named right, stale `title`), `collision`, `undetected`, `conflict`, `dry_run`. A second run renames
//! nothing (renamed notes are in the target shape).
//!
//! A **dry run** also writes the review note `<app_folder>/reviews/<kind>-titles-<YYYY-MM-DD>.md`
//! (overwritten when it exists: it holds no user input) listing what an apply run would rename,
//! the collisions and the undetected notes, and sets outcome vars `review_path` and the outcome's
//! `open_path`. A real run writes no note.

use std::collections::{BTreeSet, HashSet};

use cv_actions::pipelines::SerialSource;
use cv_actions::{ConfigSet, StepKind};
use cv_ingest::fanfic_title::{detect_with, target_name};
use cv_note::{FrontmatterDoc, JsValue};
use cv_text::vpath;
use cv_vault::VaultError;
use serde::Serialize;

use super::auto_file::{clip_of, serial_hit};
use super::file::{kind_def, target_path};
use super::materialize::{join, now};
use super::params::{check_keys, check_placeholders, expand, get_bool, get_str};
use super::{StepCtx, StepError, StepModule, StepOutcome, job_flag};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Note,
    Backfill,
}

/// Parsed step params.
#[derive(Debug, Clone)]
pub(crate) struct Params {
    scope: Scope,
    /// Note scope: the filed kind (placeholders ok); `None` = always runs.
    kind: Option<String>,
    /// The kind this step renames (plan public-release WP-4, C#30): note scope `match_kind`,
    /// backfill `kind`. Required; there is no built-in kind.
    target: String,
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
        let match_kind = get_str(t, "match_kind")?;
        let dry_run = get_bool(t, "dry_run")?;
        if scope == Scope::Backfill && (match_kind.is_some() || path.is_some()) {
            return Err("fanfic_title step: `match_kind`/`path` need scope = \"note\"".into());
        }
        if scope == Scope::Note && dry_run.is_some() {
            return Err("fanfic_title step: `dry_run` needs scope = \"backfill\"".into());
        }
        let (target, kind_p) = match scope {
            Scope::Note => (
                match_kind.ok_or("fanfic_title step: scope = \"note\" needs `match_kind`")?,
                kind_p,
            ),
            Scope::Backfill => (
                kind_p.ok_or("fanfic_title step: scope = \"backfill\" needs `kind`")?,
                None,
            ),
        };
        let target = target.trim().to_owned();
        if target.is_empty() || target.contains('{') {
            return Err("fanfic_title step: the kind to rename must be a plain kind name".into());
        }
        for s in kind_p.iter().chain(path.iter()) {
            check_placeholders(s)?;
        }
        Ok(Params {
            scope,
            kind: kind_p,
            target,
            path,
            dry_run: dry_run.unwrap_or(true),
        })
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        match p.scope {
            Scope::Note => run_note(&p, ctx),
            Scope::Backfill => backfill(
                &p.target,
                job_flag(ctx, "dry_run").unwrap_or(p.dry_run),
                ctx,
            ),
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

/// The story publication of a note (WP-8): the matched `[[serial]]` row's `publication()`
/// (from the note's `source`/`author`), else its `publication` frontmatter text, else none.
fn story_publication(
    doc: &FrontmatterDoc,
    serial: &[SerialSource],
    is_kind: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let nonempty = |s: &str| Some(s.trim().to_owned()).filter(|s| !s.is_empty());
    if let Some(row) = serial_hit(serial, &clip_of(doc), is_kind) {
        return nonempty(row.publication());
    }
    doc.get("publication")
        .ok()
        .flatten()
        .and_then(|v| nonempty(&cv_note::js_text(&v)))
}

/// The pure rule: `path`, the note's `text`, the `[[serial]]` rows and the defined-kind test →
/// what to do. `Already` only when the basename equals the computed target.
pub(crate) fn plan(
    path: &str,
    text: &str,
    serial: &[SerialSource],
    is_kind: &dyn Fn(&str) -> bool,
) -> Plan {
    let title = vpath::stem(path);
    let doc = FrontmatterDoc::new(text);
    let publication = story_publication(&doc, serial, is_kind);
    match detect_with(title, doc.body(), publication.as_deref()) {
        None => Plan::Undetected,
        Some(t) => {
            let name = target_name(&t);
            if name == title {
                Plan::Already
            } else {
                Plan::Rename(join(vpath::parent(path), &format!("{name}.md")))
            }
        }
    }
}

/// [`plan`] with the step's config.
fn plan_in(ctx: &StepCtx<'_>, path: &str, text: &str) -> Plan {
    let config = ctx.config;
    let is_kind = |k: &str| kind_def(config, k).is_ok();
    plan(path, text, &config.sources().serial, &is_kind)
}

/// Why a planned backfill rename is withheld.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Withheld {
    /// Two or more notes compute the same target.
    Duplicate,
    /// The target exists and its holder is not moving.
    Exists,
    /// The renames form a cycle (each target held by the next mover).
    Cycle,
}

impl Withheld {
    fn label(self) -> &'static str {
        match self {
            Self::Duplicate => "duplicate target",
            Self::Exists => "exists",
            Self::Cycle => "cycle",
        }
    }
}

/// A withheld rename: `(src, dst, why)`.
pub(crate) type WithheldRename = (String, String, Withheld);

/// Chain-safe ordering of planned renames `(src, dst)` (pure; `exists` = a path is taken now):
/// returns the renames to run, holders first (a target held by a note that itself moves is
/// freed before it is claimed), and the withheld ones with their reason.
///
/// Every src moves to one dst and (after duplicate removal) every dst has one claimer, so
/// "dst is held by mover m" is a functional graph: cycles are withheld, then any rename whose
/// target is taken by a non-mover (fixpoint: a withheld mover holds its path) is withheld, and
/// the rest are emitted depth-first, the holder before its claimer.
pub(crate) fn order_renames(
    renames: &[(String, String)],
    exists: &dyn Fn(&str) -> bool,
) -> (Vec<(String, String)>, Vec<WithheldRename>) {
    use std::collections::{BTreeMap, HashMap};
    let mut claims: HashMap<&str, usize> = HashMap::new();
    for (_, d) in renames {
        *claims.entry(d.as_str()).or_default() += 1;
    }
    let mut withheld: BTreeMap<usize, Withheld> = BTreeMap::new();
    for (i, (_, d)) in renames.iter().enumerate() {
        if claims[d.as_str()] > 1 {
            withheld.insert(i, Withheld::Duplicate);
        }
    }
    let by_src: HashMap<&str, usize> = renames
        .iter()
        .enumerate()
        .map(|(i, (s, _))| (s.as_str(), i))
        .collect();
    // The mover holding rename i's target, if any (and not withheld).
    let next = |i: usize, w: &BTreeMap<usize, Withheld>| {
        by_src
            .get(renames[i].1.as_str())
            .copied()
            .filter(|j| !w.contains_key(j))
    };
    // Cycles among the live renames.
    for start in 0..renames.len() {
        if withheld.contains_key(&start) {
            continue;
        }
        let mut seen = vec![start];
        let mut cur = start;
        while let Some(j) = next(cur, &withheld) {
            if let Some(pos) = seen.iter().position(|&s| s == j) {
                if pos == 0 {
                    for &m in &seen {
                        withheld.insert(m, Withheld::Cycle);
                    }
                }
                break;
            }
            seen.push(j);
            cur = j;
        }
    }
    // Targets taken by a note that does not move.
    loop {
        let mut changed = false;
        for (i, (_, dst)) in renames.iter().enumerate() {
            if !withheld.contains_key(&i) && exists(dst) && next(i, &withheld).is_none() {
                withheld.insert(i, Withheld::Exists);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut done = vec![false; renames.len()];
    let mut order = Vec::new();
    for start in 0..renames.len() {
        let mut stack = Vec::new();
        let mut cur = Some(start);
        while let Some(i) = cur.filter(|i| !done[*i] && !withheld.contains_key(i)) {
            stack.push(i);
            done[i] = true;
            cur = next(i, &withheld);
        }
        while let Some(i) = stack.pop() {
            order.push(renames[i].clone());
        }
    }
    let withheld = withheld
        .into_iter()
        .map(|(i, w)| (renames[i].0.clone(), renames[i].1.clone(), w))
        .collect();
    (order, withheld)
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
        Ok(r) => {
            // The renamed note's `title` follows its new name (2026-11-10 user report: a filed
            // chapter kept the clip's title).
            retitle(ctx, dst)?;
            Ok(Attempt::Renamed {
                rewritten: r.rewritten,
            })
        }
        Err(VaultError::Exists(_)) => Ok(Attempt::Collision),
        Err(e) => Err(e.into()),
    }
}

/// `text` with its frontmatter `title` set to `name`, when the note has a `title` that differs;
/// `None` when there is none, it already matches, or the frontmatter does not parse (a note
/// without a `title` is not given one).
pub(crate) fn retitled_text(text: &str, name: &str) -> Option<String> {
    let mut doc = FrontmatterDoc::new(text);
    let current = doc.get("title").ok().flatten()?;
    if cv_note::js_text(&current).trim() == name {
        return None;
    }
    doc.set("title", &JsValue::Str(name.to_owned()))
        .ok()?
        .then(|| doc.into_text())
}

/// Sets `path`'s frontmatter `title` to its basename (see [`retitled_text`]); `true` on a change.
fn retitle(ctx: &mut StepCtx<'_>, path: &str) -> Result<bool, StepError> {
    let name = vpath::stem(path).to_owned();
    Ok(ctx.modify(path, |t| Ok(retitled_text(t, &name)))?.is_some())
}

/// Note-scope outcome vars.
#[derive(Serialize, Default)]
struct NoteVars {
    fanfic_renamed: u32,
    fanfic_skipped_collision: u32,
    fanfic_undetected: u32,
    fanfic_already: u32,
    /// Already named right, but its `title` was stale and is now set to the name.
    fanfic_retitled: u32,
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
        && expand(ctx, k)?.trim() != p.target
    {
        return Ok(out);
    }
    let src = target_path(ctx, p.path.as_deref())?;
    let (bytes, _) = ctx.read_required(&src)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let mut v = NoteVars::default();
    match plan_in(ctx, &src, &text) {
        Plan::Already => {
            v.fanfic_already = 1;
            if retitle(ctx, &src)? {
                v.fanfic_retitled = 1;
                out.push_output(src);
            }
        }
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

/// Where the dry-run review notes live, under the app folder (`vault.toml` `[structure]
/// app_folder`, plan public-release WP-2; default `_crucible/reviews`).
const REVIEW_SUBDIR: &str = "reviews";
/// Review note name suffix (`<kind><suffix><date>.md`, e.g. `fanfic-titles-2026-11-08.md`).
const REVIEW_SUFFIX: &str = "-titles-";

/// What a dry run found, in path order.
#[derive(Debug, Default)]
struct Findings {
    /// `(old path, new path)`.
    rename: Vec<(String, String)>,
    collision: Vec<(String, String, Withheld)>,
    undetected: Vec<String>,
    /// Already named right, with a stale `title`.
    retitle: Vec<String>,
}

/// A resolvable wikilink `[[path-without-.md|basename]]`; names with link syntax characters fall
/// back to a backticked path.
fn link(path: &str) -> String {
    let stem = path.strip_suffix(".md").unwrap_or(path);
    if stem.contains(['|', '[', ']', '#', '^']) {
        return format!("`{path}`");
    }
    format!("[[{stem}|{}]]", vpath::stem(path))
}

fn section(out: &mut String, title: &str, lines: Vec<String>) {
    out.push_str(&format!("\n## {title} ({})\n\n", lines.len()));
    if lines.is_empty() {
        out.push_str("- (none)\n");
    }
    for l in lines {
        out.push_str(&format!("- {l}\n"));
    }
}

/// `fanfic` → `Fanfic` (the review heading).
fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| {
        f.to_uppercase().chain(c).collect::<String>()
    })
}

/// The pure review-note renderer.
fn render_review(kind: &str, date: &str, v: &BackfillVars, f: &Findings) -> String {
    let base = |p: &str| vpath::stem(p).to_owned();
    let mut out = format!(
        "# {} title backfill \u{2014} dry run {date}\n\nScanned {}, already {}, would rename {}, would retitle {}, collision {}, undetected {}.\n\nApply: run this backfill again with Dry run off.\n",
        capitalized(kind),
        v.scanned,
        v.already,
        v.would_rename,
        v.would_retitle,
        v.collision,
        v.undetected
    );
    section(
        &mut out,
        "Would rename",
        f.rename
            .iter()
            .map(|(o, n)| format!("{} \u{2192} {}", link(o), base(n)))
            .collect(),
    );
    section(
        &mut out,
        "Would retitle",
        f.retitle.iter().map(|o| link(o)).collect(),
    );
    section(
        &mut out,
        "Collision",
        f.collision
            .iter()
            .map(|(o, n, w)| format!("{} \u{2192} {} ({})", link(o), base(n), w.label()))
            .collect(),
    );
    section(
        &mut out,
        "Undetected",
        f.undetected.iter().map(|o| link(o)).collect(),
    );
    out
}

#[derive(Serialize, Default)]
struct BackfillVars {
    scanned: u32,
    already: u32,
    renamed: u32,
    would_rename: u32,
    /// Already named right; `title` set to the name (real run).
    retitled: u32,
    /// Already named right with a stale `title` (dry run).
    would_retitle: u32,
    collision: u32,
    undetected: u32,
    conflict: u32,
    dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    review_path: Option<String>,
}

fn backfill(kind: &str, dry_run: bool, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let paths: BTreeSet<String> = ctx
        .index()?
        .query()
        .tagged(kind)?
        .into_iter()
        .map(|r| r.path)
        .filter(|p| p.ends_with(".md") && vpath::is_user_path(p))
        .collect();
    let mut v = BackfillVars {
        dry_run,
        ..BackfillVars::default()
    };
    let mut out = StepOutcome::default();
    let mut found = Findings::default();
    let mut planned = Vec::new();
    for path in &paths {
        let Some((bytes, _)) = ctx.read(path)? else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        v.scanned += 1;
        match plan_in(ctx, path, &text) {
            Plan::Already => {
                v.already += 1;
                if retitled_text(&text, vpath::stem(path)).is_some() {
                    found.retitle.push(path.clone());
                }
            }
            Plan::Undetected => {
                v.undetected += 1;
                found.undetected.push(path.clone());
            }
            Plan::Rename(dst) => planned.push((path.clone(), dst)),
        }
    }
    let mut taken = HashSet::new();
    for (_, dst) in &planned {
        if ctx.read(dst)?.is_some() {
            taken.insert(dst.clone());
        }
    }
    let (order, withheld) = order_renames(&planned, &|p| taken.contains(p));
    v.collision = u32::try_from(withheld.len()).unwrap_or(u32::MAX);
    found.collision = withheld;
    if dry_run {
        v.would_rename = u32::try_from(order.len()).unwrap_or(u32::MAX);
        v.would_retitle = u32::try_from(found.retitle.len()).unwrap_or(u32::MAX);
        found.rename = order;
    } else {
        for (src, dst) in order {
            match rename(ctx, &src, &dst) {
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
            }
        }
        for path in std::mem::take(&mut found.retitle) {
            match retitle(ctx, &path) {
                Ok(true) => {
                    v.retitled += 1;
                    out.push_output(path);
                }
                Ok(false) => {}
                Err(e) if e.is_conflict() => v.conflict += 1,
                Err(e) => return Err(e),
            }
        }
    }
    if dry_run {
        let date = now().date().format("%Y-%m-%d").to_string();
        let review = format!(
            "{}/{REVIEW_SUBDIR}/{kind}{REVIEW_SUFFIX}{date}.md",
            ctx.config.app_folder()
        );
        let text = render_review(kind, &date, &v, &found).into_bytes();
        match ctx.read(&review)? {
            Some((_, hash)) => {
                ctx.write(&review, text, Some(hash))?;
            }
            None => {
                ctx.create(&review, text)?;
            }
        }
        out.open_path = Some(review.clone());
        v.review_path = Some(review);
    }
    out.set_vars(&v);
    Ok(out)
}
