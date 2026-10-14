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
//! | `upsert_tags` | `tags` (string or list, placeholders ok) and/or `kind` | append to `tags` unless present (leading `#` stripped) |
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

use std::sync::LazyLock;

use cv_actions::{ConfigSet, Step, StepKind};
use cv_lint::{FrontmatterDoc, JsValue};
use regex::Regex;
use serde_json::Value;

use super::capture::{FORWARDED_TASK, OPEN_TASK};
use super::materialize::{
    check_keys, check_placeholders, expand, get_bool, get_str, get_str_list, join, table,
};
use super::{StepCtx, StepError, StepOutcome};

const KIND_KEYS: &[&str] = &["label", "tags", "folder", "cleanup_transcript", "props"];

/// The `[kinds.NAME]` table of `kinds.toml`.
pub(super) fn kind_def<'c>(config: &'c ConfigSet, name: &str) -> Result<&'c toml::Table, String> {
    let def = config
        .table("kinds.toml")
        .get("kinds")
        .and_then(toml::Value::as_table)
        .and_then(|k| k.get(name))
        .and_then(toml::Value::as_table)
        .ok_or_else(|| format!("kind {name:?} is not defined in kinds.toml [kinds]"))?;
    check_keys(def, KIND_KEYS, "[kinds]")?;
    Ok(def)
}

/// Resolves the step's `kind` (expanded) to its table, if the step names one.
pub(super) fn step_kind<'c>(
    ctx: &StepCtx<'c>,
    t: &toml::Table,
) -> Result<Option<&'c toml::Table>, StepError> {
    let Some(k) = get_str(t, "kind").map_err(StepError::failed)? else {
        return Ok(None);
    };
    let name = expand(ctx, &k)?;
    if name.trim().is_empty() {
        return Err(StepError::failed("`kind` expanded to nothing"));
    }
    kind_def(ctx.config, name.trim())
        .map(Some)
        .map_err(StepError::failed)
}

/// The step's target: `path` (expanded) or the active note.
pub(super) fn target_path(ctx: &StepCtx<'_>, t: &toml::Table) -> Result<String, StepError> {
    let p = match get_str(t, "path").map_err(StepError::failed)? {
        Some(p) => expand(ctx, &p)?,
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

fn fm_err(e: cv_lint::FmError) -> StepError {
    StepError::failed(e.to_string())
}

fn keys_for(kind: StepKind) -> &'static [&'static str] {
    match kind {
        StepKind::Guard => &[
            "path",
            "tag",
            "not_tag",
            "prop",
            "not_prop",
            "folder",
            "not_folder",
        ],
        StepKind::Move => &["path", "to", "name", "kind"],
        StepKind::Copy => &["path", "to", "name"],
        StepKind::SetProps => &["path", "props", "if_empty", "kind"],
        StepKind::RemoveProps => &["path", "props"],
        StepKind::UpsertTags => &["path", "tags", "kind"],
        StepKind::MarkForwarded => &["path", "line"],
        _ => &[],
    }
}

/// Validates one step's params.
pub fn validate(step: &Step, config: &ConfigSet) -> Result<(), String> {
    let t = table(step);
    let kind = step.kind();
    check_keys(&t, keys_for(kind), kind.as_str())?;
    for k in ["path", "to", "name"] {
        if let Some(s) = get_str(&t, k)? {
            check_placeholders(&s)?;
        }
    }
    if let Some(k) = get_str(&t, "kind")? {
        check_placeholders(&k)?;
        if !k.contains('{') {
            kind_def(config, &k)?;
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
            }
        }
    }
    match kind {
        StepKind::Guard => {
            for k in ["tag", "not_tag", "prop", "not_prop", "folder", "not_folder"] {
                get_str_list(&t, k)?;
            }
        }
        StepKind::Move => {
            if get_str(&t, "to")?.is_none() && get_str(&t, "kind")?.is_none() {
                return Err("move needs `to` or `kind`".into());
            }
        }
        StepKind::Copy => {
            if get_str(&t, "to")?.is_none() {
                return Err("copy needs `to`".into());
            }
        }
        StepKind::SetProps => {
            get_bool(&t, "if_empty")?;
            match t.get("props") {
                None => {}
                Some(toml::Value::Table(p)) => {
                    for v in p.values() {
                        to_js_checked(v)?;
                    }
                }
                Some(_) => return Err("set_props `props` must be a table".into()),
            }
        }
        StepKind::RemoveProps => {
            get_str_list(&t, "props")?;
        }
        StepKind::UpsertTags => {
            let tags = get_str_list(&t, "tags")?;
            for tg in &tags {
                check_placeholders(tg)?;
            }
            if tags.is_empty() && get_str(&t, "kind")?.is_none() {
                return Err("upsert_tags needs `tags` or `kind`".into());
            }
        }
        StepKind::MarkForwarded => match t.get("line") {
            None | Some(toml::Value::Integer(0..)) => {}
            Some(_) => return Err("mark_forwarded `line` must be a non-negative integer".into()),
        },
        _ => {}
    }
    Ok(())
}

