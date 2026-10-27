//! `dupes_resolve` (plan `round-12-feedback-todos` WP-7, Decisions 2, 3): the user's verdict on
//! one `possible-duplicate` pair, behind the `dupes-resolve` action. Job params (strings): `keep`
//! and `other` (vault paths) and `verdict` (`not-duplicates` | `keep`). Note-mutating.
//!
//! - `not-duplicates`: each note leaves the other's `possible-duplicate` and joins its
//!   `not-duplicate-of` (set semantics, so a rescan suppresses the pair); one `dup_labels` row.
//! - `keep`: every tracker row pointing at `other` is re-pointed at `keep` **before** `other` is
//!   trashed (`delete_replaced`, so clients retarget), then `other` leaves `keep`'s
//!   `possible-duplicate` and one `dup_labels` row is written. Var `inbound_links` counts the
//!   other notes that linked to `other` by a user link ([`user_inbound_sources`]); inbound links
//!   are not rewritten.
//! - Either note missing: the dangling entry leaves the survivor's `possible-duplicate`, no label
//!   row, var `action = "stale"` (so a rerun after success is a no-op).
//!
//! Writes touch only the two label keys of the two notes, plus the trash move.

use cv_actions::{ConfigSet, StepKind};
use cv_index::LinkKind;
use cv_note::{FrontmatterDoc, JsValue};
use cv_state::DupLabel;
use cv_text::resolver::Resolver;
use std::collections::HashMap;

use serde_json::{Map, Value};

use super::dupes::{NOT_DUP_KEY, POSSIBLE_KEY, identity, post_id_options, score};
use super::{StepCtx, StepError, StepModule, StepOutcome};
use crate::now_ms;

/// The `dupes_resolve` step.
pub(crate) struct DupesResolve;

impl StepModule for DupesResolve {
    type Params = ();

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<(), String> {
        super::no_params(kind, t)
    }

    fn run((): (), ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run(ctx)
    }
}

fn param<'a>(ctx: &'a StepCtx<'_>, k: &str) -> Result<&'a str, StepError> {
    match ctx.params.get(k) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.trim()),
        _ => Err(StepError::failed(format!(
            "dupes_resolve: missing param `{k}`"
        ))),
    }
}

/// The vault's resolver plus each indexed path's id.
struct Links {
    r: Resolver,
    ids: HashMap<String, i64>,
}

fn resolver(ctx: &StepCtx<'_>) -> Result<Links, StepError> {
    let files = ctx.index()?.query().file_paths()?;
    let mut r = Resolver::default();
    for (id, p) in &files {
        r.add(*id, p);
    }
    Ok(Links {
        r,
        ids: files.into_iter().map(|(i, p)| (p, i)).collect(),
    })
}

/// Whether the label entry `entry` (in note `from`) points at `target`: by its link text equal
/// to `target` without `.md` (also covers a dangling entry), else by resolved id.
fn points_at(l: &Links, from: &str, entry: &str, target: &str) -> bool {
    let t = cv_text::wikilink::target(entry);
    if t.is_empty() {
        return false;
    }
    let bare = target.strip_suffix(".md").unwrap_or(target);
    let t_bare = t.strip_suffix(".md").unwrap_or(t);
    if t_bare == bare {
        return true;
    }
    match (l.ids.get(from), l.ids.get(target)) {
        (Some(&fid), Some(&tid)) => l.r.resolve(from, fid, t) == Some(tid),
        _ => false,
    }
}

fn wikilink(path: &str) -> JsValue {
    JsValue::Str(cv_text::wikilink::link_to(path))
}

/// Edits `path`'s label keys: drops `drop` from `possible-duplicate` and (when `add_not_dup`)
/// adds it to `not-duplicate-of` unless already there. Returns whether the note changed.
fn relabel(
    ctx: &mut StepCtx<'_>,
    r: &Links,
    path: &str,
    drop: &str,
    add_not_dup: bool,
) -> Result<bool, StepError> {
    let changed = ctx.modify(path, |old| {
        let mut d = FrontmatterDoc::new(old);
        let mut changed = false;
        let entries = d.get_strings(POSSIBLE_KEY);
        if entries.iter().any(|e| points_at(r, path, e, drop)) {
            let kept: Vec<JsValue> = entries
                .into_iter()
                .filter(|e| !points_at(r, path, e, drop))
                .map(JsValue::Str)
                .collect();
            if kept.is_empty() {
                d.remove(POSSIBLE_KEY)?;
            } else {
                d.set(POSSIBLE_KEY, &JsValue::Arr(kept))?;
            }
            changed = true;
        }
        if add_not_dup
            && !d
                .get_strings(NOT_DUP_KEY)
                .iter()
                .any(|e| points_at(r, path, e, drop))
        {
            changed |= d.append_to_list(NOT_DUP_KEY, &wikilink(drop))?;
        }
        Ok(changed.then(|| d.into_text()))
    })?;
    Ok(changed.is_some())
}

