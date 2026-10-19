//! Search chunker: a native-`str` port of Crucible's chunker
//! (`https://github.com/dulrich/obsidian-crucible/blob/master/src/search/chunker.ts:158-408,496-558`).
//!
//! The algorithm (frontmatter, title/tag metadata, frontmatter entities, heading sections,
//! paragraph packing with overlap, image and linked-post chunks, the folded `contentHash`) is
//! Crucible's. The strings are Rust `str`: lengths, `max_chars`/`overlap_chars` and slices count
//! `char`s (so a slice never splits a code point), whitespace is [`char::is_whitespace`], facets
//! sort in `str` order and [`hash_string`] runs over UTF-8 bytes. Hashes and slice boundaries
//! therefore differ from Crucible's UTF-16 ones, so vectors are not shared with Crucible's index.
//!
//! Dropped (Obsidian-induced or unused here): nothing in this module is Obsidian-induced; the
//! TS module is already a pure, settings-free function of its input. `isImageChunkHeading` is a
//! results-UI concern and is not ported.

use std::collections::HashSet;
use std::sync::Arc;

/// `MAX_ENTITIES_PER_CHUNK` (`chunker.ts:27`).
pub const MAX_ENTITIES_PER_CHUNK: usize = 32;
/// `MAX_ENTITY_TEXT_CHARS` (`chunker.ts:28`), in chars.
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

/// A line terminator (what `.` does not match in the TS regexes).
fn is_term(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// The byte offset of the `n`th char of `s` (or `s.len()`).
fn char_offset(s: &str, n: usize) -> usize {
    s.char_indices().nth(n).map_or(s.len(), |(i, _)| i)
}

/// The first `n` chars of `s`.
#[must_use]
pub fn take_chars(s: &str, n: usize) -> &str {
    &s[..char_offset(s, n)]
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// One parsed `parseSimpleFrontmatter` value (`chunker.ts:509-539`): a scalar or a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FmValue {
    /// A scalar string.
    Str(String),
    /// A list of strings.
    List(Vec<String>),
}

/// `parseSimpleFrontmatter`'s object, in JS own-property order (integer-like keys ascending
/// first, then insertion order; a re-assigned key keeps its first position).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    entries: Vec<(String, FmValue)>,
}

impl Frontmatter {
    /// `frontmatter[key]`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&FmValue> {
        self.entries.iter().find(|(e, _)| e == key).map(|(_, v)| v)
    }

    fn set(&mut self, key: &str, value: FmValue) {
        // `out['__proto__'] = v` sets the prototype, never an own property.
        if key == "__proto__" {
            return;
        }
        if let Some(slot) = self.entries.iter_mut().find(|(e, _)| e == key) {
            slot.1 = value;
        } else {
            self.entries.push((key.to_owned(), value));
        }
    }

    /// Entries in JS own-property enumeration order.
    #[must_use]
    pub fn ordered(&self) -> Vec<&(String, FmValue)> {
        let index = |k: &str| -> Option<u64> {
            let n: u64 = k.parse().ok()?;
            (n.to_string() == k && n < 4_294_967_295).then_some(n)
        };
        let mut ints: Vec<(u64, &(String, FmValue))> = self
            .entries
            .iter()
            .filter_map(|e| index(&e.0).map(|n| (n, e)))
            .collect();
        ints.sort_by_key(|(n, _)| *n);
        let mut out: Vec<&(String, FmValue)> = ints.into_iter().map(|(_, e)| e).collect();
        out.extend(self.entries.iter().filter(|e| index(&e.0).is_none()));
        out
    }
}

/// `SearchEntity` (`src/search/types.ts`): `{text, type, source: 'frontmatter'}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEntity {
    /// Normalized entity text (capped at [`MAX_ENTITY_TEXT_CHARS`] chars).
    pub text: String,
    /// Entity type (`person`).
    pub kind: String,
}

/// `SearchDocumentMetadata` (`src/search/types.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentMetadata {
    /// Title: `frontmatter.title` || first `# ` heading || basename.
    pub title: String,
    /// `frontmatter.created`.
    pub created: Option<String>,
    /// `frontmatter.modified`.
    pub modified: Option<String>,
    /// `frontmatter.source`.
    pub source: Option<String>,
    /// Normalized tags.
    pub tags: Vec<String>,
    /// The parsed frontmatter.
    pub frontmatter: Frontmatter,
}