/// Executes one file step.
pub fn execute(step: &Step, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let t = table(step);
    match step.kind() {
        StepKind::Guard => guard(ctx, &t),
        StepKind::Move => move_note(ctx, &t),
        StepKind::Copy => copy_note(ctx, &t),
        StepKind::SetProps => set_props(ctx, &t),
        StepKind::RemoveProps => remove_props(ctx, &t),
        StepKind::UpsertTags => upsert_tags(ctx, &t),
        StepKind::MarkForwarded => mark_forwarded(ctx, &t),
        k => Err(StepError::failed(format!(
            "step {} is not a file step",
            k.as_str()
        ))),
    }
}

// --- guard -----------------------------------------------------------------------------------

static INLINE_TAG: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?:^|\s)#([\p{L}\p{N}_/\-]+)").ok());

fn norm_tag(s: &str) -> String {
    s.trim().trim_start_matches('#').to_lowercase()
}

fn note_tags(text: &str) -> Result<Vec<String>, StepError> {
    let doc = FrontmatterDoc::new(text);
    let mut out = Vec::new();
    match doc.get("tags").map_err(fm_err)? {
        Some(JsValue::Arr(items)) => {
            for i in items {
                if let JsValue::Str(s) = i {
                    out.push(norm_tag(&s));
                }
            }
        }
        Some(JsValue::Str(s)) => out.extend(
            s.split(|c: char| c == ',' || c.is_whitespace())
                .filter(|x| !x.trim().is_empty())
                .map(norm_tag),
        ),
        _ => {}
    }
    if let Some(re) = INLINE_TAG.as_ref() {
        for c in re.captures_iter(doc.body()) {
            if let Some(m) = c.get(1) {
                out.push(norm_tag(m.as_str()));
            }
        }
    }
    Ok(out)
}

fn has_tag(tags: &[String], want: &str) -> bool {
    let w = norm_tag(want);
    tags.iter()
        .any(|t| *t == w || t.strip_prefix(&w).is_some_and(|r| r.starts_with('/')))
}

fn has_prop(text: &str, key: &str) -> Result<bool, StepError> {
    Ok(
        match FrontmatterDoc::new(text).get(key.trim()).map_err(fm_err)? {
            None | Some(JsValue::Null) => false,
            Some(JsValue::Str(s)) => !s.trim().is_empty(),
            Some(JsValue::Arr(a)) => !a.is_empty(),
            Some(_) => true,
        },
    )
}

fn in_folder(path: &str, folder: &str) -> bool {
    let f = folder.trim_matches('/');
    f.is_empty() || path.strip_prefix(f).is_some_and(|r| r.starts_with('/'))
}

fn guard(ctx: &mut StepCtx<'_>, t: &toml::Table) -> Result<StepOutcome, StepError> {
    let stop = StepOutcome {
        stop: true,
        ..StepOutcome::default()
    };
    let list = |k: &str| -> Result<Vec<String>, StepError> {
        get_str_list(t, k)
            .map_err(StepError::failed)?
            .iter()
            .map(|s| expand(ctx, s))
            .collect()
    };
    let (tag, not_tag, prop, not_prop, folder, not_folder) = (
        list("tag")?,
        list("not_tag")?,
        list("prop")?,
        list("not_prop")?,
        list("folder")?,
        list("not_folder")?,
    );
    let needs_note =
        !(tag.is_empty() && not_tag.is_empty() && prop.is_empty() && not_prop.is_empty());
    let has_path = get_str(t, "path").map_err(StepError::failed)?.is_some()
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
    let path = target_path(ctx, t)?;
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
    let tags = note_tags(&text)?;
    if tag.iter().any(|w| !has_tag(&tags, w)) || not_tag.iter().any(|w| has_tag(&tags, w)) {
        return Ok(stop);
    }
    for p in &prop {
        if !has_prop(&text, p)? {
            return Ok(stop);
        }
    }
    for p in &not_prop {
        if has_prop(&text, p)? {
            return Ok(stop);
        }
    }
    Ok(StepOutcome::default())
}

// --- move / copy -----------------------------------------------------------------------------

fn destination(
    ctx: &StepCtx<'_>,
    t: &toml::Table,
    src: &str,
    folder: Option<String>,
) -> Result<String, StepError> {
    let folder = match folder {
        Some(f) => f,
        None => {
            let to = get_str(t, "to")
                .map_err(StepError::failed)?
                .ok_or_else(|| StepError::failed("no destination folder"))?;
            expand(ctx, &to)?
        }
    };
    let name = match get_str(t, "name").map_err(StepError::failed)? {
        Some(n) => expand(ctx, &n)?,
        None => src.rsplit('/').next().unwrap_or(src).to_owned(),
    };
    let name = name.trim().to_owned();
    if name.is_empty() || name.contains('/') {
        return Err(StepError::failed("invalid destination name"));
    }
    Ok(join(folder.trim(), &name))
}

