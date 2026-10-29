//! Helpers shared by several step modules:
//! the bounded concurrent fetch loop, the vault `.md` walk and the content MD5 of attachments.

use std::future::Future;

use tokio::task::JoinSet;

use super::web::block_on;
use super::{StepCtx, StepError};

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

/// Every `.md` file under `<vault_root>/<rel>` (recursive, read-only, no symlinks), as
/// vault-relative paths.
pub(crate) fn md_files_under(ctx: &StepCtx<'_>, rel: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![rel.trim_matches('/').to_owned()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(ctx.vault_root.join(&dir)) else {
            continue;
        };
        for e in rd.filter_map(Result::ok) {
            let Ok(name) = e.file_name().into_string() else {
                continue;
            };
            let Ok(ft) = e.file_type() else { continue };
            let p = format!("{dir}/{name}");
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() && name.ends_with(".md") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Every file in the vault outside the top-level `_custom/` and every dot-directory (`.trash/`,
/// `.git/`, ...), recursive, read-only, no symlinks, sorted, vault-relative; root-level files
/// included (plan remote-media-sprint WP-4, Decision 5).
pub(crate) fn vault_files(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(root.join(&dir)) else {
            continue;
        };
        for e in rd.filter_map(Result::ok) {
            let Ok(name) = e.file_name().into_string() else {
                continue;
            };
            let Ok(ft) = e.file_type() else { continue };
            let p = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            if ft.is_dir() {
                if name.starts_with('.') || (dir.is_empty() && name == "_custom") {
                    continue;
                }
                stack.push(p);
            } else if ft.is_file() {
                out.push(p);
            }
        }
    }
    out.sort();
    out
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

/// Bumps the mtime of the vault file `rel` under `root` to now (plan tidiness-trash-orphans WP-2,
/// audit B4): an editor-paste attachment reuse is not a queue job, so the `tidy` step's
/// `min_age_hours` grace period is what protects a reused-but-unreferenced file. Best effort.
pub(crate) fn touch_now(root: &std::path::Path, rel: &str) {
    let bumped = std::fs::OpenOptions::new()
        .write(true)
        .open(root.join(rel))
        .and_then(|f| f.set_modified(std::time::SystemTime::now()));
    if let Err(e) = bumped {
        tracing::warn!(error = %e, "bumping a reused attachment's mtime");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_now_bumps_an_old_mtime() {
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
        touch_now(d.path(), "a.png");
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
