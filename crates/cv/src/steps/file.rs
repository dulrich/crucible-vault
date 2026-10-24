//! Step module `file`: `guard`, `move`, `copy`, `set_props`, `remove_props`, `upsert_tags`,
//! `mark_forwarded` — the building blocks of the file-as-kind and refine-transcript actions.
//!
//! Sources (all under `https://github.com/dulrich/obsidian-crucible/blob/master/`): the chain guard step (`src/chains.ts`,
//! `guardCondition {type: "not-has-tag"}`), the internal commands `crucible:upsert-tags`,
//! `crucible:upsert-property`, `crucible:copy-note-to-folder`,
//! `obsidian-crucible:move-current-file-to-folder` / `move-current-file-to-daily-folder`
//! (`src/internalCommands.ts`), and `src/markForwarded.ts:3-16` (`[ ]` → `[>]`). Dropped
//! (Obsidian-induced): the command-id indirection and arg strings (`{"tags": "x"}` → typed step
//! keys), `processFrontMatter` whole-block re-serialization (minimal-diff `FrontmatterDoc`
//! instead), saving the owning `MarkdownView` before the post-chain buffer reload (we write the
//! file with CAS; the client reloads on `file_changed`), and `fileManager.renameFile`'s
//! metadata-cache dependence (the writer's rename rewrites links from the index).
//!
//! # Step params
//!
//! Every op targets `path` (placeholders: `{param.NAME}`, `{vars.NAME}`, `{path}`, `{period.P}`)
//! or, by default, the active/triggering note.
//!
//! | step | keys | meaning |
//! |---|---|---|
//! | `guard` | `tag`, `not_tag`, `prop`, `not_prop`, `folder`, `not_folder` (string or list) | every listed condition must hold, else `stop = true` (never an error); a missing note does not hold |
//! | `move` | `to` (folder template) or `kind`; `name` (default: same basename) | rename via the writer (links rewritten); `vars.moved_path`, `open_path` |
//! | `copy` | `to` (folder template), `name` | create a copy; `vars.copied_path`, `vars.copied_link` (`[[path]]`), `output_paths` |
//! | `set_props` | `props = { key = value }`, `if_empty` (bool) | set-semantics; string values expand placeholders |
//! | `remove_props` | `props = ["key", …]` | |
//! | `upsert_tags` | `tags` (string or list, placeholders ok) and/or `kind` | append to `tags` unless present (leading `#` stripped); with `kind` = `blog`/`newsletter`, also `trim-label: false` when that key is absent (round-12 WP-5) |
//! | `mark_forwarded` | `line` (0-based; default `context.line`) | `- [ ] x` → `- [>] x`; already `[>]` = no-op; anything else fails closed |
//!
//! Kinds (`kinds.toml`): `[kinds.NAME]` with `label`, `tags` (list), `folder` (destination
//! folder template, e.g. `"{period.day}"` = today's daily asset folder), `cleanup_transcript`
//! (bool, read by the `lint` step) and `props` (table, applied by `set_props` with `kind`).
//! A step's `kind` value usually is `"{param.kind}"`.
//!
//! Tags for `guard` are the frontmatter `tags` plus inline `#tags` in the body, compared
//! case-insensitively; a guard tag also matches its nested children (`a` matches `a/b`).
//!
//! Idempotence (E2): tag/prop ops are set-semantics and write only on change; `mark_forwarded`
//! on `[>]` is a no-op; a `move` whose source is gone but whose destination exists succeeds; a
//! `copy` whose destination already holds identical bytes succeeds.

use cv_actions::{ConfigSet, StepKind};
use cv_note::{FrontmatterDoc, JsValue};
use serde::Serialize;

use super::capture::{FORWARDED_TASK, OPEN_TASK};
use super::materialize::join;
use super::params::{check_keys, check_placeholders, expand, get_bool, get_str, get_str_list};
use super::{StepCtx, StepError, StepModule, StepOutcome};

const KIND_KEYS: &[&str] = &[
    "label",
    "tags",
    "folder",
    "cleanup_transcript",
    "props",
    "hosts",
];

/// The `[kinds.NAME]` table of `kinds.toml`.
pub(super) fn kind_def<'c>(config: &'c ConfigSet, name: &str) -> Result<&'c toml::Table, String> {
    let def = config
        .table("kinds.toml")
        .get("kinds")
        .and_then(toml::Value::as_table)
        .and_then(|k| k.get(name))
        .and_then(toml::Value::as_table)
        .ok_or_else(|| format!("kind {name:?} is not defined in kinds.toml [kinds]"))?;
    if let Some(k) = def.keys().find(|k| !KIND_KEYS.contains(&k.as_str())) {
        return Err(format!("[kinds] step: unknown key {k:?}"));
    }
    Ok(def)
}

