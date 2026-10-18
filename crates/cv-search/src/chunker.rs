//! Byte-compatible port of Crucible's search chunker
//! (`https://github.com/dulrich/obsidian-crucible/blob/master/src/search/chunker.ts:158-408,496-558`).
//!
//! Byte compatibility is the point (plan "Decisions locked": the search feed is byte-compatible,
//! so cut-over inherits the companion's index and embeddings under Crucible's `vaultId` with no
//! re-embed): `stableChunkId`, `hashSearchContent` (+ `hashString`), the chunk text and the
//! entity text must equal what the TS produces for the same input. Every string operation the TS
//! performs on code units is reproduced on UTF-16 code units ([`crate::js`], audit item E4), and
//! every regex is hand-ported with its exact JS backtracking result (no `u` flag, `\s` = the JS
//! whitespace set, `.` excludes the four JS line terminators). A `slice` that splits a surrogate
//! pair keeps the lone surrogate, exactly as the TS does — it is hashed as itself and serialized
//! as a `\uXXXX` escape.
//!
//! Dropped (Obsidian-induced or unused here): nothing in this module is Obsidian-induced; the
//! TS module is already a pure, settings-free function of its input. `isImageChunkHeading` is a
//! results-UI concern and is not ported.

use std::collections::HashSet;
use std::sync::Arc;

use crate::js::{
    JsStr, is_term, is_ws, js, run, split_char, split_lines, split_runs, starts_with, to_lower,
    trim, write_json_string,
};

/// `MAX_ENTITIES_PER_CHUNK` (`chunker.ts:27`).
pub const MAX_ENTITIES_PER_CHUNK: usize = 32;
/// `MAX_ENTITY_TEXT_CHARS` (`chunker.ts:28`), in UTF-16 code units.
pub const MAX_ENTITY_TEXT_CHARS: usize = 200;
/// `IMAGE_CHUNK_HEADING_PREFIX` (`chunker.ts:45`).
pub const IMAGE_CHUNK_HEADING_PREFIX: &str = "Image: ";
/// `LINKED_POST_HEADING_PREFIX` (`chunker.ts:74`).
pub const LINKED_POST_HEADING_PREFIX: &str = "Linked post: ";
/// `MAX_LINKED_DOCUMENTS_PER_NOTE` (`chunker.ts:85`).
pub const MAX_LINKED_DOCUMENTS_PER_NOTE: usize = 8;
/// Crucible's `searchChunkMaxChars` default (`src/types.ts:913`).
pub const DEFAULT_MAX_CHARS: usize = 1800;
/// Crucible's `searchChunkOverlapChars` default (`src/types.ts:914`).
pub const DEFAULT_OVERLAP_CHARS: usize = 200;
/// `DEFAULT_SEARCH_EXTENSIONS` (`chunker.ts:36`) = Crucible's `searchIndexExtensions` default.
pub const SEARCH_EXTENSIONS: [&str; 3] = ["md", "qmd", "txt"];

/// `FRONTMATTER_ENTITY_FIELDS` (`chunker.ts:20-22`).
const FRONTMATTER_ENTITY_FIELDS: [(&str, &str); 1] = [("author", "person")];

/// One parsed `parseSimpleFrontmatter` value (`chunker.ts:509-539`): a scalar or a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FmValue {
    /// A scalar string.
    Str(JsStr),
    /// A list of strings.
    List(Vec<JsStr>),
}

/// `parseSimpleFrontmatter`'s object, in JS own-property order (integer-like keys ascending
/// first, then insertion order; a re-assigned key keeps its first position).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    entries: Vec<(JsStr, FmValue)>,
}

