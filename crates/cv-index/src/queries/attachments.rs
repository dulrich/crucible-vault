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
use crate::query::{LinkKind, Query};

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

/// Whether any indexed link row (all kinds) from a source other than `note` resolves to
/// `target` (plan editor-ui-round-3 WP-3). Links from non-indexed files do not count.
pub fn referenced_elsewhere(q: &Query<'_>, target: &str, note: &str) -> Result<bool> {
    Ok(q.backlinks(target)?.iter().any(|l| l.src_path != note))
}

/// The `delete_preview` items of `note` as `(path, referenced_elsewhere)`, in first-link order:
/// the note's outgoing embeds plus its non-note link targets that resolve to an indexed file.
/// Unresolved (missing) targets and self-links are not listed.
pub fn delete_preview(q: &Query<'_>, note: &str) -> Result<Vec<(String, bool)>> {
    let mut out: Vec<(String, bool)> = Vec::new();
    for l in q.outgoing(note)? {
        let Some(target) = l.resolved_path else {
            continue;
        };
        let is_note = target.to_ascii_lowercase().ends_with(".md");
        if target == note || (is_note && l.kind != LinkKind::Embed) {
            continue;
        }
        if out.iter().any(|(p, _)| *p == target) {
            continue;
        }
        let shared = referenced_elsewhere(q, &target, note)?;
        out.push((target, shared));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Index;

    /// A temp root holding `vault/` and the index (removed on drop).
    struct Fx(tempfile::TempDir);
    impl Fx {
        fn new() -> Self {
            let d = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(d.path().join("vault")).unwrap();
            Self(d)
        }
        fn path(&self) -> &std::path::Path {
            self.0.path()
        }
        fn write(&self, rel: &str, content: &str) {
            let p = self.path().join("vault").join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
    }

    #[test]
    fn orphans_are_unlinked_managed_media_in_path_order() {
        let t = Fx::new();
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
        let idx = Index::create(&t.path().join("vault"), &t.path().join("index.db")).unwrap();
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

    #[test]
    fn delete_preview_classifies_orphan_shared_missing() {
        let t = Fx::new();
        t.write("img/own.png", "png");
        t.write("img/shared.png", "png");
        t.write("doc.pdf", "pdf");
        t.write("other.md", "body");
        t.write("emb.md", "embedded");
        t.write(
            "n.md",
            "![[own.png]] ![[shared.png]] ![[gone.png]] [[other]] ![[emb]] [d](doc.pdf) ![[own.png]] [[n]]\n",
        );
        t.write("m.md", "uses ![[shared.png]]\n");
        let idx = Index::create(&t.path().join("vault"), &t.path().join("index.db")).unwrap();
        let q = idx.query();
        let items = delete_preview(&q, "n.md").unwrap();
        assert_eq!(
            items,
            vec![
                ("img/own.png".to_owned(), false),
                ("img/shared.png".to_owned(), true),
                ("emb.md".to_owned(), false),
                ("doc.pdf".to_owned(), false),
            ]
        );
        assert!(referenced_elsewhere(&q, "img/shared.png", "n.md").unwrap());
        assert!(!referenced_elsewhere(&q, "img/own.png", "n.md").unwrap());
    }
}