/// Resolves the step's `kind` (expanded) to its table, if the step names one.
pub(super) fn step_kind<'c>(
    ctx: &StepCtx<'c>,
    kind: Option<&str>,
) -> Result<Option<&'c toml::Table>, StepError> {
    let Some(k) = kind else {
        return Ok(None);
    };
    let name = expand(ctx, k)?;
    if name.trim().is_empty() {
        return Err(StepError::failed("`kind` expanded to nothing"));
    }
    kind_def(ctx.config, name.trim())
        .map(Some)
        .map_err(StepError::failed)
}

/// The kinds whose new captures start with an unticked `trim-label` (plan round-12 WP-5).
pub(crate) fn wants_trim_label(kind: &str) -> bool {
    matches!(kind.trim(), "blog" | "newsletter")
}

/// The one shared absent-key write: `trim-label: false` for [`wants_trim_label`] kinds. An
/// existing value (`true`, `null`, `""` included) is never touched.
pub(crate) fn apply_trim_label(
    doc: &mut FrontmatterDoc,
    kind: &str,
) -> Result<bool, cv_note::FmError> {
    if !wants_trim_label(kind) {
        return Ok(false);
    }
    doc.set_if_absent("trim-label", &JsValue::Bool(false))
}

/// The step's target: `path` (expanded) or the active note.
pub(super) fn target_path(ctx: &StepCtx<'_>, path: Option<&str>) -> Result<String, StepError> {
    let p = match path {
        Some(p) => expand(ctx, p)?,
        None => ctx.context.active_path.clone().unwrap_or_default(),
    };
    let p = p.trim().trim_matches('/').to_owned();
    if p.is_empty() {
        return Err(StepError::failed(
            "no target note (no `path`, no active path)",
        ));
    }
    Ok(p)
}

fn utf8(bytes: Vec<u8>) -> Result<String, StepError> {
    String::from_utf8(bytes).map_err(|_| StepError::failed("note is not UTF-8"))
}

/// Parsed params of the file steps (each kind reads its own subset).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Params {
    kind: Option<StepKind>,
    path: Option<String>,
    to: Option<String>,
    name: Option<String>,
    /// `kind` (a `kinds.toml` entry name; placeholders ok).
    note_kind: Option<String>,
    tag: Vec<String>,
    not_tag: Vec<String>,
    prop: Vec<String>,
    not_prop: Vec<String>,
    folder: Vec<String>,
    not_folder: Vec<String>,
    /// `set_props` `props` (values expanded at run time).
    set: toml::Table,
    /// `remove_props` `props`.
    remove: Vec<String>,
    if_empty: bool,
    tags: Vec<String>,
    line: Option<usize>,
}

/// The file steps: `guard`, `move`, `copy`, `set_props`, `remove_props`, `upsert_tags`,
/// `mark_forwarded`.
pub(crate) struct File;

impl StepModule for File {
    type Params = Params;

    fn parse(kind: StepKind, t: &toml::Table, config: &ConfigSet) -> Result<Params, String> {
        parse(kind, t, config)
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        match p.kind {
            Some(StepKind::Guard) => guard(ctx, &p),
            Some(StepKind::Move) => move_note(ctx, &p),
            Some(StepKind::Copy) => copy_note(ctx, &p),
            Some(StepKind::SetProps) => set_props(ctx, &p),
            Some(StepKind::RemoveProps) => remove_props(ctx, &p),
            Some(StepKind::UpsertTags) => upsert_tags(ctx, &p),
            Some(StepKind::MarkForwarded) => mark_forwarded(ctx, &p),
            k => Err(StepError::failed(format!(
                "step {} is not a file step",
                k.map_or("?", StepKind::as_str)
            ))),
        }
    }
}