impl Frontmatter {
    /// `frontmatter[key]`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&FmValue> {
        let k = js(key);
        self.entries.iter().find(|(e, _)| *e == k).map(|(_, v)| v)
    }

    fn set(&mut self, key: JsStr, value: FmValue) {
        // `out['__proto__'] = v` sets the prototype, never an own property.
        if key == js("__proto__") {
            return;
        }
        if let Some(slot) = self.entries.iter_mut().find(|(e, _)| *e == key) {
            slot.1 = value;
        } else {
            self.entries.push((key, value));
        }
    }

    /// Entries in JS own-property enumeration order.
    #[must_use]
    pub fn ordered(&self) -> Vec<&(JsStr, FmValue)> {
        let index = |k: &JsStr| -> Option<u64> {
            let s = String::from_utf16(k).ok()?;
            let n: u64 = s.parse().ok()?;
            (n.to_string() == s && n < 4_294_967_295).then_some(n)
        };
        let mut ints: Vec<(u64, &(JsStr, FmValue))> = self
            .entries
            .iter()
            .filter_map(|e| index(&e.0).map(|n| (n, e)))
            .collect();
        ints.sort_by_key(|(n, _)| *n);
        let mut out: Vec<&(JsStr, FmValue)> = ints.into_iter().map(|(_, e)| e).collect();
        out.extend(self.entries.iter().filter(|e| index(&e.0).is_none()));
        out
    }
}

/// `SearchEntity` (`src/search/types.ts`): `{text, type, source: 'frontmatter'}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEntity {
    /// Normalized entity text (may end in a lone surrogate after the 200-unit cap).
    pub text: JsStr,
    /// Entity type (`person`).
    pub kind: String,
}

/// `SearchDocumentMetadata` (`src/search/types.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentMetadata {
    /// Title: `frontmatter.title` || first `# ` heading || basename.
    pub title: JsStr,
    /// `frontmatter.created`.
    pub created: Option<JsStr>,
    /// `frontmatter.modified`.
    pub modified: Option<JsStr>,
    /// `frontmatter.source`.
    pub source: Option<JsStr>,
    /// Normalized tags.
    pub tags: Vec<JsStr>,
    /// The parsed frontmatter.
    pub frontmatter: Frontmatter,
}

/// One described image reaching the chunker (`ImageDescriptionChunkInput`, `chunker.ts:60-67`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageDescriptionChunkInput {
    /// Attachment basename.
    pub filename: JsStr,
    /// Narrative (may be empty).
    pub narrative: JsStr,
    /// Extraction (may be empty).
    pub extraction: JsStr,
}

/// One linked post reaching the chunker (`LinkedDocumentChunkInput`, `chunker.ts:93-105`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedDocumentChunkInput {
    /// Linked note's vault path.
    pub path: JsStr,
    /// Linked note's basename.
    pub title: JsStr,
    /// Linked note's body (frontmatter sliced off, trimmed, capped).
    pub text: JsStr,
}

/// `BuildChunksInput` (`chunker.ts:107-138`).
#[derive(Debug, Clone, Default)]
pub struct BuildChunksInput {
    /// Companion vault id.
    pub vault_id: String,
    /// Note path.
    pub path: String,
    /// Note basename (title fallback).
    pub basename: String,
    /// Note mtime (ms).
    pub mtime: i64,
    /// Note content.
    pub content: JsStr,
    /// Precomputed `contentHash` (else recomputed with `extra_hash_facets`).
    pub content_hash: Option<String>,
    /// `maxChars` (`0` = default, floored at 400).
    pub max_chars: usize,
    /// `overlapChars`.
    pub overlap_chars: usize,
    /// Described images, ordered by the caller.
    pub image_descriptions: Vec<ImageDescriptionChunkInput>,
    /// Linked posts, ordered by the caller.
    pub linked_documents: Vec<LinkedDocumentChunkInput>,
    /// Facets for the `contentHash` fallback.
    pub extra_hash_facets: Vec<JsStr>,
}

