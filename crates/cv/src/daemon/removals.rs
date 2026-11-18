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
    NextFoldNoteParams, NextFoldNoteResult, NoteId, RemovalOp, RemovalOrigin, RemovalRefusal,
    RemovalVerdict, RemovalsApplied, ResolveRemovalsParams, ResolveRemovalsResult,
};
use cv_state::RemovalLabel;
use cv_vault::NoteLocation;

use super::*;

/// What the pure step produced: the new body, label rows (sans note/publication), restore
/// counts and the deleted lines.
struct Applied {
    body: String,
    labels: Vec<Labelled>,
    groups: u32,
    at_anchor: u32,
    at_end: u32,
    deleted: Vec<String>,
}

/// One label row before the note and publication are known.
struct Labelled {
    kind: String,
    verdict: RemovalVerdict,
    origin: RemovalOrigin,
    line: String,
}

fn refusal_of(e: &FoldError) -> RemovalRefusal {
    match e {
        FoldError::UnknownGroup(_) => RemovalRefusal::UnknownGroup,
        FoldError::EmptyRange | FoldError::Unordered | FoldError::OutOfBounds { .. } => {
            RemovalRefusal::BadRange
        }
        FoldError::Generated => RemovalRefusal::Generated,
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn label_lines(
    out: &mut Vec<Labelled>,
    kind: &str,
    verdict: RemovalVerdict,
    origin: RemovalOrigin,
    lines: &[String],
) {
    for l in lines {
        if let Some(line) = normalize_line(l) {
            out.push(Labelled {
                kind: kind.to_owned(),
                verdict,
                origin,
                line,
            });
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
            label_lines(
                &mut labels,
                kind,
                RemovalVerdict::Delete,
                RemovalOrigin::Selection,
                &t.removed,
            );
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
                label_lines(
                    &mut a.labels,
                    kind,
                    RemovalVerdict::Delete,
                    RemovalOrigin::Fold,
                    lines,
                );
                a.deleted.extend(lines.iter().cloned());
            }
            Verdict::Keep => {
                label_lines(
                    &mut a.labels,
                    kind,
                    RemovalVerdict::Keep,
                    RemovalOrigin::Fold,
                    lines,
                );
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

/// What one resolve writer job decided: a refusal that changed nothing, or an applied result
/// with the media cleanup candidates the RPC tail enqueues.
enum Resolved {
    Refused(ResolveRemovalsResult),
    Applied {
        out: RemovalsApplied,
        candidates: Vec<String>,
    },
}

fn refused(reason: RemovalRefusal, current_hash: Option<String>) -> Resolved {
    Resolved::Refused(ResolveRemovalsResult::Refused {
        reason,
        current_hash,
    })
}

/// The note `id`'s live path, or none for a gone or unknown id.
pub(super) fn locate_live(
    vw: &mut cv_vault::VaultWriter,
    id: NoteId,
) -> Result<Option<String>, JobError> {
    Ok(match vw.locate_note(id).map_err(VaultError::from)? {
        NoteLocation::Live { path } => Some(path),
        NoteLocation::Gone { .. } | NoteLocation::Unknown => None,
    })
}

/// The writer job of one resolve: locate, hold check, compare-and-swap, apply, write, label
/// insert, result assembly. Every refusal returns before the write.
fn resolve_in_writer(
    vw: &mut cv_vault::VaultWriter,
    holds: &crate::runner::HoldsView,
    state: &Mutex<StateDb>,
    p: &ResolveRemovalsParams,
) -> Result<Resolved, JobError> {
    let Some(path) = locate_live(vw, p.id)? else {
        return Ok(refused(RemovalRefusal::UnknownNote, None));
    };
    holds.refuse([path.as_str()])?;
    let Some(bytes) = vw.fs().read(&path)? else {
        return Ok(refused(RemovalRefusal::UnknownNote, None));
    };
    let hash = ContentHash::of(&bytes);
    if hash.as_str() != p.base_hash {
        return Ok(refused(
            RemovalRefusal::StaleHash,
            Some(hash.as_str().to_owned()),
        ));
    }
    let text =
        String::from_utf8(bytes).map_err(|_| JobError::Invalid("the note is not UTF-8".into()))?;
    let doc = cv_note::FrontmatterDoc::new(text);
    let body = doc.body();
    let a = match apply(body, &p.op) {
        Ok(a) => a,
        Err(e) => return Ok(refused(refusal_of(&e), None)),
    };
    let candidates = media_candidates(vw, &path, &a.deleted);
    let new_hash = if a.body == body {
        hash
    } else {
        let head = &doc.text()[..doc.text().len() - body.len()];
        vw.write(&path, format!("{head}{}", a.body).as_bytes(), Some(hash))?
    };
    let publication = doc.get_str_trimmed("publication").unwrap_or_default();
    let rows = label_rows(p.id, &publication, a.labels);
    let mut out = RemovalsApplied {
        hash: new_hash.as_str().to_owned(),
        path,
        groups: a.groups,
        lines_labelled: count(rows.len()),
        restored_at_anchor: a.at_anchor,
        restored_at_end: a.at_end,
        media_queued: count(candidates.len()),
        ..RemovalsApplied::default()
    };
    insert_labels(state, &rows, &mut out);
    Ok(Resolved::Applied { out, candidates })
}

/// The stored rows of `labels` for the note `id` in `publication`.
fn label_rows(id: NoteId, publication: &str, labels: Vec<Labelled>) -> Vec<RemovalLabel> {
    let now = now_ms();
    labels
        .into_iter()
        .map(|l| RemovalLabel {
            note_id: i64::try_from(id.0).unwrap_or(i64::MAX),
            publication: publication.to_owned(),
            kind: l.kind,
            verdict: l.verdict,
            origin: l.origin,
            line_key: line_key(&l.line),
            line: l.line,
            at_ms: now,
        })
        .collect()
}

/// Inserts `rows` after the note write; a failure is logged and reported, the write stands.
fn insert_labels(state: &Mutex<StateDb>, rows: &[RemovalLabel], out: &mut RemovalsApplied) {
    if !rows.is_empty()
        && let Err(e) = lock(state).insert_removal_labels(rows)
    {
        tracing::warn!(error = %e, rows = rows.len(), "recording removal labels");
        out.labels_failed = true;
        out.lines_labelled = 0;
    }
}

impl Shared {
    /// `resolve_removals` (module doc).
    pub(super) async fn resolve_removals(
        &self,
        p: ResolveRemovalsParams,
    ) -> Result<ResolveRemovalsResult, RpcError> {
        let holds = self.holds_view();
        let state = Arc::clone(&self.state);
        let resolved = self
            .writer_job(move |vw| resolve_in_writer(vw, &holds, &state, &p))
            .await?;
        let (mut out, candidates) = match resolved {
            Resolved::Refused(r) => return Ok(r),
            Resolved::Applied { out, candidates } => (out, candidates),
        };
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
        Ok(ResolveRemovalsResult::Applied(out))
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
    /// `.trash/`) are listed in one writer job; each is then read and confirmed by parsing its
    /// body for a fold group off the executor. The first after the `after` note's path,
    /// wrapping, never that note; a gone or unknown `after` starts from the first.
    pub(super) async fn next_fold_note(
        &self,
        p: NextFoldNoteParams,
    ) -> Result<NextFoldNoteResult, RpcError> {
        let (root, current, candidates) = self
            .writer_job(move |vw| {
                let current = match p.after {
                    Some(id) => locate_live(vw, id)?,
                    None => None,
                };
                let candidates = cv_index::notes_with_callout(&vw.index().query(), "removed")
                    .map_err(|e| JobError::Invalid(e.to_string()))?;
                Ok((vw.fs().root().to_path_buf(), current, candidates))
            })
            .await?;
        tokio::task::spawn_blocking(move || {
            let split = current
                .as_deref()
                .map_or(0, |c| candidates.partition_point(|x| x.as_str() <= c));
            let path = candidates[split..]
                .iter()
                .chain(&candidates[..split])
                .filter(|path| current.as_deref() != Some(path.as_str()))
                .find(|path| has_fold(&root, path))
                .cloned();
            NextFoldNoteResult { path }
        })
        .await
        .map_err(internal)
    }
}

/// Whether the vault note `rel` under `root` still has a fold group.
fn has_fold(root: &Path, rel: &str) -> bool {
    if cv_vault::validate_rel_path(rel).is_err() {
        return false;
    }
    let Ok(bytes) = std::fs::read(root.join(rel)) else {
        return false;
    };
    let doc = cv_note::FrontmatterDoc::new(String::from_utf8_lossy(&bytes).into_owned());
    !fold_groups(doc.body()).is_empty()
}

#[cfg(test)]
#[path = "removals_tests.rs"]
mod tests;