fn parse(kind: StepKind, t: &toml::Table, config: &ConfigSet) -> Result<Params, String> {
    check_keys(t, kind, kind.as_str())?;
    for k in ["path", "to", "name"] {
        if let Some(s) = get_str(t, k)? {
            check_placeholders(&s)?;
        }
    }
    let note_kind = get_str(t, "kind")?;
    if let Some(k) = &note_kind {
        check_placeholders(k)?;
        if !k.contains('{') {
            kind_def(config, k)?;
        }
        if let Some(kinds) = config.table("kinds.toml").get("kinds") {
            let kinds = kinds
                .as_table()
                .ok_or("kinds.toml `kinds` must be a table of kinds")?;
            for name in kinds.keys() {
                let def = kind_def(config, name)?;
                if let Some(f) = get_str(def, "folder")? {
                    check_placeholders(&f)?;
                }
                get_str_list(def, "tags")?;
                get_bool(def, "cleanup_transcript")?;
                get_str_list(def, "hosts")?;
            }
        }
    }
    let mut p = Params {
        kind: Some(kind),
        path: get_str(t, "path")?,
        to: get_str(t, "to")?,
        name: get_str(t, "name")?,
        note_kind,
        ..Params::default()
    };
    match kind {
        StepKind::Guard => {
            p.tag = get_str_list(t, "tag")?;
            p.not_tag = get_str_list(t, "not_tag")?;
            p.prop = get_str_list(t, "prop")?;
            p.not_prop = get_str_list(t, "not_prop")?;
            p.folder = get_str_list(t, "folder")?;
            p.not_folder = get_str_list(t, "not_folder")?;
        }
        StepKind::Move => {
            if p.to.is_none() && p.note_kind.is_none() {
                return Err("move needs `to` or `kind`".into());
            }
        }
        StepKind::Copy => {
            if p.to.is_none() {
                return Err("copy needs `to`".into());
            }
        }
        StepKind::SetProps => {
            p.if_empty = get_bool(t, "if_empty")?.unwrap_or(false);
            match t.get("props") {
                None => {}
                Some(toml::Value::Table(set)) => {
                    for v in set.values() {
                        to_js_checked(v)?;
                    }
                    p.set = set.clone();
                }
                Some(_) => return Err("set_props `props` must be a table".into()),
            }
        }
        StepKind::RemoveProps => {
            p.remove = get_str_list(t, "props")?;
        }
        StepKind::UpsertTags => {
            p.tags = get_str_list(t, "tags")?;
            for tg in &p.tags {
                check_placeholders(tg)?;
            }
            if p.tags.is_empty() && p.note_kind.is_none() {
                return Err("upsert_tags needs `tags` or `kind`".into());
            }
        }
        StepKind::MarkForwarded => match t.get("line") {
            None => {}
            Some(toml::Value::Integer(i @ 0..)) => p.line = usize::try_from(*i).ok(),
            Some(_) => return Err("mark_forwarded `line` must be a non-negative integer".into()),
        },
        _ => {}
    }
    Ok(p)
}

// --- guard -----------------------------------------------------------------------------------

fn norm_tag(s: &str) -> String {
    s.trim().trim_start_matches('#').to_lowercase()
}

/// The note's tags as the index records them ([`cv_index::note_tags`]: ofm's frontmatter +
/// inline tags, never inside code — SR11), normalized for the guard's case-insensitive match.
fn note_tags(text: &str) -> Vec<String> {
    cv_index::note_tags(text)
        .iter()
        .map(|t| norm_tag(t))
        .collect()
}

fn has_tag(tags: &[String], want: &str) -> bool {
    let w = norm_tag(want);
    tags.iter()
        .any(|t| *t == w || t.strip_prefix(&w).is_some_and(|r| r.starts_with('/')))
}

fn has_prop(text: &str, key: &str) -> Result<bool, StepError> {
    Ok(match FrontmatterDoc::new(text).get(key.trim())? {
        None | Some(JsValue::Null) => false,
        Some(JsValue::Str(s)) => !s.trim().is_empty(),
        Some(JsValue::Arr(a)) => !a.is_empty(),
        Some(_) => true,
    })
}

fn in_folder(path: &str, folder: &str) -> bool {
    let f = folder.trim_matches('/');
    f.is_empty() || path.strip_prefix(f).is_some_and(|r| r.starts_with('/'))
}