/// One chunk (`SearchChunk`, `src/search/types.ts`). `metadata`/`entities` are shared per note.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchChunk {
    /// `stableChunkId`.
    pub id: String,
    /// Vault id.
    pub vault_id: String,
    /// Note path.
    pub path: String,
    /// Note `contentHash`.
    pub content_hash: String,
    /// Note title.
    pub title: JsStr,
    /// Section heading.
    pub heading: JsStr,
    /// Trimmed chunk text.
    pub text: JsStr,
    /// Note mtime (ms).
    pub mtime: i64,
    /// Monotonic ordinal within the note.
    pub ordinal: usize,
    /// Note metadata.
    pub metadata: Arc<DocumentMetadata>,
    /// Note entities (omitted from the payload when empty).
    pub entities: Arc<Vec<SearchEntity>>,
    /// Embedding vector, when semantic indexing is on.
    pub embedding: Option<Vec<f32>>,
    /// Model that produced `embedding`.
    pub embedding_model: Option<String>,
    /// Vector space of `embedding` (`embeddingSpaceId(model)` = the bare model id when no
    /// precision is known, `src/search/types.ts` `embeddingSpaceId`).
    pub embedding_space: Option<String>,
}

impl SearchChunk {
    /// The chunk's `metadata` object as `JSON.stringify` would emit it (the companion's
    /// `metadata_json` column; stored as the `semsearch` chunk metadata).
    #[must_use]
    pub fn metadata_json(&self) -> String {
        let mut o = String::with_capacity(256);
        write_metadata(&mut o, &self.metadata);
        o
    }

    /// The chunk as `JSON.stringify` would emit it (key order as the TS object literal; lone
    /// surrogates escaped).
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut o = String::with_capacity(self.text.len() + 256);
        let s = |o: &mut String, v: &str| write_json_string(o, &js(v));
        o.push_str("{\"id\":");
        s(&mut o, &self.id);
        o.push_str(",\"vaultId\":");
        s(&mut o, &self.vault_id);
        o.push_str(",\"path\":");
        s(&mut o, &self.path);
        o.push_str(",\"contentHash\":");
        s(&mut o, &self.content_hash);
        o.push_str(",\"title\":");
        write_json_string(&mut o, &self.title);
        o.push_str(",\"heading\":");
        write_json_string(&mut o, &self.heading);
        o.push_str(",\"text\":");
        write_json_string(&mut o, &self.text);
        o.push_str(&format!(
            ",\"mtime\":{},\"ordinal\":{},\"metadata\":",
            self.mtime, self.ordinal
        ));
        write_metadata(&mut o, &self.metadata);
        if !self.entities.is_empty() {
            o.push_str(",\"entities\":[");
            for (i, e) in self.entities.iter().enumerate() {
                if i > 0 {
                    o.push(',');
                }
                o.push_str("{\"text\":");
                write_json_string(&mut o, &e.text);
                o.push_str(",\"type\":");
                s(&mut o, &e.kind);
                o.push_str(",\"source\":\"frontmatter\"}");
            }
            o.push(']');
        }
        if let Some(v) = &self.embedding {
            o.push_str(",\"embedding\":");
            o.push_str(&serde_json::to_string(v).unwrap_or_else(|_| "[]".into()));
        }
        if let Some(m) = &self.embedding_model {
            o.push_str(",\"embeddingModel\":");
            s(&mut o, m);
        }
        if let Some(m) = &self.embedding_space {
            o.push_str(",\"embeddingSpace\":");
            s(&mut o, m);
        }
        o.push('}');
        o
    }
}

fn write_metadata(o: &mut String, m: &DocumentMetadata) {
    o.push_str("{\"title\":");
    write_json_string(o, &m.title);
    for (k, v) in [
        ("created", &m.created),
        ("modified", &m.modified),
        ("source", &m.source),
    ] {
        if let Some(v) = v {
            o.push_str(&format!(",\"{k}\":"));
            write_json_string(o, v);
        }
    }
    o.push_str(",\"tags\":[");
    for (i, t) in m.tags.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        write_json_string(o, t);
    }
    o.push_str("],\"frontmatter\":{");
    for (i, (k, v)) in m.frontmatter.ordered().into_iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        write_json_string(o, k);
        o.push(':');
        match v {
            FmValue::Str(s) => write_json_string(o, s),
            FmValue::List(items) => {
                o.push('[');
                for (j, it) in items.iter().enumerate() {
                    if j > 0 {
                        o.push(',');
                    }
                    write_json_string(o, it);
                }
                o.push(']');
            }
        }
    }
    o.push_str("}}");
}

