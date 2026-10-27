//! File preparation: everything the chunker needs from one note, without touching the companion
//! (port of `https://github.com/dulrich/obsidian-crucible/blob/master/src/search/filePreparation.ts:63-295`).
//!
//! The two facets folded into `contentHash` are reproduced exactly:
//! - **image descriptions** (`resolveImageDescriptions`, `:101-160`): the note's local embeds,
//!   resolved through the `_MD5` naming convention (`localizedImageInfo`,
//!   `src/orchestration/utils/imageMetadata.ts:16-28`) to description records, deduplicated and
//!   sorted by md5; `kind: failed` and empty records emit nothing; the facet is
//!   `image-desc:<combinedDescriptionHash(emitted)>` (`imageDescriptionStore.ts:217-227`);
//! - **linked posts** (`resolveLinkedDocuments`, `:186-220`): `x-metadata` then `yt-metadata`
//!   stamps, capped at 8 *before* resolution, each read, frontmatter-stripped, trimmed and capped
//!   at 4000 chars; the facet is `linked:<path>:<hashSearchContent(text)>`.
//!
//! Dropped (Obsidian-induced): `metadataCache` (the host supplies embeds and frontmatter stamps
//! from `index.db`), `getFirstLinkpathDest` (the host resolves links through the index),
//! `vault.cachedRead` and the `Promise.all` concurrency — resolution here is sequential, which
//! the TS doc (`:226-242`) states yields byte-identical output. Not ported: Crucible's
//! settings-driven exclusion predicate (`isPathExcluded` + `isUserIgnored`) — the caller decides
//! indexability; and `normalizePath`'s NFC step on an *unresolved* embed link (resolved embeds
//! use the vault path, which is what Crucible uses too).

use cv_state::ImageDescription;
use serde_json::Value;

use crate::chunker::{
    BuildChunksInput, ImageDescriptionChunkInput, LinkedDocumentChunkInput,
    MAX_LINKED_DOCUMENTS_PER_NOTE, SearchChunk, build_search_chunks, hash_search_content,
    hash_string, strip_frontmatter_block, take_chars,
};

/// `LINKED_DOCUMENT_MAX_CHARS` (`filePreparation.ts:20`), in chars.
pub const LINKED_DOCUMENT_MAX_CHARS: usize = 4000;

/// One embed of the note (Obsidian `metadataCache.embeds[]`): the link text and, when the vault
/// resolves it, the destination path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedRef {
    /// The link as written (no alias).
    pub link: String,
    /// The resolved vault path, if any.
    pub resolved: Option<String>,
}

/// A resolved linked note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedSource {
    /// Vault path.
    pub path: String,
    /// Basename (file name without extension).
    pub basename: String,
    /// Raw content.
    pub content: String,
}

/// What file preparation reads from its host.
pub trait PrepareHost {
    /// The description record for `md5` (Crucible `store.has` + `store.get`).
    fn image_description(&self, md5: &str) -> Option<ImageDescription>;
    /// Resolves a linkpath from the note at `from` and reads the target.
    fn resolve_linked(&self, linkpath: &str, from: &str) -> Option<LinkedSource>;
}

/// The note-side inputs.
#[derive(Debug, Clone, Default)]
pub struct PrepareInput {
    /// Note path.
    pub path: String,
    /// Note content.
    pub content: String,
    /// Embeds in source order.
    pub embeds: Vec<EmbedRef>,
    /// Frontmatter `x-metadata` value (parsed YAML as JSON), if present.
    pub x_metadata: Option<Value>,
    /// Frontmatter `yt-metadata` value, if present.
    pub yt_metadata: Option<Value>,
}

/// `PreparedSearchFile` (`filePreparation.ts:43-61`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedFile {
    /// Note path.
    pub path: String,
    /// Note content.
    pub content: String,
    /// The folded `contentHash`.
    pub content_hash: String,
    /// Image descriptions (md5 order).
    pub image_descriptions: Vec<ImageDescriptionChunkInput>,
    /// Linked documents (stamp order).
    pub linked_documents: Vec<LinkedDocumentChunkInput>,
    /// Facets folded into `content_hash`.
    pub hash_facets: Vec<String>,
}

