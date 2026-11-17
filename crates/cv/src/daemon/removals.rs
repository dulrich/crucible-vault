//! Removal review RPCs (plan `intake-removals-sources-rescan` WP-4): `resolve_removals` and
//! `next_fold_note`.
//!
//! **One writer job per resolve:** locate the id → refuse a held note → read + CAS check →
//! split frontmatter/body → the pure `cv_ingest::enrich` operation → write (frontmatter bytes
//! kept exactly) → label rows → media cleanup. Every refusal returns before the write, so it
//! changes nothing. Label rows are inserted only after the write succeeded; an insert failure
//! is logged and reported (`labels_failed`), never rolled back into the note: the user's verdict
//! on the note is the primary effect, the labels are training data.
//!
//! **Media cleanup (audit M4).** The candidates are the managed media files (`…_MD5.<media
//! ext>`, the `tidy`/`localize` managed-name rule) that the note linked from a deleted line,
//! computed in the writer job before the write. After the writer job returns, the async RPC
//! (never the writer thread) enqueues ONE job of the built-in `removals-media-cleanup` action
//! (`steps/media_cleanup.rs`, no dedupe) with them as its `paths` param and replies without
//! waiting. That job holds [`cv_actions::ATTACHMENTS_KEY`] through the queue's own resource-key
//! exclusion, so it never runs inside a localize run's selection → commit window, and re-checks
//! each file with the unchanged `tidy` `sweep_one`; its outcome vars carry the counts. An
//! enqueue failure after the write is logged (counts only) and reported (`cleanup_failed`),
//! never rolled back; the daily `tidy` sweep picks up any orphan.
//!
//! Logs carry counts only, never note text, titles or paths.

use cv_ingest::boilerplate::{line_key, normalize_line};
use cv_ingest::enrich::{
    FoldError, GroupTarget, Placement, ResolvedGroup, Verdict, fold_groups, resolve_groups,
    trim_lines,
};
use cv_protocol::{
    NextFoldNoteParams, NextFoldNoteResult, RemovalOp, RemovalRefusal, RemovalVerdict,
    RemovalsApplied, ResolveRemovalsParams, ResolveRemovalsResult,
};
use cv_state::RemovalLabel;
use cv_vault::NoteLocation;

use super::*;

/// What the pure step produced: the new body, label rows (sans note/publication), restore
/// counts and the deleted lines.
struct Applied {
    body: String,
    labels: Vec<(String, &'static str, &'static str, String)>,
    groups: u32,
    at_anchor: u32,
    at_end: u32,
    deleted: Vec<String>,
}