/// `isSearchIndexablePath` (`chunker.ts:147-156`) with the default extension list.
#[must_use]
pub fn is_search_indexable_path(path: &str) -> bool {
    let name = js(path.rsplit('/').next().unwrap_or(""));
    let Some(dot) = name.iter().rposition(|&c| c == u16::from(b'.')) else {
        return false;
    };
    if !(dot > 0 && dot < name.len() - 1) {
        return false;
    }
    let ext = String::from_utf16_lossy(&name[dot + 1..]).to_lowercase();
    SEARCH_EXTENSIONS.contains(&ext.as_str())
}

/// `hashString` (`chunker.ts:500-507`): 32-bit FNV-1a over UTF-16 code units
/// (`hash ^= charCodeAt(i); hash = Math.imul(hash, 0x01000193)`), as 8 lowercase hex digits.
#[must_use]
pub fn hash_string(value: &[u16]) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for &c in value {
        hash ^= u32::from(c);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

/// `stableChunkId` (`chunker.ts:496-498`): `path#ordinal:hashString(vaultId\npath\nheading\nordinal)`.
#[must_use]
pub fn stable_chunk_id(vault_id: &str, path: &str, ordinal: usize, heading: &[u16]) -> String {
    let mut key = js(&format!("{vault_id}\n{path}\n"));
    key.extend_from_slice(heading);
    key.extend(js(&format!("\n{ordinal}")));
    format!("{path}#{ordinal}:{}", hash_string(&key))
}

/// `FRONTMATTER_RE = /^---\s*\n([\s\S]*?)\n---\s*(?:\n|$)/` (`chunker.ts:3`), with JS
/// backtracking: returns `(group_start, group_end, match_end)`.
#[must_use]
pub fn match_frontmatter(c: &[u16]) -> Option<(usize, usize, usize)> {
    if !starts_with(c, "---") {
        return None;
    }
    let nl = u16::from(b'\n');
    let w0 = run(c, 3, is_ws);
    // Greedy `\s*` backtracks from the longest run to the last `\n` it can hand to the literal.
    for p in (3..3 + w0).rev().filter(|&p| c[p] == nl) {
        let start = p + 1;
        for g in start..c.len() {
            if c[g] != nl || !starts_with(&c[g + 1..], "---") {
                continue;
            }
            let q = g + 4;
            let w = run(c, q, is_ws);
            if q + w == c.len() {
                return Some((start, g, c.len()));
            }
            if let Some(last) = (q..q + w).rev().find(|&i| c[i] == nl) {
                return Some((start, g, last + 1));
            }
        }
    }
    None
}

/// `stripFrontmatterBlock` (`chunker.ts:403-406`).
#[must_use]
pub fn strip_frontmatter_block(content: &[u16]) -> &[u16] {
    match match_frontmatter(content) {
        Some((_, _, end)) => &content[end..],
        None => content,
    }
}

/// `unquote` (`chunker.ts:553-558`).
fn unquote(v: &[u16]) -> JsStr {
    let (dq, sq) = (u16::from(b'"'), u16::from(b'\''));
    if let (Some(&f), Some(&l)) = (v.first(), v.last())
        && ((f == dq && l == dq) || (f == sq && l == sq))
    {
        // `value.slice(1, -1)`: a one-unit `"` yields "".
        return v.get(1..v.len().saturating_sub(1)).unwrap_or(&[]).to_vec();
    }
    v.to_vec()
}

fn is_key_char(c: u16) -> bool {
    c < 128 && {
        let b = c as u8;
        b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
    }
}

/// `/^([A-Za-z0-9_-]+):\s*(.*)$/` → `(key, rest)`.
fn match_key_line(line: &[u16]) -> Option<(&[u16], &[u16])> {
    let k = run(line, 0, is_key_char);
    if k == 0 || line.get(k) != Some(&u16::from(b':')) {
        return None;
    }
    let w = run(line, k + 1, is_ws);
    let rest = &line[k + 1 + w..];
    if rest.iter().any(|&c| is_term(c)) {
        return None;
    }
    Some((&line[..k], rest))
}

/// `/^\s*-\s+(.+)$/` → group 1.
fn match_list_item(line: &[u16]) -> Option<&[u16]> {
    let l = run(line, 0, is_ws);
    if line.get(l) != Some(&u16::from(b'-')) {
        return None;
    }
    let w = run(line, l + 1, is_ws);
    if w == 0 {
        return None;
    }
    for s in (1..=w).rev() {
        let g = &line[l + 1 + s..];
        if !g.is_empty() && !g.iter().any(|&c| is_term(c)) {
            return Some(g);
        }
    }
    None
}

/// `parseSimpleFrontmatter` (`chunker.ts:509-539`).
#[must_use]
pub fn parse_simple_frontmatter(text: &[u16]) -> Frontmatter {
    let mut out = Frontmatter::default();
    if text.is_empty() {
        return out;
    }
    let lines = split_lines(text);
    let mut i = 0;
    while i < lines.len() {
        if let Some((key, rest)) = match_key_line(lines[i]) {
            let raw = trim(rest);
            if raw.is_empty() {
                let mut values = Vec::new();
                while i + 1 < lines.len() {
                    let Some(item) = match_list_item(lines[i + 1]) else {
                        break;
                    };
                    values.push(unquote(trim(item)));
                    i += 1;
                }
                let v = if values.is_empty() {
                    FmValue::Str(Vec::new())
                } else {
                    FmValue::List(values)
                };
                out.set(key.to_vec(), v);
            } else if raw.first() == Some(&u16::from(b'[')) && raw.last() == Some(&u16::from(b']'))
            {
                let inner = raw.get(1..raw.len().saturating_sub(1)).unwrap_or(&[]);
                let items = split_char(inner, u16::from(b','))
                    .into_iter()
                    .map(|v| unquote(trim(v)))
                    .filter(|v| !v.is_empty())
                    .collect();
                out.set(key.to_vec(), FmValue::List(items));
            } else {
                out.set(key.to_vec(), FmValue::Str(unquote(raw)));
            }
        }
        i += 1;
    }
    out
}

/// `normalizeEntityText` (`chunker.ts:338-343`): `String(v).replace(/\s+/g, ' ').trim().slice(0, 200)`.
fn normalize_entity_text(v: &[u16]) -> JsStr {
    let mut collapsed = Vec::with_capacity(v.len());
    let mut i = 0;
    while i < v.len() {
        let n = run(v, i, is_ws);
        if n > 0 {
            collapsed.push(u16::from(b' '));
            i += n;
        } else {
            collapsed.push(v[i]);
            i += 1;
        }
    }
    let t = trim(&collapsed);
    t[..t.len().min(MAX_ENTITY_TEXT_CHARS)].to_vec()
}

/// `extractFrontmatterEntities` (`chunker.ts:287-307`).
#[must_use]
pub fn extract_frontmatter_entities(fm: &Frontmatter) -> Vec<SearchEntity> {
    let mut entities = Vec::new();
    let mut seen: HashSet<JsStr> = HashSet::new();
    for (key, kind) in FRONTMATTER_ENTITY_FIELDS {
        let values: Vec<&[u16]> = match fm.get(key) {
            Some(FmValue::List(items)) => items.iter().map(Vec::as_slice).collect(),
            Some(FmValue::Str(s)) => vec![s.as_slice()],
            None => Vec::new(),
        };
        for value in values {
            let text = normalize_entity_text(value);
            if text.is_empty() {
                continue;
            }
            let mut k = js(&format!("{kind}\n"));
            k.extend(to_lower(&text));
            if !seen.insert(k) {
                continue;
            }
            entities.push(SearchEntity {
                text,
                kind: kind.to_owned(),
            });
            if entities.len() >= MAX_ENTITIES_PER_CHUNK {
                return entities;
            }
        }
    }
    entities
}

/// `entityIndexText` (`chunker.ts:323-336`): deduplicated by lowercased text, joined by `\n`.
#[must_use]
pub fn entity_index_text(entities: &[SearchEntity]) -> JsStr {
    let mut out: JsStr = Vec::new();
    let mut seen: HashSet<JsStr> = HashSet::new();
    let mut count = 0;
    for e in entities {
        let text = normalize_entity_text(&e.text);
        if text.is_empty() || !seen.insert(to_lower(&text)) {
            continue;
        }
        if count > 0 {
            out.push(u16::from(b'\n'));
        }
        out.extend_from_slice(&text);
        count += 1;
        if count >= MAX_ENTITIES_PER_CHUNK {
            break;
        }
    }
    out
}

/// `hashSearchContent` (`chunker.ts:379-391`). Facets are trimmed, blank ones dropped,
/// deduplicated and sorted in **code-unit** order (JS default sort).
#[must_use]
pub fn hash_search_content(content: &[u16], extra_facets: &[JsStr]) -> String {
    let fm_text = match_frontmatter(content).map_or(&[][..], |(s, e, _)| &content[s..e]);
    let entities = extract_frontmatter_entities(&parse_simple_frontmatter(fm_text));
    let mut base = content.to_vec();
    base.extend(js("\nentities:"));
    base.extend(entity_index_text(&entities));
    let mut facets: Vec<JsStr> = extra_facets
        .iter()
        .map(|f| trim(f).to_vec())
        .filter(|f| !f.is_empty())
        .collect();
    facets.sort();
    facets.dedup();
    if facets.is_empty() {
        return hash_string(&base);
    }
    base.extend(js("\nfacets:"));
    for (i, f) in facets.iter().enumerate() {
        if i > 0 {
            base.push(u16::from(b'\n'));
        }
        base.extend_from_slice(f);
    }
    hash_string(&base)
}

/// `/^#\s+(.+)$/m` over the body → the trimmed group, if any (`chunker.ts:413`).
fn first_h1(body: &[u16]) -> Option<JsStr> {
    let hash = u16::from(b'#');
    for i in 0..body.len() {
        if body[i] != hash || (i > 0 && !is_term(body[i - 1])) {
            continue;
        }
        let w = run(body, i + 1, is_ws);
        for s in (1..=w).rev() {
            let st = i + 1 + s;
            let n = run(body, st, |c| !is_term(c));
            if n > 0 {
                return Some(trim(&body[st..st + n]).to_vec());
            }
        }
    }
    None
}

/// `stringValue` (`chunker.ts:549-551`).
fn string_value(v: Option<&FmValue>) -> Option<JsStr> {
    match v {
        Some(FmValue::Str(s)) if !trim(s).is_empty() => Some(trim(s).to_vec()),
        _ => None,
    }
}

/// `normalizeTags` (`chunker.ts:541-547`).
fn normalize_tags(v: Option<&FmValue>) -> Vec<JsStr> {
    let strip = |t: &[u16]| -> JsStr {
        let t = if t.first() == Some(&u16::from(b'#')) {
            &t[1..]
        } else {
            t
        };
        trim(t).to_vec()
    };
    match v {
        Some(FmValue::List(items)) => items
            .iter()
            .map(|t| strip(t))
            .filter(|t| !t.is_empty())
            .collect(),
        Some(FmValue::Str(s)) => split_runs(s, 1, |c| c == u16::from(b',') || is_ws(c))
            .into_iter()
            .map(strip)
            .filter(|t| !t.is_empty())
            .collect(),
        None => Vec::new(),
    }
}

/// `parseSearchDocument` (`chunker.ts:408-427`) → `(body, metadata)`.
#[must_use]
pub fn parse_search_document<'a>(
    content: &'a [u16],
    fallback_title: &str,
) -> (&'a [u16], DocumentMetadata) {
    let m = match_frontmatter(content);
    let fm_text = m.map_or(&[][..], |(s, e, _)| &content[s..e]);
    let body = m.map_or(content, |(_, _, end)| &content[end..]);
    let fm = parse_simple_frontmatter(fm_text);
    let title = string_value(fm.get("title"))
        .or_else(|| first_h1(body).filter(|h| !h.is_empty()))
        .unwrap_or_else(|| js(fallback_title));
    let meta = DocumentMetadata {
        title,
        created: string_value(fm.get("created")),
        modified: string_value(fm.get("modified")),
        source: string_value(fm.get("source")),
        tags: normalize_tags(fm.get("tags")),
        frontmatter: fm,
    };
    (body, meta)
}