/// `localizedImageInfo` → `(md5, basename)`: `([a-f0-9]{32})_MD5\.([A-Za-z0-9]+)$` on the
/// file name, case-insensitive. Kept apart from `cv_text::attachments::is_md5_name` on purpose:
/// that predicate is case-sensitive (`_MD5.`) and does not extract or length-check the hash, and
/// `cv-search` has no `cv-text` edge.
#[must_use]
pub fn localized_image_md5(path: &str) -> Option<(String, String)> {
    let normalized: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let name = (*normalized.last()?).to_owned();
    let (stem, ext) = name.rsplit_once('.')?;
    if ext.is_empty() || !ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let lower = stem.to_ascii_lowercase();
    let head = lower.strip_suffix("_md5")?;
    if head.len() < 32 || !head.is_char_boundary(head.len() - 32) {
        return None;
    }
    let md5 = &head[head.len() - 32..];
    md5.bytes()
        .all(|b| b.is_ascii_hexdigit())
        .then(|| (md5.to_owned(), name.clone()))
}

/// `combinedDescriptionHash` (`imageDescriptionStore.ts:217-227`) over `(md5, hash)` pairs.
#[must_use]
pub fn combined_description_hash(pairs: &[(String, String)]) -> String {
    let mut sorted: Vec<&(String, String)> = pairs.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    sorted.dedup_by(|a, b| a.0 == b.0);
    if sorted.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = sorted.iter().map(|(m, h)| format!("{m}:{h}")).collect();
    hash_string(&lines.join("\n"))
}

fn resolve_images(
    host: &dyn PrepareHost,
    input: &PrepareInput,
) -> (Vec<ImageDescriptionChunkInput>, Vec<String>) {
    let mut md5s: Vec<(String, String)> = Vec::new();
    for embed in &input.embeds {
        let link = embed.link.as_str();
        let lower = link.to_ascii_lowercase();
        if link.is_empty() || lower.starts_with("http://") || lower.starts_with("https://") {
            continue;
        }
        let target = embed.resolved.as_deref().unwrap_or(link);
        let Some((md5, filename)) = localized_image_md5(target) else {
            continue;
        };
        if md5s.iter().any(|(m, _)| *m == md5) {
            continue;
        }
        if host.image_description(&md5).is_none() {
            continue;
        }
        md5s.push((md5, filename));
    }
    md5s.sort_by(|a, b| a.0.cmp(&b.0));
    let mut descriptions = Vec::new();
    let mut emitted = Vec::new();
    for (md5, filename) in md5s {
        let Some(record) = host.image_description(&md5) else {
            continue;
        };
        if record.kind == "failed" {
            continue;
        }
        let narrative = record.narrative.trim().to_owned();
        let extraction = record.extraction.trim().to_owned();
        if narrative.is_empty() && extraction.is_empty() {
            continue;
        }
        descriptions.push(ImageDescriptionChunkInput {
            filename,
            narrative,
            extraction,
        });
        emitted.push((md5, record.description_hash));
    }
    if descriptions.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let facet = format!("image-desc:{}", combined_description_hash(&emitted));
    (descriptions, vec![facet])
}

/// `stripWikilink` (`filePreparation.ts:95-98`). `cv-search` has no `cv-text` edge, so this keeps a local copy of the canonical form `cv_text::wikilink::target`.
fn strip_wikilink(raw: &str) -> String {
    let t = raw.trim();
    let t = t.strip_prefix("[[").unwrap_or(t);
    let t = t.strip_suffix("]]").unwrap_or(t);
    let t = t.split('|').next().unwrap_or("");
    t.split('#').next().unwrap_or("").trim().to_owned()
}