/// One described image reaching the chunker (`ImageDescriptionChunkInput`, `chunker.ts:60-67`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageDescriptionChunkInput {
    /// Attachment basename.
    pub filename: String,
    /// Narrative (may be empty).
    pub narrative: String,
    /// Extraction (may be empty).
    pub extraction: String,
}

/// One linked post reaching the chunker (`LinkedDocumentChunkInput`, `chunker.ts:93-105`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedDocumentChunkInput {
    /// Linked note's vault path.
    pub path: String,
    /// Linked note's basename.
    pub title: String,
    /// Linked note's body (frontmatter sliced off, trimmed, capped).
    pub text: String,
}

/// `BuildChunksInput` (`chunker.ts:107-138`).
#[derive(Debug, Clone, Default)]
pub struct BuildChunksInput {
    /// Vault id folded into chunk ids.
    pub vault_id: String,
    /// Note path.
    pub path: String,
    /// Note basename (title fallback).
    pub basename: String,
    /// Note mtime (ms).
    pub mtime: i64,
    /// Note content.
    pub content: String,
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
    pub extra_hash_facets: Vec<String>,
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
    pub title: String,
    /// Section heading.
    pub heading: String,
    /// Trimmed chunk text.
    pub text: String,
    /// Note mtime (ms).
    pub mtime: i64,
    /// Monotonic ordinal within the note.
    pub ordinal: usize,
    /// Note metadata.
    pub metadata: Arc<DocumentMetadata>,
    /// Note entities.
    pub entities: Arc<Vec<SearchEntity>>,
    /// Embedding vector, when semantic indexing is on.
    pub embedding: Option<Vec<f32>>,
    /// Model that produced `embedding`.
    pub embedding_model: Option<String>,
    /// Vector space of `embedding`.
    pub embedding_space: Option<String>,
}

impl SearchChunk {
    /// The chunk's `metadata` object as JSON (stored as the `semsearch` chunk metadata).
    #[must_use]
    pub fn metadata_json(&self) -> String {
        let mut o = String::with_capacity(256);
        write_metadata(&mut o, &self.metadata);
        o
    }
}

fn write_json_string(o: &mut String, s: &str) {
    o.push_str(&serde_json::Value::from(s).to_string());
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
    let name = path.rsplit('/').next().unwrap_or("");
    let Some(dot) = name.rfind('.') else {
        return false;
    };
    if !(dot > 0 && dot < name.len() - 1) {
        return false;
    }
    let ext = name[dot + 1..].to_lowercase();
    SEARCH_EXTENSIONS.contains(&ext.as_str())
}