/// `HEADING_RE = /^(#{1,6})\s+(.+?)\s*#*\s*$/` on one line → group 2 (`chunker.ts:4`).
fn match_heading(line: &[u16]) -> Option<&[u16]> {
    let hash = u16::from(b'#');
    let h = run(line, 0, |c| c == hash);
    if h == 0 || h > 6 {
        return None;
    }
    let w = run(line, h, is_ws);
    // The valid starts of the `\s*#*\s*$` tail form a contiguous suffix [m, len].
    let len = line.len();
    let mut m = len;
    while m > 0 && is_ws(line[m - 1]) {
        m -= 1;
    }
    while m > 0 && line[m - 1] == hash {
        m -= 1;
    }
    while m > 0 && is_ws(line[m - 1]) {
        m -= 1;
    }
    for s in (1..=w).rev() {
        let gs = h + s;
        let ge = (gs + 1).max(m);
        if ge > len || line[gs..ge].iter().any(|&c| is_term(c)) {
            continue;
        }
        return Some(&line[gs..ge]);
    }
    None
}

struct Section {
    heading: JsStr,
    text: JsStr,
}

/// `splitSections` (`chunker.ts:429-443`).
fn split_sections(body: &[u16]) -> Vec<Section> {
    let mut sections: Vec<(JsStr, Vec<&[u16]>)> = vec![(Vec::new(), Vec::new())];
    for line in split_lines(body) {
        if let Some(h) = match_heading(line) {
            sections.push((trim(h).to_vec(), vec![line]));
        } else if let Some(cur) = sections.last_mut() {
            cur.1.push(line);
        }
    }
    sections
        .into_iter()
        .map(|(heading, lines)| Section {
            heading,
            text: trim(&lines.join(&u16::from(b'\n'))).to_vec(),
        })
        .filter(|s| !s.text.is_empty())
        .collect()
}