/// `collectLinkedMetadataLinkpaths` (`filePreparation.ts:71-87`).
#[must_use]
pub fn linked_metadata_linkpaths(x: Option<&Value>, yt: Option<&Value>) -> Vec<String> {
    let mut raw: Vec<&str> = Vec::new();
    for v in [x, yt].into_iter().flatten() {
        match v {
            Value::Array(items) => raw.extend(
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|s| !s.trim().is_empty()),
            ),
            Value::String(s) if !s.trim().is_empty() => raw.push(s),
            _ => {}
        }
    }
    raw.into_iter()
        .map(strip_wikilink)
        .filter(|s| !s.is_empty())
        .collect()
}

fn resolve_linked(
    host: &dyn PrepareHost,
    input: &PrepareInput,
) -> (Vec<LinkedDocumentChunkInput>, Vec<String>) {
    let mut documents = Vec::new();
    let mut facets = Vec::new();
    let paths = linked_metadata_linkpaths(input.x_metadata.as_ref(), input.yt_metadata.as_ref());
    for linkpath in paths.iter().take(MAX_LINKED_DOCUMENTS_PER_NOTE) {
        let Some(dest) = host.resolve_linked(linkpath, &input.path) else {
            continue;
        };
        let body = strip_frontmatter_block(&dest.content).trim();
        let text = take_chars(body, LINKED_DOCUMENT_MAX_CHARS).to_owned();
        if text.is_empty() {
            continue;
        }
        facets.push(format!(
            "linked:{}:{}",
            dest.path,
            hash_search_content(&text, &[])
        ));
        documents.push(LinkedDocumentChunkInput {
            path: dest.path,
            title: dest.basename,
            text,
        });
    }
    (documents, facets)
}

/// `prepareSearchFile` (`filePreparation.ts:244-269`): images then linked, in that fixed order.
#[must_use]
pub fn prepare_search_file(host: &dyn PrepareHost, input: &PrepareInput) -> PreparedFile {
    let content = input.content.clone();
    let (images, image_facets) = resolve_images(host, input);
    let (linked, linked_facets) = resolve_linked(host, input);
    let mut hash_facets = image_facets;
    hash_facets.extend(linked_facets);
    PreparedFile {
        path: input.path.clone(),
        content_hash: hash_search_content(&content, &hash_facets),
        content,
        image_descriptions: images,
        linked_documents: linked,
        hash_facets,
    }
}

