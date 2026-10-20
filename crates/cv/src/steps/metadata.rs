//! The one metadata-link algorithm shared by `yt_metadata` and `x_metadata`:
//! read the active note (lossily), resolve the id,
//! short-circuit when the note already links a live metadata note, find-or-create the metadata
//! note (the per-kind fetch), then link it onto the source note through [`StepCtx::modify`]
//! (fetch first, CAS after, SR7). Only the id resolution and the fetch + note body stay per step.

use cv_note::{FrontmatterDoc, JsValue};
use serde::Serialize;

use super::web::{append_link, link_targets};
use super::{StepCtx, StepError, StepOutcome};

/// What differs between the metadata kinds: the id key the link list (`list_key`) is inserted
/// after, the default root folder, and the id check (an invalid id is a no-op run).
pub(crate) struct MetadataKind {
    /// The outcome var naming the id (`video_id` / `status_id`).
    pub id_var: &'static str,
    pub anchor: &'static str,
    pub list_key: &'static str,
    pub default_root: &'static str,
    pub valid: fn(&str) -> bool,
}

impl MetadataKind {
    /// The configured root (`/`-trimmed), else [`Self::default_root`].
    pub fn root(&self, configured: &str) -> String {
        let r = configured.trim_matches('/');
        if r.is_empty() { self.default_root } else { r }.to_owned()
    }
}

/// A found or created metadata note; `output` reports `path` as a step output, `extra` is
/// per-kind (the created video's channel id).
pub(crate) struct Ensured<E> {
    pub path: String,
    pub created: bool,
    pub tombstone: bool,
    pub output: bool,
    pub extra: E,
}

/// The shared outcome vars (`metadata_path` is absent for an invalid id).
#[derive(Serialize)]
struct Linked {
    #[serde(skip)]
    id: String,
    created: bool,
    tombstone: bool,
    linked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata_path: Option<String>,
}

/// A string or numeric frontmatter value, trimmed and non-empty.
pub(crate) fn str_prop(v: Option<JsValue>) -> Option<String> {
    match v? {
        JsValue::Str(s) => Some(s.trim().to_owned()).filter(|s| !s.is_empty()),
        JsValue::Num(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Crucible `fetched_at` (`new Date().toISOString()`).
pub(crate) fn iso_now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// The `link_metadata` algorithm; sets the shared outcome vars. `resolve` gets the active
/// note's frontmatter (read lossily) and returns the id plus whatever `ensure` needs; `ensure`
/// finds or creates the note. Returns `ensure`'s extra (`None` when it did not run).
pub(crate) fn link_metadata<C, E>(
    kind: &MetadataKind,
    ctx: &mut StepCtx<'_>,
    out: &mut StepOutcome,
    resolve: impl FnOnce(Option<&FrontmatterDoc>) -> (String, C),
    ensure: impl FnOnce(&mut StepCtx<'_>, &str, C) -> Result<Ensured<E>, StepError>,
) -> Result<Option<E>, StepError> {
    let (r, extra) = link(kind, ctx, out, resolve, ensure)?;
    out.set_vars(&r);
    out.vars.insert(kind.id_var.to_owned(), r.id.into());
    Ok(extra)
}

fn link<C, E>(
    kind: &MetadataKind,
    ctx: &mut StepCtx<'_>,
    out: &mut StepOutcome,
    resolve: impl FnOnce(Option<&FrontmatterDoc>) -> (String, C),
    ensure: impl FnOnce(&mut StepCtx<'_>, &str, C) -> Result<Ensured<E>, StepError>,
) -> Result<(Linked, Option<E>), StepError> {
    let active = ctx.context.active_path.clone();
    let doc = match &active {
        Some(p) => ctx
            .read(p)?
            .map(|(b, _)| FrontmatterDoc::new(String::from_utf8_lossy(&b).into_owned())),
        None => None,
    };
    let (id, c) = resolve(doc.as_ref());
    let mut r = Linked {
        id,
        metadata_path: None,
        created: false,
        tombstone: false,
        linked: false,
    };
    if !(kind.valid)(&r.id) {
        return Ok((r, None));
    }

    // Already linked to a live metadata note for this id → done, no write.
    if let Some(d) = &doc {
        let list = d.get(kind.list_key);
        for target in link_targets(list?.as_ref()) {
            if target.rsplit('/').next() == Some(r.id.as_str())
                && ctx.read(&format!("{target}.md"))?.is_some()
            {
                r.metadata_path = Some(format!("{target}.md"));
                return Ok((r, None));
            }
        }
    }

    let e = ensure(ctx, &r.id, c)?;
    if e.output {
        out.push_output(e.path.clone());
    }
    r.created = e.created;
    r.tombstone = e.tombstone;
    if let Some(p) = active.filter(|_| doc.is_some())
        && link_onto(kind, ctx, &p, &e.path)?
    {
        out.push_output(p);
        r.linked = true;
    }
    r.metadata_path = Some(e.path);
    Ok((r, Some(e.extra)))
}

/// Appends `[[metadata_path]]` to `note`'s list through [`StepCtx::modify`] (re-read after the
/// fetch, CAS-written, so a concurrent edit is kept). `false` = already linked, or the note is
/// gone.
pub(crate) fn link_onto(
    kind: &MetadataKind,
    ctx: &mut StepCtx<'_>,
    note: &str,
    metadata_path: &str,
) -> Result<bool, StepError> {
    if ctx.read(note)?.is_none() {
        return Ok(false);
    }
    let link = format!(
        "[[{}]]",
        metadata_path.strip_suffix(".md").unwrap_or(metadata_path)
    );
    let changed = ctx.modify(note, |cur| {
        let mut d = FrontmatterDoc::new(cur.to_owned());
        Ok(append_link(&mut d, kind.anchor, kind.list_key, &link)?.then(|| d.into_text()))
    })?;
    Ok(changed.is_some())
}