fn guard(ctx: &mut StepCtx<'_>, p: &Params) -> Result<StepOutcome, StepError> {
    let stop = StepOutcome {
        stop: true,
        ..StepOutcome::default()
    };
    let list = |l: &[String]| -> Result<Vec<String>, StepError> {
        l.iter().map(|s| expand(ctx, s)).collect()
    };
    let (tag, not_tag, prop, not_prop, folder, not_folder) = (
        list(&p.tag)?,
        list(&p.not_tag)?,
        list(&p.prop)?,
        list(&p.not_prop)?,
        list(&p.folder)?,
        list(&p.not_folder)?,
    );
    let needs_note =
        !(tag.is_empty() && not_tag.is_empty() && prop.is_empty() && not_prop.is_empty());
    let has_path = p.path.is_some()
        || ctx
            .context
            .active_path
            .as_deref()
            .is_some_and(|p| !p.is_empty());
    if !has_path {
        // Nothing to test against: only a condition-free guard holds.
        let any = needs_note || !folder.is_empty() || !not_folder.is_empty();
        return Ok(if any { stop } else { StepOutcome::default() });
    }
    let path = target_path(ctx, p.path.as_deref())?;
    if folder.iter().any(|f| !in_folder(&path, f)) || not_folder.iter().any(|f| in_folder(&path, f))
    {
        return Ok(stop);
    }
    if !needs_note {
        return Ok(StepOutcome::default());
    }
    let Some((bytes, _)) = ctx.read(&path)? else {
        return Ok(stop);
    };
    let text = utf8(bytes)?;
    let tags = note_tags(&text);
    if tag.iter().any(|w| !has_tag(&tags, w)) || not_tag.iter().any(|w| has_tag(&tags, w)) {
        return Ok(stop);
    }
    for k in &prop {
        if !has_prop(&text, k)? {
            return Ok(stop);
        }
    }
    for k in &not_prop {
        if has_prop(&text, k)? {
            return Ok(stop);
        }
    }
    Ok(StepOutcome::default())
}

// --- move / copy -----------------------------------------------------------------------------

fn destination(
    ctx: &StepCtx<'_>,
    p: &Params,
    src: &str,
    folder: Option<String>,
) -> Result<String, StepError> {
    let folder = match folder {
        Some(f) => f,
        None => {
            let to =
                p.to.as_deref()
                    .ok_or_else(|| StepError::failed("no destination folder"))?;
            expand(ctx, to)?
        }
    };
    let name = match &p.name {
        Some(n) => expand(ctx, n)?,
        None => cv_text::vpath::basename(src).to_owned(),
    };
    let name = name.trim().to_owned();
    if name.is_empty() || name.contains('/') {
        return Err(StepError::failed("invalid destination name"));
    }
    Ok(join(folder.trim(), &name))
}

fn move_note(ctx: &mut StepCtx<'_>, p: &Params) -> Result<StepOutcome, StepError> {
    let src = target_path(ctx, p.path.as_deref())?;
    let folder = if p.to.is_some() {
        None
    } else {
        let def = step_kind(ctx, p.note_kind.as_deref())?
            .ok_or_else(|| StepError::failed("move needs `to`"))?;
        let f = get_str(def, "folder")
            .map_err(StepError::failed)?
            .ok_or_else(|| StepError::failed("kind has no `folder`"))?;
        Some(expand(ctx, &f)?)
    };
    let dst = destination(ctx, p, &src, folder)?;
    let mut out = StepOutcome::default();
    out.set_vars(&MoveVars {
        moved_path: dst.clone(),
        moved_from: src.clone(),
        link_conflicts: None,
    });
    out.open_path = Some(dst.clone());
    if dst == src {
        return Ok(out);
    }
    let Some((_, hash)) = ctx.read(&src)? else {
        // E2: an earlier attempt already moved it.
        if ctx.read(&dst)?.is_some() {
            return Ok(out);
        }
        return Err(StepError::failed("move source note not found"));
    };
    let (s2, d2) = (src.clone(), dst.clone());
    let r = ctx.with_writer(&[src.as_str(), dst.as_str()], move |vw| {
        vw.rename(&s2, &d2, hash)
    })??;
    out.output_paths.push(dst);
    out.output_paths.extend(r.rewritten);
    if !r.skipped_conflicts.is_empty() {
        out.set_vars(&LinkConflicts {
            link_conflicts: r.skipped_conflicts.len(),
        });
    }
    Ok(out)
}

/// `move` outcome vars (`moved_from` = [`super::MOVED_FROM`]).
#[derive(Serialize)]
struct MoveVars {
    moved_path: String,
    moved_from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    link_conflicts: Option<usize>,
}

/// `move`'s `link_conflicts` var (set only when the rename skipped some).
#[derive(Serialize)]
struct LinkConflicts {
    link_conflicts: usize,
}

/// `copy` outcome vars.
#[derive(Serialize)]
struct CopyVars {
    copied_path: String,
    copied_link: String,
}