/// `hashString` (`chunker.ts:500-507`): 32-bit FNV-1a, here over UTF-8 bytes, as 8 lowercase
/// hex digits.
#[must_use]
pub fn hash_string(value: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for &b in value.as_bytes() {
        hash ^= u32::from(b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

/// `stableChunkId` (`chunker.ts:496-498`): `path#ordinal:hashString(vaultId\npath\nheading\nordinal)`.
#[must_use]
pub fn stable_chunk_id(vault_id: &str, path: &str, ordinal: usize, heading: &str) -> String {
    let key = format!("{vault_id}\n{path}\n{heading}\n{ordinal}");
    format!("{path}#{ordinal}:{}", hash_string(&key))
}

/// Byte length of the whitespace run of `s` starting at byte `from`.
fn ws_run(s: &str, from: usize) -> usize {
    s[from..]
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map_or(s.len() - from, |(i, _)| i)
}

/// `FRONTMATTER_RE = /^---\s*\n([\s\S]*?)\n---\s*(?:\n|$)/` (`chunker.ts:3`), with regex
/// backtracking: returns byte offsets `(group_start, group_end, match_end)`.
#[must_use]
pub fn match_frontmatter(c: &str) -> Option<(usize, usize, usize)> {
    if !c.starts_with("---") {
        return None;
    }
    let b = c.as_bytes();
    let w0 = ws_run(c, 3);
    // Greedy `\s*` backtracks from the longest run to the last `\n` it can hand to the literal.
    for p in (3..3 + w0).rev().filter(|&p| b[p] == b'\n') {
        let start = p + 1;
        for g in start..b.len() {
            if b[g] != b'\n' || !b[g + 1..].starts_with(b"---") {
                continue;
            }
            let q = g + 4;
            let w = ws_run(c, q);
            if q + w == b.len() {
                return Some((start, g, b.len()));
            }
            if let Some(last) = (q..q + w).rev().find(|&i| b[i] == b'\n') {
                return Some((start, g, last + 1));
            }
        }
    }
    None
}

/// `stripFrontmatterBlock` (`chunker.ts:403-406`).
#[must_use]
pub fn strip_frontmatter_block(content: &str) -> &str {
    match match_frontmatter(content) {
        Some((_, _, end)) => &content[end..],
        None => content,
    }
}

/// `unquote` (`chunker.ts:553-558`).
fn unquote(v: &str) -> String {
    for q in ['"', '\''] {
        if let Some(inner) = v.strip_prefix(q).and_then(|r| r.strip_suffix(q)) {
            return inner.to_owned();
        }
    }
    // A lone quote character (`"` sliced `(1, -1)`) yields "".
    if v == "\"" || v == "'" {
        return String::new();
    }
    v.to_owned()
}

/// `/^([A-Za-z0-9_-]+):\s*(.*)$/` → `(key, rest)`.
fn match_key_line(line: &str) -> Option<(&str, &str)> {
    let k = line
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(line.len());
    if k == 0 || !line[k..].starts_with(':') {
        return None;
    }
    let rest = line[k + 1..].trim_start();
    if rest.contains(is_term) {
        return None;
    }
    Some((&line[..k], rest))
}

/// `/^\s*-\s+(.+)$/` → group 1, left-trimmed.
fn match_list_item(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('-')?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let g = rest.trim_start();
    (!g.is_empty() && !g.contains(is_term)).then_some(g)
}

/// `s.split(/\r?\n/)`.
fn split_lines(s: &str) -> Vec<&str> {
    let mut out: Vec<&str> = s.split('\n').collect();
    // Only a `\r` before a `\n` belongs to the separator; the last piece keeps its own.
    let last = out.len() - 1;
    for l in &mut out[..last] {
        *l = l.strip_suffix('\r').unwrap_or(l);
    }
    out
}

/// `parseSimpleFrontmatter` (`chunker.ts:509-539`).
#[must_use]
pub fn parse_simple_frontmatter(text: &str) -> Frontmatter {
    let mut out = Frontmatter::default();
    if text.is_empty() {
        return out;
    }
    let lines = split_lines(text);
    let mut i = 0;
    while i < lines.len() {
        if let Some((key, rest)) = match_key_line(lines[i]) {
            let raw = rest.trim();
            if raw.is_empty() {
                let mut values = Vec::new();
                while i + 1 < lines.len() {
                    let Some(item) = match_list_item(lines[i + 1]) else {
                        break;
                    };
                    values.push(unquote(item.trim()));
                    i += 1;
                }
                let v = if values.is_empty() {
                    FmValue::Str(String::new())
                } else {
                    FmValue::List(values)
                };
                out.set(key, v);
            } else if let Some(inner) = raw.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                let items = inner
                    .split(',')
                    .map(|v| unquote(v.trim()))
                    .filter(|v| !v.is_empty())
                    .collect();
                out.set(key, FmValue::List(items));
            } else {
                out.set(key, FmValue::Str(unquote(raw)));
            }
        }
        i += 1;
    }
    out
}

/// `normalizeEntityText` (`chunker.ts:338-343`): whitespace runs collapsed to one space,
/// trimmed, capped at [`MAX_ENTITY_TEXT_CHARS`] chars.
fn normalize_entity_text(v: &str) -> String {
    let collapsed = v.split_whitespace().collect::<Vec<_>>().join(" ");
    take_chars(&collapsed, MAX_ENTITY_TEXT_CHARS).to_owned()
}

/// `extractFrontmatterEntities` (`chunker.ts:287-307`).
#[must_use]
pub fn extract_frontmatter_entities(fm: &Frontmatter) -> Vec<SearchEntity> {
    let mut entities = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (key, kind) in FRONTMATTER_ENTITY_FIELDS {
        let values: Vec<&str> = match fm.get(key) {
            Some(FmValue::List(items)) => items.iter().map(String::as_str).collect(),
            Some(FmValue::Str(s)) => vec![s.as_str()],
            None => Vec::new(),
        };
        for value in values {
            let text = normalize_entity_text(value);
            if text.is_empty() || !seen.insert(format!("{kind}\n{}", text.to_lowercase())) {
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
pub fn entity_index_text(entities: &[SearchEntity]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for e in entities {
        let text = normalize_entity_text(&e.text);
        if text.is_empty() || !seen.insert(text.to_lowercase()) {
            continue;
        }
        out.push(text);
        if out.len() >= MAX_ENTITIES_PER_CHUNK {
            break;
        }
    }
    out.join("\n")
}

/// `hashSearchContent` (`chunker.ts:379-391`). Facets are trimmed, blank ones dropped,
/// deduplicated and sorted.
#[must_use]
pub fn hash_search_content(content: &str, extra_facets: &[String]) -> String {
    let fm_text = match_frontmatter(content).map_or("", |(s, e, _)| &content[s..e]);
    let entities = extract_frontmatter_entities(&parse_simple_frontmatter(fm_text));
    let mut base = format!("{content}\nentities:{}", entity_index_text(&entities));
    let mut facets: Vec<&str> = extra_facets
        .iter()
        .map(|f| f.trim())
        .filter(|f| !f.is_empty())
        .collect();
    facets.sort_unstable();
    facets.dedup();
    if !facets.is_empty() {
        base.push_str("\nfacets:");
        base.push_str(&facets.join("\n"));
    }
    hash_string(&base)
}

/// `/^#\s+(.+)$/m` over the body → the trimmed group, if any (`chunker.ts:413`). `\s+` may
/// cross line ends, so `#` alone on a line titles the note with the next line.
fn first_h1(body: &str) -> Option<String> {
    let b = body.as_bytes();
    for i in (0..b.len()).filter(|&i| b[i] == b'#') {
        if i > 0 && !body[..i].ends_with(is_term) {
            continue;
        }
        let rest = &body[i + 1..];
        // Every char boundary inside the leading whitespace run, longest first (backtracking).
        let ws: Vec<usize> = rest
            .char_indices()
            .take_while(|(_, c)| c.is_whitespace())
            .map(|(j, c)| j + c.len_utf8())
            .collect();
        for &st in ws.iter().rev() {
            let line = rest[st..].split(is_term).next().unwrap_or("");
            if !line.is_empty() {
                return Some(line.trim().to_owned());
            }
        }
    }
    None
}

/// `stringValue` (`chunker.ts:549-551`).
fn string_value(v: Option<&FmValue>) -> Option<String> {
    match v {
        Some(FmValue::Str(s)) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        _ => None,
    }
}

/// `normalizeTags` (`chunker.ts:541-547`).
fn normalize_tags(v: Option<&FmValue>) -> Vec<String> {
    let strip = |t: &str| -> String { t.strip_prefix('#').unwrap_or(t).trim().to_owned() };
    match v {
        Some(FmValue::List(items)) => items
            .iter()
            .map(|t| strip(t))
            .filter(|t| !t.is_empty())
            .collect(),
        Some(FmValue::Str(s)) => s
            .split(|c: char| c == ',' || c.is_whitespace())
            .map(strip)
            .filter(|t| !t.is_empty())
            .collect(),
        None => Vec::new(),
    }
}

/// `parseSearchDocument` (`chunker.ts:408-427`) → `(body, metadata)`.
#[must_use]
pub fn parse_search_document<'a>(
    content: &'a str,
    fallback_title: &str,
) -> (&'a str, DocumentMetadata) {
    let m = match_frontmatter(content);
    let fm_text = m.map_or("", |(s, e, _)| &content[s..e]);
    let body = m.map_or(content, |(_, _, end)| &content[end..]);
    let fm = parse_simple_frontmatter(fm_text);
    let title = string_value(fm.get("title"))
        .or_else(|| first_h1(body))
        .unwrap_or_else(|| fallback_title.to_owned());
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

/// `HEADING_RE = /^(#{1,6})\s+(.+?)\s*#*\s*$/` on one line → the trimmed heading text.
fn match_heading(line: &str) -> Option<&str> {
    let h = line.len() - line.trim_start_matches('#').len();
    if h == 0 || h > 6 {
        return None;
    }
    let rest = &line[h..];
    if !rest.starts_with(char::is_whitespace) || rest.contains(is_term) {
        return None;
    }
    let inner = rest.trim();
    if inner.is_empty() {
        return None;
    }
    // Lazy `(.+?)` then `\s*#*\s*$`: a closing `#` run is dropped, but the group keeps ≥1 char.
    let stripped = inner.trim_end_matches('#').trim_end();
    if stripped.is_empty() {
        let first = inner.chars().next().map_or(0, char::len_utf8);
        return Some(&inner[..first]);
    }
    Some(stripped)
}

struct Section {
    heading: String,
    text: String,
}

/// `splitSections` (`chunker.ts:429-443`).
fn split_sections(body: &str) -> Vec<Section> {
    let mut sections: Vec<(String, Vec<&str>)> = vec![(String::new(), Vec::new())];
    for line in split_lines(body) {
        if let Some(h) = match_heading(line) {
            sections.push((h.to_owned(), vec![line]));
        } else if let Some(cur) = sections.last_mut() {
            cur.1.push(line);
        }
    }
    sections
        .into_iter()
        .map(|(heading, lines)| Section {
            heading,
            text: lines.join("\n").trim().to_owned(),
        })
        .filter(|s| !s.text.is_empty())
        .collect()
}

/// Splits on runs of two or more `\n` (`/\n{2,}/`).
fn split_paragraphs(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let (mut start, mut i) = (0, 0);
    while i < b.len() {
        let n = b[i..].iter().take_while(|&&c| c == b'\n').count();
        if n >= 2 {
            out.push(&text[start..i]);
            i += n;
            start = i;
        } else {
            i += n.max(1);
        }
    }
    out.push(&text[start..]);
    out
}

/// `chunkText` (`chunker.ts:445-468`); lengths in chars.
fn chunk_text(text: &str, max: usize, overlap: usize) -> Vec<String> {
    if char_len(text) <= max {
        return vec![text.to_owned()];
    }
    let join = |a: &str, b: &str| -> String {
        if a.is_empty() {
            b.to_owned()
        } else {
            format!("{a}\n\n{b}")
        }
    };
    let mut chunks = Vec::new();
    let mut current = String::new();
    for paragraph in split_paragraphs(text) {
        let next = join(&current, paragraph);
        if char_len(&next) <= max {
            current = next;
            continue;
        }
        if !current.is_empty() {
            chunks.push(current.clone());
        }
        if char_len(paragraph) <= max {
            current = join(tail_overlap(&current, overlap), paragraph);
        } else {
            chunks.extend(slice_long_text(paragraph, max, overlap));
            current = String::new();
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// `sliceLongText` (`chunker.ts:470-480`), on char boundaries.
fn slice_long_text(text: &str, max: usize, overlap: usize) -> Vec<String> {
    let bounds: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain([text.len()])
        .collect();
    let n = bounds.len() - 1;
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < n {
        let end = n.min(start + max);
        chunks.push(text[bounds[start]..bounds[end]].to_owned());
        if end == n {
            break;
        }
        start = end.saturating_sub(overlap).max(start + 1);
    }
    chunks
}

/// `tailOverlap` (`chunker.ts:482-485`): the last `overlap` chars.
fn tail_overlap(text: &str, overlap: usize) -> &str {
    if overlap == 0 {
        return "";
    }
    let n = char_len(text);
    &text[char_offset(text, n.saturating_sub(overlap))..]
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
    let mut emit = |heading: &str, source: &str, chunks: &mut Vec<SearchChunk>| {
        for text in chunk_text(source, max, overlap) {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            chunks.push(SearchChunk {
                id: stable_chunk_id(&input.vault_id, &input.path, ordinal, heading),
                vault_id: input.vault_id.clone(),
                path: input.path.clone(),
                content_hash: content_hash.clone(),
                title: metadata.title.clone(),
                heading: heading.to_owned(),
                text: trimmed.to_owned(),
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
        let filename = image.filename.trim();
        if filename.is_empty() {
            continue;
        }
        let narrative_heading = format!("{IMAGE_CHUNK_HEADING_PREFIX}{filename}");
        let text_heading = format!("{narrative_heading} (text)");
        for (heading, part) in [
            (narrative_heading, &image.narrative),
            (text_heading, &image.extraction),
        ] {
            let source = part.trim();
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
        let linked_text = doc.text.trim();
        if linked_text.is_empty() {
            continue;
        }
        let title = doc.title.trim();
        let heading = format!(
            "{LINKED_POST_HEADING_PREFIX}{}",
            if title.is_empty() { &doc.path } else { title }
        );
        emit(&heading, linked_text, &mut chunks);
    }
    chunks
}
