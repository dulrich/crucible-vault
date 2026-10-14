//! Attachment queries (plan `v0-core-daemon` WP-11): the orphaned-attachment scan and the vault
//! path snapshot the `localize` step's repair resolves against.
//!
//! Ported from Crucible `src/ingestion/data/orphanedAttachments.ts` (`computeOrphanedAttachmentRows`):
//! an orphan is a *managed* attachment (`…_MD5.<ext>` name, Crucible `src/ingestion/AGENTS.md:19`:
//! "keys off the `_MD5` name convention, not a localize folder") of a localizable media type with
//! no reference from any note. Dropped (Obsidian-induced): Crucible unions
//! `metadataCache.resolvedLinks` with separately resolved `frontmatterLinks` because the cache
//! omits property links; here every link kind (body, embed, md, fm-link) is one `links` row, so
//! "no resolved incoming link" is a single `NOT EXISTS` on `links.resolved_id`.

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::query::Query;

/// One orphaned managed attachment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanRow {
    /// Vault path.
    pub path: String,
    /// Parent folder (`""` = vault root).
    pub folder: String,
    /// Size in bytes.
    pub size: i64,
    /// Modification time (ms since epoch, as indexed).
    pub mtime: i64,
}

/// Crucible `OBSIDIAN_NATIVE_EMBED_FORMATS` (`src/types.ts:33`), all four localize media types:
/// `classifyLocalizeMediaType` returns non-null exactly for these extensions.
const MEDIA_EXTS: &[&str] = &[
    "avif", "bmp", "gif", "jpeg", "jpg", "png", "svg", "webp", // images
    "flac", "m4a", "mp3", "ogg", "wav", "webm", "3gp", // audio
    "mkv", "mov", "mp4", "ogv", // video
    "pdf",
];

/// Crucible `MD5_NAME_RE` (`/_MD5\.[A-Za-z0-9]+$/`) on a split (`name`, `ext`) pair.
fn is_managed(name: &str, ext: &str) -> bool {
    name.ends_with("_MD5") && !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Managed attachments (`…_MD5.<media ext>`, anywhere in the vault) with **no resolved incoming
/// link** of any kind, ordered by path (deterministic). Read-only: nothing is deleted.
pub fn orphaned_attachments(q: &Query<'_>) -> Result<Vec<OrphanRow>> {
    let mut stmt = q.conn().prepare_cached(
        "SELECT f.path, f.folder, f.name, f.ext, f.size, f.mtime FROM files f \
         WHERE f.is_md = 0 AND f.name LIKE '%\\_MD5' ESCAPE '\\' \
         AND NOT EXISTS (SELECT 1 FROM links l WHERE l.resolved_id = f.id) \
         ORDER BY f.path",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            OrphanRow {
                path: r.get(0)?,
                folder: r.get(1)?,
                size: r.get(4)?,
                mtime: r.get(5)?,
            },
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (orphan, name, ext) = row?;
        if is_managed(&name, &ext) && MEDIA_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
            out.push(orphan);
        }
    }
    Ok(out)
}

/// Every indexed vault path, sorted (Crucible `app.vault.getFiles().map(f => f.path)`, the input
/// of `buildAttachmentPathIndex` in `repairNote`).
pub fn attachment_vault_paths(q: &Query<'_>) -> Result<Vec<String>> {
    let mut stmt = q
        .conn()
        .prepare_cached("SELECT path FROM files ORDER BY path")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::Index;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "cv-index-attach-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(p.join("vault")).unwrap();
            Self(p)
        }
        fn write(&self, rel: &str, content: &str) {
            let p = self.0.join("vault").join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn orphans_are_unlinked_managed_media_in_path_order() {
        let t = Tmp::new();
        t.write("a/_attachments/n/aaa_MD5.png", "png");
        t.write("z/zzz_MD5.webp", "webp-bytes");
        t.write("b/bbb_MD5.png", "png");
        t.write("c/ccc_MD5.pdf", "pdf");
        t.write("d/plain.png", "png"); // not managed
        t.write("e/eee_MD5.txt", "txt"); // not a media type
        t.write("cover/cov_MD5.jpg", "jpg"); // linked from frontmatter only
        t.write(
            "n.md",
            "---\ncover: \"[[cov_MD5.jpg]]\"\n---\n![[bbb_MD5.png]] [doc](c/ccc_MD5.pdf)\n",
        );
        let idx = Index::create(&t.0.join("vault"), &t.0.join("index.db")).unwrap();
        let q = idx.query();
        let rows = orphaned_attachments(&q).unwrap();
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, ["a/_attachments/n/aaa_MD5.png", "z/zzz_MD5.webp"]);
        assert_eq!(rows[1].size, 10);
        assert_eq!(rows[1].folder, "z");
        // Deterministic: same answer twice.
        assert_eq!(orphaned_attachments(&q).unwrap(), rows);
        let all = attachment_vault_paths(&q).unwrap();
        assert!(all.windows(2).all(|w| w[0] < w[1]));
        assert!(all.contains(&"n.md".to_owned()));
    }
}