fn copy_note(ctx: &mut StepCtx<'_>, p: &Params) -> Result<StepOutcome, StepError> {
    let src = target_path(ctx, p.path.as_deref())?;
    let dst = destination(ctx, p, &src, None)?;
    if dst == src {
        return Err(StepError::failed("copy destination equals the source"));
    }
    let (bytes, _) = ctx
        .read(&src)?
        .ok_or_else(|| StepError::failed("copy source note not found"))?;
    let mut out = StepOutcome::default();
    out.set_vars(&CopyVars {
        copied_path: dst.clone(),
        copied_link: format!("[[{}]]", dst.strip_suffix(".md").unwrap_or(&dst)),
    });
    match ctx.read(&dst)? {
        Some((existing, _)) if existing == bytes => {}
        Some(_) => {
            return Err(StepError::failed(
                "copy destination exists with different content",
            ));
        }
        None => {
            ctx.create(&dst, bytes)?;
        }
    }
    out.output_paths.push(dst);
    Ok(out)
}

// --- props / tags ----------------------------------------------------------------------------

fn to_js_checked(v: &toml::Value) -> Result<JsValue, String> {
    Ok(match v {
        toml::Value::String(s) => JsValue::Str(s.clone()),
        toml::Value::Integer(i) => {
            #[allow(clippy::cast_precision_loss)]
            let f = *i as f64;
            JsValue::Num(f)
        }
        toml::Value::Float(f) => JsValue::Num(*f),
        toml::Value::Boolean(b) => JsValue::Bool(*b),
        toml::Value::Array(a) => {
            JsValue::Arr(a.iter().map(to_js_checked).collect::<Result<_, _>>()?)
        }
        _ => return Err("prop values must be strings, numbers, booleans or lists".into()),
    })
}

fn to_js(ctx: &StepCtx<'_>, v: &toml::Value) -> Result<JsValue, StepError> {
    Ok(match v {
        toml::Value::String(s) => JsValue::Str(expand(ctx, s)?),
        toml::Value::Array(a) => {
            JsValue::Arr(a.iter().map(|x| to_js(ctx, x)).collect::<Result<_, _>>()?)
        }
        other => to_js_checked(other).map_err(StepError::failed)?,
    })
}

/// Reads `path`, applies `edit` to its frontmatter, writes (CAS) only when it changed.
fn edit_note(
    ctx: &mut StepCtx<'_>,
    path: &str,
    edit: impl FnOnce(&mut FrontmatterDoc) -> Result<bool, StepError>,
) -> Result<StepOutcome, StepError> {
    let (bytes, hash) = ctx
        .read(path)?
        .ok_or_else(|| StepError::failed("target note not found"))?;
    let mut doc = FrontmatterDoc::new(utf8(bytes)?);
    let mut out = StepOutcome::default();
    if edit(&mut doc)? {
        ctx.write(path, doc.into_text().into_bytes(), Some(hash))?;
        out.output_paths.push(path.to_owned());
    }
    Ok(out)
}

fn set_props(ctx: &mut StepCtx<'_>, p: &Params) -> Result<StepOutcome, StepError> {
    let if_empty = p.if_empty;
    let mut props: Vec<(String, JsValue)> = Vec::new();
    if let Some(def) = step_kind(ctx, p.note_kind.as_deref())?
        && let Some(toml::Value::Table(kp)) = def.get("props")
    {
        for (k, v) in kp {
            props.push((k.clone(), to_js(ctx, v)?));
        }
    }
    for (k, v) in &p.set {
        props.push((k.clone(), to_js(ctx, v)?));
    }
    if props.is_empty() {
        return Ok(StepOutcome::default());
    }
    let path = target_path(ctx, p.path.as_deref())?;
    edit_note(ctx, &path, |doc| {
        let mut changed = false;
        for (k, v) in &props {
            changed |= if if_empty {
                doc.set_if_empty(k, v)
            } else {
                doc.set(k, v)
            }?;
        }
        Ok(changed)
    })
}

fn remove_props(ctx: &mut StepCtx<'_>, p: &Params) -> Result<StepOutcome, StepError> {
    let keys = p.remove.clone();
    if keys.is_empty() {
        return Ok(StepOutcome::default());
    }
    let path = target_path(ctx, p.path.as_deref())?;
    edit_note(ctx, &path, |doc| {
        let mut changed = false;
        for k in &keys {
            changed |= doc.remove(k)?;
        }
        Ok(changed)
    })
}