/// `chunkText` (`chunker.ts:445-468`).
fn chunk_text(text: &[u16], max: usize, overlap: usize) -> Vec<JsStr> {
    if text.len() <= max {
        return vec![text.to_vec()];
    }
    let sep: JsStr = js("\n\n");
    let join = |a: &[u16], b: &[u16]| -> JsStr {
        if a.is_empty() {
            b.to_vec()
        } else {
            [a, &sep, b].concat()
        }
    };
    let mut chunks = Vec::new();
    let mut current: JsStr = Vec::new();
    for paragraph in split_runs(text, 2, |c| c == u16::from(b'\n')) {
        let next = join(&current, paragraph);
        if next.len() <= max {
            current = next;
            continue;
        }
        if !current.is_empty() {
            chunks.push(current.clone());
        }
        if paragraph.len() <= max {
            current = tail_overlap(&current, overlap);
            current = join(&current, paragraph);
        } else {
            chunks.extend(slice_long_text(paragraph, max, overlap));
            current = Vec::new();
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// `sliceLongText` (`chunker.ts:470-480`) — may split a surrogate pair, as the TS does.
fn slice_long_text(text: &[u16], max: usize, overlap: usize) -> Vec<JsStr> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = text.len().min(start + max);
        chunks.push(text[start..end].to_vec());
        if end == text.len() {
            break;
        }
        start = end.saturating_sub(overlap).max(start + 1);
    }
    chunks
}

/// `tailOverlap` (`chunker.ts:482-485`).
fn tail_overlap(text: &[u16], overlap: usize) -> JsStr {
    if text.is_empty() || overlap == 0 {
        return Vec::new();
    }
    text[text.len().saturating_sub(overlap)..].to_vec()
}

/// `buildSearchChunks` (`chunker.ts:158-275`).
#[must_use]
pub fn build_search_chunks(input: &BuildChunksInput) -> Vec<SearchChunk> {
    // `Math.max(400, input.maxChars || 1800)`.
    let max = if input.max_chars == 0 {
        DEFAULT_MAX_CHARS
    } else {
        input.max_chars
    }
    .max(400);
    let overlap = input.overlap_chars.min(max / 3);
    let (body, metadata) = parse_search_document(&input.content, &input.basename);
    let content_hash = input
        .content_hash
        .clone()
        .unwrap_or_else(|| hash_search_content(&input.content, &input.extra_hash_facets));
    let entities = Arc::new(extract_frontmatter_entities(&metadata.frontmatter));
    let metadata = Arc::new(metadata);
    let mut chunks = Vec::new();
    let mut ordinal = 0usize;
    let mut emit = |heading: &[u16], source: &[u16], chunks: &mut Vec<SearchChunk>| {
        for text in chunk_text(source, max, overlap) {
            let trimmed = trim(&text);
            if trimmed.is_empty() {
                continue;
            }
            chunks.push(SearchChunk {
                id: stable_chunk_id(&input.vault_id, &input.path, ordinal, heading),
                vault_id: input.vault_id.clone(),
                path: input.path.clone(),
                content_hash: content_hash.clone(),
                title: metadata.title.clone(),
                heading: heading.to_vec(),
                text: trimmed.to_vec(),
                mtime: input.mtime,
                ordinal,
                metadata: metadata.clone(),
                entities: entities.clone(),
                embedding: None,
                embedding_model: None,
                embedding_space: None,
            });
            ordinal += 1;
        }
    };
    for section in split_sections(body) {
        emit(&section.heading, &section.text, &mut chunks);
    }
    for image in &input.image_descriptions {
        let filename = trim(&image.filename);
        if filename.is_empty() {
            continue;
        }
        let mut narrative_heading = js(IMAGE_CHUNK_HEADING_PREFIX);
        narrative_heading.extend_from_slice(filename);
        let mut text_heading = narrative_heading.clone();
        text_heading.extend(js(" (text)"));
        for (heading, part) in [
            (narrative_heading, &image.narrative),
            (text_heading, &image.extraction),
        ] {
            let source = trim(part);
            if source.is_empty() {
                continue;
            }
            emit(&heading, source, &mut chunks);
        }
    }
    for doc in input
        .linked_documents
        .iter()
        .take(MAX_LINKED_DOCUMENTS_PER_NOTE)
    {
        let linked_text = trim(&doc.text);
        if linked_text.is_empty() {
            continue;
        }
        let title = trim(&doc.title);
        let mut heading = js(LINKED_POST_HEADING_PREFIX);
        heading.extend_from_slice(if title.is_empty() { &doc.path } else { title });
        emit(&heading, linked_text, &mut chunks);
    }
    chunks
}