fn refusal_of(e: &FoldError) -> RemovalRefusal {
    match e {
        FoldError::UnknownGroup(_) => RemovalRefusal::UnknownGroup,
        FoldError::EmptyRange | FoldError::OutOfBounds { .. } => RemovalRefusal::BadRange,
        FoldError::Generated => RemovalRefusal::Generated,
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn label_lines(
    out: &mut Vec<(String, &'static str, &'static str, String)>,
    kind: &str,
    verdict: &'static str,
    origin: &'static str,
    lines: &[String],
) {
    for l in lines {
        if let Some(n) = normalize_line(l) {
            out.push((kind.to_owned(), verdict, origin, n));
        }
    }
}

/// Applies `op` to `body` (pure).
fn apply(body: &str, op: &RemovalOp) -> Result<Applied, FoldError> {
    let resolve = |target: GroupTarget, v: RemovalVerdict| {
        let verdict = match v {
            RemovalVerdict::Delete => Verdict::Delete,
            RemovalVerdict::Keep => Verdict::Keep,
        };
        resolve_groups(body, &target, verdict)
    };
    let res = match op {
        RemovalOp::Group { group, verdict } => resolve(GroupTarget::One(group.clone()), *verdict)?,
        RemovalOp::All { verdict } => resolve(GroupTarget::All, *verdict)?,
        RemovalOp::Trim { first, last, kind } => {
            let t = trim_lines(body, *first, *last)?;
            let mut labels = Vec::new();
            let kind = kind.map(|k| k.as_str()).unwrap_or_default();
            label_lines(&mut labels, kind, "delete", "selection", &t.removed);
            return Ok(Applied {
                body: t.text,
                labels,
                groups: 0,
                at_anchor: 0,
                at_end: 0,
                deleted: t.removed,
            });
        }
    };
    let mut a = Applied {
        body: res.text,
        labels: Vec::new(),
        groups: count(res.groups.len()),
        at_anchor: 0,
        at_end: 0,
        deleted: Vec::new(),
    };
    for g in &res.groups {
        let ResolvedGroup {
            kind,
            lines,
            verdict,
            placement,
            ..
        } = g;
        let kind = kind.as_ref().map(|k| k.as_str()).unwrap_or_default();
        match verdict {
            Verdict::Delete => {
                label_lines(&mut a.labels, kind, "delete", "fold", lines);
                a.deleted.extend(lines.iter().cloned());
            }
            Verdict::Keep => {
                label_lines(&mut a.labels, kind, "keep", "fold", lines);
                let n = count(lines.len());
                match placement {
                    Some(Placement::Anchor) => a.at_anchor += n,
                    Some(Placement::End) | None => a.at_end += n,
                }
            }
        }
    }
    Ok(a)
}

/// The managed media files `path` links from any of `deleted` (before the write), deduped.
fn media_candidates(vw: &cv_vault::VaultWriter, path: &str, deleted: &[String]) -> Vec<String> {
    let Ok(links) = vw.index().query().outgoing(path) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for l in links {
        let Some(target) = l.resolved_path else {
            continue;
        };
        if crate::steps::media_cleanup::is_managed_media(&target)
            && !l.raw.is_empty()
            && deleted.iter().any(|d| d.contains(&l.raw))
            && !out.contains(&target)
        {
            out.push(target);
        }
    }
    out
}

impl Shared {
    /// `resolve_removals` (module doc).
    pub(super) async fn resolve_removals(
        &self,
        p: ResolveRemovalsParams,
    ) -> Result<ResolveRemovalsResult, RpcError> {
        let holds = self.holds_view();
        let state = Arc::clone(&self.state);
        let (res, candidates) = self
            .writer_job(move |vw| {
                let refused = |reason, current_hash| {
                    Ok((
                        ResolveRemovalsResult::Refused {
                            reason,
                            current_hash,
                        },
                        Vec::new(),
                    ))
                };
                let path = match vw.locate_note(p.id).map_err(VaultError::from)? {
                    NoteLocation::Live { path } => path,
                    NoteLocation::Gone { .. } | NoteLocation::Unknown => {
                        return refused(RemovalRefusal::UnknownNote, None);
                    }
                };
                holds.refuse([path.as_str()])?;
                let Some(bytes) = vw.fs().read(&path)? else {
                    return refused(RemovalRefusal::UnknownNote, None);
                };
                let hash = ContentHash::of(&bytes);
                if hash.as_str() != p.base_hash {
                    return refused(RemovalRefusal::StaleHash, Some(hash.as_str().to_owned()));
                }
                let text = String::from_utf8(bytes)
                    .map_err(|_| JobError::Invalid("the note is not UTF-8".into()))?;
                let doc = cv_note::FrontmatterDoc::new(text);
                let body = doc.body();
                let head = &doc.text()[..doc.text().len() - body.len()];
                let a = match apply(body, &p.op) {
                    Ok(a) => a,
                    Err(e) => return refused(refusal_of(&e), None),
                };
                let publication = doc.get_str_trimmed("publication").unwrap_or_default();
                let candidates = media_candidates(vw, &path, &a.deleted);
                let new_hash = if a.body == body {
                    hash
                } else {
                    let out = format!("{head}{}", a.body);
                    vw.write(&path, out.as_bytes(), Some(hash))?
                };
                let now = now_ms();
                let rows: Vec<RemovalLabel> = a
                    .labels
                    .into_iter()
                    .map(|(kind, verdict, origin, line)| RemovalLabel {
                        note_id: i64::try_from(p.id.0).unwrap_or(i64::MAX),
                        publication: publication.clone(),
                        kind,
                        verdict: verdict.into(),
                        origin: origin.into(),
                        line_key: line_key(&line),
                        line,
                        at_ms: now,
                    })
                    .collect();
                let out = RemovalsApplied {
                    hash: new_hash.as_str().to_owned(),
                    path: path.clone(),
                    groups: a.groups,
                    lines_labelled: count(rows.len()),
                    restored_at_anchor: a.at_anchor,
                    restored_at_end: a.at_end,
                    media_queued: count(candidates.len()),
                    ..RemovalsApplied::default()
                };
                let mut out = out;
                if !rows.is_empty()
                    && let Err(e) = lock(&state).insert_removal_labels(&rows)
                {
                    tracing::warn!(error = %e, rows = rows.len(), "recording removal labels");
                    out.labels_failed = true;
                    out.lines_labelled = 0;
                }
                Ok((ResolveRemovalsResult::Applied(out), candidates))
            })
            .await?;
        let res = match res {
            ResolveRemovalsResult::Applied(mut out) => {
                if !candidates.is_empty() {
                    match self.enqueue_media_cleanup(candidates) {
                        Ok(id) => out.cleanup_job = Some(id),
                        Err(e) => {
                            tracing::warn!(error = %e, media = out.media_queued, "enqueueing the removals media cleanup");
                            out.cleanup_failed = true;
                        }
                    }
                }
                tracing::info!(
                    groups = out.groups,
                    labelled = out.lines_labelled,
                    media_queued = out.media_queued,
                    cleanup_failed = out.cleanup_failed,
                    "removals resolved"
                );
                ResolveRemovalsResult::Applied(out)
            }
            r @ ResolveRemovalsResult::Refused { .. } => r,
        };
        Ok(res)
    }

    /// Enqueues the built-in media cleanup job over `paths` (module doc); its job id.
    fn enqueue_media_cleanup(&self, paths: Vec<String>) -> Result<String, String> {
        let runner = self.runner().ok_or("runner not started")?;
        let config = runner.config();
        let action = config
            .armed(cv_actions::MEDIA_CLEANUP_ACTION)
            .ok_or("the built-in media cleanup action is not armed")?;
        let mut params = serde_json::Map::new();
        params.insert(
            "paths".into(),
            serde_json::Value::Array(paths.into_iter().map(Into::into).collect()),
        );
        let req = crate::runner::RunRequest {
            params,
            context: cv_protocol::ActionContext::default(),
            trigger: crate::runner::Trigger::Command,
            chain: Vec::new(),
            default_lane: cv_protocol::Lane::Background,
            dedupe_key: None,
        };
        runner.enqueue(action, req).map_err(|e| e.to_string())
    }

    /// `next_fold_note`: index candidates (notes with a `removed` callout, path order, no
    /// `.trash/`), each confirmed by parsing its body for a fold group; the first after the
    /// current note's path, wrapping, never the current note.
    pub(super) async fn next_fold_note(
        &self,
        p: NextFoldNoteParams,
    ) -> Result<NextFoldNoteResult, RpcError> {
        self.writer_job(move |vw| {
            let current = match p.id {
                Some(id) => match vw.locate_note(id).map_err(VaultError::from)? {
                    NoteLocation::Live { path } => Some(path),
                    _ => p.path,
                },
                None => p.path,
            };
            let candidates = cv_index::notes_with_callout(&vw.index().query(), "removed")
                .map_err(|e| JobError::Invalid(e.to_string()))?;
            let split = current
                .as_deref()
                .map_or(0, |c| candidates.partition_point(|x| x.as_str() <= c));
            let order = candidates[split..].iter().chain(&candidates[..split]);
            for path in order {
                if current.as_deref() == Some(path.as_str()) {
                    continue;
                }
                let Some(bytes) = vw.fs().read(path)? else {
                    continue;
                };
                let text = String::from_utf8_lossy(&bytes);
                let doc = cv_note::FrontmatterDoc::new(text.into_owned());
                if !fold_groups(doc.body()).is_empty() {
                    return Ok(NextFoldNoteResult {
                        path: Some(path.clone()),
                    });
                }
            }
            Ok(NextFoldNoteResult { path: None })
        })
        .await
    }
}

#[cfg(test)]
#[path = "removals_tests.rs"]
mod tests;