fn upsert_tags(ctx: &mut StepCtx<'_>, p: &Params) -> Result<StepOutcome, StepError> {
    let mut tags = Vec::new();
    if let Some(def) = step_kind(ctx, p.note_kind.as_deref())? {
        tags.extend(get_str_list(def, "tags").map_err(StepError::failed)?);
    }
    tags.extend(p.tags.iter().cloned());
    let mut clean: Vec<String> = Vec::new();
    for tg in &tags {
        let e = expand(ctx, tg)?;
        let c = e.trim().trim_start_matches('#').trim().to_owned();
        if !c.is_empty() && !clean.contains(&c) {
            clean.push(c);
        }
    }
    if clean.is_empty() {
        return Err(StepError::failed("upsert_tags: no tags to add"));
    }
    let kind = match p.note_kind.as_deref() {
        Some(k) => Some(expand(ctx, k)?),
        None => None,
    };
    let path = target_path(ctx, p.path.as_deref())?;
    edit_note(ctx, &path, |doc| {
        let mut changed = false;
        for tg in &clean {
            changed |= doc.append_to_list("tags", &JsValue::Str(tg.clone()))?;
        }
        if let Some(k) = &kind {
            changed |= apply_trim_label(doc, k)?;
        }
        Ok(changed)
    })
}

// --- mark_forwarded --------------------------------------------------------------------------

fn mark_forwarded(ctx: &mut StepCtx<'_>, p: &Params) -> Result<StepOutcome, StepError> {
    let path = target_path(ctx, p.path.as_deref())?;
    let line = p
        .line
        .or_else(|| ctx.context.line.and_then(|l| usize::try_from(l).ok()))
        .ok_or_else(|| StepError::failed("mark_forwarded needs a line"))?;
    let (bytes, hash) = ctx
        .read(&path)?
        .ok_or_else(|| StepError::failed("mark_forwarded note not found"))?;
    let text = utf8(bytes)?;
    let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    let cur = lines
        .get(line)
        .cloned()
        .ok_or_else(|| StepError::failed("mark_forwarded line out of range"))?;
    let (body, cr) = match cur.strip_suffix('\r') {
        Some(b) => (b.to_owned(), "\r"),
        None => (cur.clone(), ""),
    };
    let open = OPEN_TASK
        .as_ref()
        .ok_or_else(|| StepError::failed("internal regex"))?;
    let fwd = FORWARDED_TASK
        .as_ref()
        .ok_or_else(|| StepError::failed("internal regex"))?;
    if fwd.is_match(&body) {
        return Ok(StepOutcome::default());
    }
    if !open.is_match(&body) {
        return Err(StepError::failed(
            "mark_forwarded: line is not an open task",
        ));
    }
    lines[line] = format!("{}{cr}", open.replace(&body, "$1>$2"));
    ctx.write(&path, lines.join("\n").into_bytes(), Some(hash))?;
    Ok(StepOutcome {
        output_paths: vec![path],
        ..StepOutcome::default()
    })
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn tags_and_folders() {
        let tags = note_tags("---\ntags: [a, B/c]\n---\nbody #inline and #x/y\n");
        assert!(has_tag(&tags, "#a"));
        assert!(has_tag(&tags, "b"));
        assert!(has_tag(&tags, "inline"));
        assert!(has_tag(&tags, "x"));
        assert!(!has_tag(&tags, "refined"));
        assert!(in_folder("inbox/x.md", "inbox"));
        assert!(!in_folder("inboxes/x.md", "inbox"));
    }

    /// SR11: guard tags are the index's (ofm) tags — a `#tag` in a code fence or inline code
    /// never matches; frontmatter and body tags do; nested `#a/b` matches `a` and `a/b` (the
    /// index's `tagged` prefix rule) but not `b`.
    #[test]
    fn guard_tags_follow_the_index_parse() {
        let text = "---\ntags: [fm]\n---\nbody #body and #a/b\n\n```\n#fenced\n```\n\nsee `#inline-code`\n";
        let tags = note_tags(text);
        assert_eq!(
            tags,
            cv_index::note_tags(text)
                .iter()
                .map(|t| norm_tag(t))
                .collect::<Vec<_>>()
        );
        assert!(has_tag(&tags, "fm"));
        assert!(has_tag(&tags, "body"));
        assert!(has_tag(&tags, "a") && has_tag(&tags, "a/b"));
        assert!(!has_tag(&tags, "b"));
        assert!(!has_tag(&tags, "fenced"));
        assert!(!has_tag(&tags, "inline-code"));
    }
}
