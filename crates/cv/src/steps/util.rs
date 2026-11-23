//! Helpers shared by several step modules:
//! the bounded concurrent fetch loop, the vault `.md` walk and the content MD5 of attachments.

use std::future::Future;

use tokio::task::JoinSet;

use super::web::block_on;
use super::{StepCtx, StepError};

/// A tag in comparison form: trimmed, leading `#` dropped, lowercased.
pub(crate) fn norm_tag(s: &str) -> String {
    s.trim().trim_start_matches('#').to_lowercase()
}

/// The note's tags as the index records them ([`cv_index::note_tags`]: ofm's frontmatter + inline
/// tags, never inside code — SR11), normalized by [`norm_tag`] for [`has_tag`].
pub(crate) fn note_tags(text: &str) -> Vec<String> {
    cv_index::note_tags(text)
        .iter()
        .map(|t| norm_tag(t))
        .collect()
}

/// Whether `tags` (from [`note_tags`]) include `want` or a nested child of it (`want/…`), the
/// index's `tagged` prefix rule, case-insensitive. The one tag-membership test: the guard and
/// `transcript_refine::is_refined` both use it.
pub(crate) fn has_tag(tags: &[String], want: &str) -> bool {
    let w = norm_tag(want);
    tags.iter()
        .any(|t| *t == w || t.strip_prefix(&w).is_some_and(|r| r.starts_with('/')))
}

/// Fetches in flight per run.
pub const MAX_IN_FLIGHT: usize = 4;

/// Runs `f` over `inputs` with at most [`MAX_IN_FLIGHT`] futures in flight on the current
/// runtime; results come back in input order.
pub(crate) fn fetch_bounded<I, T, F, Fut>(inputs: Vec<I>, f: F) -> Result<Vec<T>, StepError>
where
    F: Fn(I) -> Fut,
    Fut: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let n = inputs.len();
    block_on(async move {
        let mut set = JoinSet::new();
        let mut out: Vec<Option<T>> = (0..n).map(|_| None).collect();
        let mut it = inputs.into_iter().enumerate();
        loop {
            while set.len() < MAX_IN_FLIGHT {
                let Some((i, input)) = it.next() else { break };
                let fut = f(input);
                set.spawn(async move { (i, fut.await) });
            }
            let Some(r) = set.join_next().await else {
                break;
            };
            let (i, v) = r.map_err(|e| StepError::failed(format!("fetch task: {e}")))?;
            out[i] = Some(v);
        }
        out.into_iter()
            .map(|v| v.ok_or_else(|| StepError::failed("fetch task lost")))
            .collect()
    })?
}

/// The vault-wide listing discovery of `root` (plan folder-links-follow Decision 5: folder links
/// followed, one owner per canonical directory), pruning excluded directories `prune` accepts. A
/// failed walk is logged and lists nothing.
fn listing(root: &std::path::Path, prune: &dyn Fn(&str) -> bool) -> Vec<cv_index::DiscoveredFile> {
    match cv_index::discover_listing(root, prune) {
        Ok(d) => d.files,
        Err(e) => {
            tracing::warn!(error = %e, "vault listing failed");
            Vec::new()
        }
    }
}

/// Every `.md` file under `<vault_root>/<rel>` (recursive, read-only, no file links), as
/// vault-relative paths: the vault-wide discovery filtered to `rel` (folder links followed under
/// Decision 5; ignored files still listed).
pub(crate) fn md_files_under(ctx: &StepCtx<'_>, rel: &str) -> Vec<String> {
    let rel = rel.trim_matches('/');
    let under = |p: &str| {
        rel.is_empty()
            || p.strip_prefix(rel)
                .is_some_and(|r| r.is_empty() || r.starts_with('/'))
    };
    // Only excluded directories outside `rel`'s subtree (and not on its way) are pruned.
    let prune = |d: &str| !(under(d) || rel.starts_with(&format!("{d}/")));
    listing(ctx.vault_root, &prune)
        .into_iter()
        .filter(|f| !f.file_link && f.path.ends_with(".md") && under(&f.path) && f.path != rel)
        .map(|f| f.path)
        .collect()
}

/// Every file in the vault outside the top-level `_custom/` and every dot-directory (`.trash/`,
/// `.git/`, ...), recursive, read-only, no file links, sorted, vault-relative; root-level files
/// included (plan remote-media-sprint WP-4, Decision 5). Folder links are followed under plan
/// folder-links-follow Decision 5 (the vault-wide discovery, filtered). Deliberately not
/// [`cv_text::vpath::is_user_path`]: that predicate also skips every other `_`-prefixed top
/// folder (`_yt_metadata`, …), which this walk must include, and it never skips a nested
/// dot-directory the way this walk does — the two rules differ, so they stay separate.
pub(crate) fn vault_files(root: &std::path::Path) -> Vec<String> {
    let dot_dir = |p: &str| p.split('/').any(|c| c.starts_with('.'));
    listing(root, &dot_dir)
        .into_iter()
        .filter(|f| {
            let dir = f.path.rsplit_once('/').map_or("", |(d, _)| d);
            !f.file_link && !dot_dir(dir) && !f.path.starts_with("_custom/")
        })
        .map(|f| f.path)
        .collect()
}

/// The names of the direct child folders of vault folder `root`, sorted: its physical
/// subdirectories plus every folder link under it that owns its target (plan folder-links-follow
/// Decision 5; a losing alias or loop is never listed).
pub(crate) fn child_dirs(vault_root: &std::path::Path, root: &str) -> Vec<String> {
    let root = root.trim_matches('/');
    let Ok(rd) = std::fs::read_dir(vault_root.join(root)) else {
        return Vec::new();
    };
    let mut dirs: Vec<String> = rd
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    match cv_index::discover(vault_root) {
        Ok(d) => dirs.extend(d.ownership.dirs().filter_map(|(dir, _)| {
            let (parent, name) = dir.rsplit_once('/').unwrap_or(("", dir));
            (parent == root && !dir.is_empty()).then(|| name.to_string())
        })),
        Err(e) => tracing::warn!(error = %e, "vault discovery failed; folder links not listed"),
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

/// [`vault_files`] restricted to `.md` notes (the whole-vault `localize` scope).
pub(crate) fn md_files_vault(root: &std::path::Path) -> Vec<String> {
    let mut v = vault_files(root);
    v.retain(|p| p.ends_with(".md"));
    v
}

/// Crucible `md5HexForBytes` (deterministic content MD5 naming `_MD5` attachments,
/// `src/AGENTS.md:23,49`); shared by the `localize` step and the daemon's `write_attachment` RPC.
pub(crate) fn md5_hex(bytes: &[u8]) -> String {
    use md5::Digest;
    hex::encode(md5::Md5::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuse_touch_bumps_an_old_mtime() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.png");
        std::fs::write(&p, "x").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(old)
            .unwrap();
        // The reuse touch is confined `VaultFs::touch` (plan folder-links-follow WP-1).
        cv_vault::VaultFs::live(d.path())
            .unwrap()
            .touch("a.png")
            .unwrap();
        let age = std::time::SystemTime::now()
            .duration_since(p.metadata().unwrap().modified().unwrap())
            .unwrap();
        assert!(age < std::time::Duration::from_secs(60));
    }

    #[test]
    fn md5_hex_handles_empty_and_numeric_bytes() {
        assert_eq!(md5_hex(&[]), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(
            md5_hex(&[1, 2, 3, 4, 5]),
            "7cfdd07889b3295d6a550914ab35e068"
        );
    }
}