/// `buildPreparedSearchFileChunks` (`filePreparation.ts:276-295`).
#[must_use]
pub fn build_prepared_chunks(
    prepared: &PreparedFile,
    vault_id: &str,
    basename: &str,
    mtime: i64,
    max_chars: usize,
    overlap_chars: usize,
) -> Vec<SearchChunk> {
    build_search_chunks(&BuildChunksInput {
        vault_id: vault_id.to_owned(),
        path: prepared.path.clone(),
        basename: basename.to_owned(),
        mtime,
        content: prepared.content.clone(),
        content_hash: Some(prepared.content_hash.clone()),
        max_chars,
        overlap_chars,
        image_descriptions: prepared.image_descriptions.clone(),
        linked_documents: prepared.linked_documents.clone(),
        extra_hash_facets: prepared.hash_facets.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const MD5_A: &str = "0123456789abcdef0123456789abcdef";
    const MD5_B: &str = "fedcba9876543210fedcba9876543210";

    #[derive(Default)]
    struct Fake {
        desc: HashMap<String, ImageDescription>,
        notes: HashMap<String, String>,
    }

    impl PrepareHost for Fake {
        fn image_description(&self, md5: &str) -> Option<ImageDescription> {
            self.desc.get(md5).cloned()
        }
        fn resolve_linked(&self, linkpath: &str, _from: &str) -> Option<LinkedSource> {
            let path = format!("{linkpath}.md");
            self.notes.get(&path).map(|c| LinkedSource {
                // The host computes `TFile.basename` (`cv_text::vpath::stem_last_ext` in the
                // daemon); this fake's paths are `<linkpath>.md`.
                basename: linkpath.rsplit('/').next().unwrap_or(linkpath).to_owned(),
                path,
                content: c.clone(),
            })
        }
    }

    fn desc(md5: &str, kind: &str, narrative: &str, hash: &str) -> ImageDescription {
        ImageDescription {
            md5: md5.into(),
            kind: kind.into(),
            narrative: narrative.into(),
            extraction: String::new(),
            description_hash: hash.into(),
            described_at: "t".into(),
            raw: "{}".into(),
        }
    }

    fn input(embeds: &[&str]) -> PrepareInput {
        PrepareInput {
            path: "n.md".into(),
            content: "# N\nbody".into(),
            embeds: embeds
                .iter()
                .map(|l| EmbedRef {
                    link: (*l).into(),
                    resolved: Some(format!("att/{l}")),
                })
                .collect(),
            ..PrepareInput::default()
        }
    }

    #[test]
    fn md5_naming_convention() {
        assert_eq!(
            localized_image_md5(&format!("a/x-{}_md5.PNG", MD5_A.to_uppercase())),
            Some((MD5_A.into(), format!("x-{}_md5.PNG", MD5_A.to_uppercase())))
        );
        assert_eq!(localized_image_md5("a/plain.png"), None);
        assert_eq!(localized_image_md5(&format!("{MD5_A}_MD5.tar.gz")), None);
    }

    #[test]
    fn description_facet_moves_the_hash_and_failed_records_do_not() {
        let mut host = Fake::default();
        let a = format!("{MD5_A}_MD5.png");
        let b = format!("{MD5_B}_MD5.png");
        let plain = prepare_search_file(&host, &input(&[&a]));
        assert!(plain.hash_facets.is_empty());
        assert_eq!(plain.content_hash, hash_search_content("# N\nbody", &[]));

        host.desc
            .insert(MD5_A.into(), desc(MD5_A, "vision", "chart", "11111111"));
        let one = prepare_search_file(&host, &input(&[&a, "https://x/y.png"]));
        assert_ne!(one.content_hash, plain.content_hash);
        assert_eq!(one.image_descriptions.len(), 1);

        host.desc
            .insert(MD5_A.into(), desc(MD5_A, "vision", "chart", "22222222"));
        let changed = prepare_search_file(&host, &input(&[&a]));
        assert_ne!(changed.content_hash, one.content_hash);

        // A failed record emits nothing and leaves the hash alone; order is by md5.
        host.desc
            .insert(MD5_B.into(), desc(MD5_B, "failed", "", "33333333"));
        assert_eq!(
            prepare_search_file(&host, &input(&[&b, &a])).content_hash,
            changed.content_hash
        );
        host.desc
            .insert(MD5_B.into(), desc(MD5_B, "vision", "second", "44444444"));
        let two = prepare_search_file(&host, &input(&[&b, &a, &a]));
        let names: Vec<String> = two
            .image_descriptions
            .iter()
            .map(|d| d.filename.clone())
            .collect();
        assert_eq!(names, [a.clone(), b.clone()]);
    }

    #[test]
    fn linked_posts_cap_before_resolution_and_fold_facets() {
        let mut host = Fake::default();
        for i in 0..10 {
            host.notes.insert(
                format!("x/p{i}.md"),
                format!("---\nstate: ok\n---\nPost {i}"),
            );
        }
        host.notes
            .insert("x/p1.md".into(), "---\nstate: unavailable\n---\n".into());
        let stamps: Vec<Value> = (0..10)
            .map(|i| Value::from(format!("[[x/p{i}|alias]]")))
            .collect();
        let mut inp = input(&[]);
        inp.x_metadata = Some(Value::Array(stamps));
        inp.yt_metadata = Some(Value::from("[[x/p9#h]]"));
        let p = prepare_search_file(&host, &inp);
        // 8 stamps considered (p0..p7), the tombstoned p1 dropped.
        assert_eq!(p.linked_documents.len(), 7);
        assert_eq!(p.hash_facets.len(), 7);
        assert!(p.hash_facets[0].starts_with("linked:x/p0.md:"));
        assert_eq!(
            linked_metadata_linkpaths(None, Some(&Value::from("  [[a#b|c]] "))),
            ["a"]
        );
    }
}