/// The other notes linking to `target` by a **user** link, per the index (plan audit E1, reused
/// by WP-8's guard): body links count; a frontmatter link counts only when it is not one of the
/// source's `possible-duplicate` / `not-duplicate-of` label entries (those are labels, not
/// links). Sorted, distinct, `target` itself excluded.
pub(crate) fn user_inbound_sources(
    ctx: &StepCtx<'_>,
    target: &str,
) -> Result<Vec<String>, StepError> {
    let rows = ctx.index()?.query().backlinks(target)?;
    let mut by_src: std::collections::BTreeMap<String, (bool, Vec<String>)> =
        std::collections::BTreeMap::new();
    for row in rows {
        if row.src_path == target {
            continue;
        }
        let e = by_src.entry(row.src_path).or_default();
        if row.kind == LinkKind::FmLink {
            e.1.push(row.target);
        } else {
            e.0 = true;
        }
    }
    let mut out = Vec::new();
    for (src, (body, mut fm)) in by_src {
        if !body {
            // Each label entry accounts for one fm-link row with its target; any left is a user link.
            let text = ctx
                .read(&src)?
                .and_then(|(b, _)| String::from_utf8(b).ok())
                .unwrap_or_default();
            let doc = FrontmatterDoc::new(text);
            for key in [POSSIBLE_KEY, NOT_DUP_KEY] {
                for entry in doc.get_strings(key) {
                    let t = cv_text::wikilink::target(&entry);
                    if let Some(i) = fm.iter().position(|f| f.trim() == t) {
                        fm.swap_remove(i);
                    }
                }
            }
            if fm.is_empty() {
                continue;
            }
        }
        out.push(src);
    }
    Ok(out)
}

fn read_text(
    ctx: &StepCtx<'_>,
    path: &str,
) -> Result<Option<(String, cv_vault::ContentHash)>, StepError> {
    Ok(ctx
        .read(path)?
        .and_then(|(b, h)| String::from_utf8(b).ok().map(|s| (s, h))))
}

fn run(ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let keep = param(ctx, "keep")?.to_owned();
    let other = param(ctx, "other")?.to_owned();
    let verdict = param(ctx, "verdict")?.to_owned();
    if verdict != "not-duplicates" && verdict != "keep" {
        return Err(StepError::failed(format!(
            "dupes_resolve: verdict must be `not-duplicates` or `keep`, got {verdict:?}"
        )));
    }
    if keep == other {
        return Err(StepError::failed(
            "dupes_resolve: keep and other are the same note",
        ));
    }
    let r = resolver(ctx)?;
    let k = read_text(ctx, &keep)?;
    let o = read_text(ctx, &other)?;
    let mut vars = Map::new();
    let (Some((k_text, _)), Some((o_text, o_hash))) = (k, o) else {
        // Stale: clean the survivor's dangling entry.
        let survivor = if ctx.read(&keep)?.is_some() {
            Some((keep.clone(), other.clone()))
        } else if ctx.read(&other)?.is_some() {
            Some((other.clone(), keep.clone()))
        } else {
            None
        };
        if let Some((s, gone)) = survivor {
            relabel(ctx, &r, &s, &gone, false)?;
        }
        vars.insert("action".into(), "stale".into());
        let mut out = StepOutcome::default();
        out.set_vars(&vars);
        return Ok(out);
    };

    let opts = post_id_options(ctx.config);
    let s = score(&k_text, &o_text, &opts);
    let label = DupLabel {
        keep_path: keep.clone(),
        other_path: other.clone(),
        keep_identity: identity(&FrontmatterDoc::new(k_text.as_str()), &opts),
        other_identity: identity(&FrontmatterDoc::new(o_text.as_str()), &opts),
        containment_keep_in_other: s.containment_a_in_b,
        containment_other_in_keep: s.containment_b_in_a,
        identity_match: s.identity_match,
        verdict: verdict.clone(),
        at_ms: now_ms(),
    };

    if verdict == "not-duplicates" {
        relabel(ctx, &r, &keep, &other, true)?;
        relabel(ctx, &r, &other, &keep, true)?;
    } else {
        let inbound = user_inbound_sources(ctx, &other)?
            .into_iter()
            .filter(|p| p != &keep)
            .count();
        // Tracker rows first, so a failed trash never leaves rows on a trashed note.
        let mut repointed = 0u32;
        {
            let mut st = ctx.state();
            for row in st.tracker_items_for_note(&other)? {
                if st.set_tracker_note_path(
                    &row.source_kind,
                    &row.source_key,
                    &row.item_id,
                    &keep,
                )? {
                    repointed += 1;
                }
            }
        }
        super::auto_file::trash(ctx, &other, o_hash, &keep)?;
        relabel(ctx, &r, &keep, &other, false)?;
        vars.insert("inbound_links".into(), inbound.into());
        vars.insert("tracker_repointed".into(), repointed.into());
    }
    ctx.state().insert_dup_label(&label)?;
    vars.insert("action".into(), verdict.into());
    let mut out = StepOutcome::default();
    out.set_vars(&vars);
    Ok(out)
}