fn move_note(ctx: &mut StepCtx<'_>, t: &toml::Table) -> Result<StepOutcome, StepError> {
    let src = target_path(ctx, t)?;
    let folder = if get_str(t, "to").map_err(StepError::failed)?.is_some() {
        None
    } else {
        let def = step_kind(ctx, t)?.ok_or_else(|| StepError::failed("move needs `to`"))?;
        let f = get_str(def, "folder")
            .map_err(StepError::failed)?
            .ok_or_else(|| StepError::failed("kind has no `folder`"))?;
        Some(expand(ctx, &f)?)
    };
    let dst = destination(ctx, t, &src, folder)?;
    let mut out = StepOutcome::default();
    out.vars
        .insert("moved_path".into(), Value::String(dst.clone()));
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
        out.vars.insert(
            "link_conflicts".into(),
            Value::from(r.skipped_conflicts.len()),
        );
    }
    Ok(out)
}

fn copy_note(ctx: &mut StepCtx<'_>, t: &toml::Table) -> Result<StepOutcome, StepError> {
    let src = target_path(ctx, t)?;
    let dst = destination(ctx, t, &src, None)?;
    if dst == src {
        return Err(StepError::failed("copy destination equals the source"));
    }
    let (bytes, _) = ctx
        .read(&src)?
        .ok_or_else(|| StepError::failed("copy source note not found"))?;
    let mut out = StepOutcome::default();
    out.vars
        .insert("copied_path".into(), Value::String(dst.clone()));
    out.vars.insert(
        "copied_link".into(),
        Value::String(format!("[[{}]]", dst.strip_suffix(".md").unwrap_or(&dst))),
    );
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

fn set_props(ctx: &mut StepCtx<'_>, t: &toml::Table) -> Result<StepOutcome, StepError> {
    let if_empty = get_bool(t, "if_empty")
        .map_err(StepError::failed)?
        .unwrap_or(false);
    let mut props: Vec<(String, JsValue)> = Vec::new();
    if let Some(def) = step_kind(ctx, t)?
        && let Some(toml::Value::Table(p)) = def.get("props")
    {
        for (k, v) in p {
            props.push((k.clone(), to_js(ctx, v)?));
        }
    }
    if let Some(toml::Value::Table(p)) = t.get("props") {
        for (k, v) in p {
            props.push((k.clone(), to_js(ctx, v)?));
        }
    }
    if props.is_empty() {
        return Ok(StepOutcome::default());
    }
    let path = target_path(ctx, t)?;
    edit_note(ctx, &path, |doc| {
        let mut changed = false;
        for (k, v) in &props {
            changed |= if if_empty {
                doc.set_if_empty(k, v)
            } else {
                doc.set(k, v)
            }
            .map_err(fm_err)?;
        }
        Ok(changed)
    })
}

fn remove_props(ctx: &mut StepCtx<'_>, t: &toml::Table) -> Result<StepOutcome, StepError> {
    let keys = get_str_list(t, "props").map_err(StepError::failed)?;
    if keys.is_empty() {
        return Ok(StepOutcome::default());
    }
    let path = target_path(ctx, t)?;
    edit_note(ctx, &path, |doc| {
        let mut changed = false;
        for k in &keys {
            changed |= doc.remove(k).map_err(fm_err)?;
        }
        Ok(changed)
    })
}

fn upsert_tags(ctx: &mut StepCtx<'_>, t: &toml::Table) -> Result<StepOutcome, StepError> {
    let mut tags = Vec::new();
    if let Some(def) = step_kind(ctx, t)? {
        tags.extend(get_str_list(def, "tags").map_err(StepError::failed)?);
    }
    tags.extend(get_str_list(t, "tags").map_err(StepError::failed)?);
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
    let path = target_path(ctx, t)?;
    edit_note(ctx, &path, |doc| {
        let mut changed = false;
        for tg in &clean {
            changed |= doc
                .append_to_list("tags", &JsValue::Str(tg.clone()))
                .map_err(fm_err)?;
        }
        Ok(changed)
    })
}

// --- mark_forwarded --------------------------------------------------------------------------

fn mark_forwarded(ctx: &mut StepCtx<'_>, t: &toml::Table) -> Result<StepOutcome, StepError> {
    let path = target_path(ctx, t)?;
    let line = match t.get("line") {
        Some(toml::Value::Integer(i)) => usize::try_from(*i).ok(),
        _ => ctx.context.line.and_then(|l| usize::try_from(l).ok()),
    }
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
        let tags = note_tags("---\ntags: [a, B/c]\n---\nbody #inline and #x/y\n").unwrap();
        assert!(has_tag(&tags, "#a"));
        assert!(has_tag(&tags, "b"));
        assert!(has_tag(&tags, "inline"));
        assert!(has_tag(&tags, "x"));
        assert!(!has_tag(&tags, "refined"));
        assert!(in_folder("inbox/x.md", "inbox"));
        assert!(!in_folder("inboxes/x.md", "inbox"));
    }
}
