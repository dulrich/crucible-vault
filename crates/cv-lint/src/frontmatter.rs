//! Minimal-diff frontmatter editing over the raw note text.
//!
//! Crucible edits frontmatter through Obsidian's `processFrontMatter`, which parses the YAML to
//! an object and re-serializes the *whole* block (`https://github.com/dulrich/obsidian-crucible/blob/master/src/frontmatter.ts:287-343`
//! `upsertFrontmatterProperty`, `upsertFrontmatterPropertyIfEmpty`, `sortFrontmatterProperties`).
//! That re-serialization (lost comments, re-quoted scalars, reflowed lists) is Obsidian-induced
//! and dropped here: every edit splices only the bytes of the entry it touches, so untouched
//! keys — their comments, quoting, ordering and blank lines — stay byte-identical.
//!
//! The block is located with `ofm::frontmatter::split_frontmatter` and values are read and
//! validated with `ofm::frontmatter::parse_frontmatter`; ofm is never used to re-serialize
//! untouched keys. Top-level entries are found by a line scan (a column-0 `key:` line plus its
//! indented / `- ` continuation lines); a block whose scanned keys disagree with ofm's parsed
//! keys (e.g. a top-level flow mapping) is refused with [`FmError::UnsupportedLayout`] rather
//! than edited blind.

use ofm::frontmatter::{JsValue, json_stringify, parse_frontmatter, split_frontmatter};
use thiserror::Error;

/// Why a frontmatter edit was refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FmError {
    #[error("property name is required")]
    EmptyKey,
    #[error("frontmatter is not valid YAML: {0}")]
    InvalidYaml(String),
    #[error("frontmatter top level is not a mapping")]
    NotAMapping,
    #[error("frontmatter layout not supported for minimal-diff edits: {0}")]
    UnsupportedLayout(String),
}

/// One top-level entry: `[start, end)` covers the key line and its continuation lines;
/// `[end, chunk_end)` is trailing column-0 trivia (blank lines, comments) up to the next key.
#[derive(Debug, Clone)]
struct Entry {
    key: String,
    start: usize,
    end: usize,
    chunk_end: usize,
}

/// The located block: `inner` is `[inner_start, inner_end)` (the YAML lines, each ending in a
/// newline), `close_end` is the byte after the closing `---` line (its newline included).
#[derive(Debug, Clone)]
struct Block {
    inner_start: usize,
    inner_end: usize,
    close_end: usize,
    newline: &'static str,
}

fn detect_newline(text: &str) -> &'static str {
    match text.find('\n') {
        Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => "\r\n",
        _ => "\n",
    }
}

fn strip_cr(s: &str) -> &str {
    s.strip_suffix('\r').unwrap_or(s)
}

/// Locate the frontmatter block. An empty `---\n---` block (which ofm's split rejects, having
/// no key line) is recognised so a first key is added inside it rather than in a second block.
fn locate(text: &str) -> Option<Block> {
    let newline = detect_newline(text);
    let first_nl = text.find('\n')?;
    if strip_cr(&text[..first_nl]) != "---" {
        return None;
    }
    let inner_start = first_nl + 1;
    let (meta, body) = split_frontmatter(text);
    if meta.is_some() {
        let close_end = text.len() - body.len();
        // The closing fence line starts after the last newline before `close_end`'s line.
        let before_close = if close_end > 0 && text.as_bytes()[close_end - 1] == b'\n' {
            &text[..close_end - 1]
        } else {
            &text[..close_end]
        };
        let close_start = before_close.rfind('\n').map_or(0, |i| i + 1);
        return Some(Block {
            inner_start,
            inner_end: close_start.max(inner_start),
            close_end,
            newline,
        });
    }
    let rest = &text[inner_start..];
    let line_end = rest.find('\n').map_or(rest.len(), |i| i + 1);
    let line = rest[..line_end].trim_end_matches('\n');
    if strip_cr(line) == "---" {
        return Some(Block {
            inner_start,
            inner_end: inner_start,
            close_end: inner_start + line_end,
            newline,
        });
    }
    None
}

fn is_continuation(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t') || line == "-" || line.starts_with("- ")
}

fn key_of_line(line: &str) -> Option<String> {
    let line = strip_cr(line);
    if line.is_empty() || line.starts_with('#') || is_continuation(line) {
        return None;
    }
    for quote in ['"', '\''] {
        if let Some(rest) = line.strip_prefix(quote) {
            let close = rest.find(quote)?;
            let after = &rest[close + 1..];
            return after
                .strip_prefix(':')
                .filter(|a| a.is_empty() || a.starts_with([' ', '\t']))
                .map(|_| rest[..close].to_owned());
        }
    }
    let colon = line
        .match_indices(':')
        .find(|(i, _)| {
            let after = &line[i + 1..];
            after.is_empty() || after.starts_with([' ', '\t'])
        })
        .map(|(i, _)| i)?;
    Some(line[..colon].trim_end().to_owned())
}

/// A note's text with its frontmatter, editable in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontmatterDoc {
    text: String,
}

impl FrontmatterDoc {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn into_text(self) -> String {
        self.text
    }

    pub fn has_frontmatter(&self) -> bool {
        locate(&self.text).is_some()
    }

    /// The note body (everything after the closing fence).
    pub fn body(&self) -> &str {
        match locate(&self.text) {
            Some(b) => &self.text[b.close_end..],
            None => &self.text,
        }
    }

    /// The raw YAML lines (without fences), as written.
    pub fn raw_block(&self) -> Option<&str> {
        locate(&self.text).map(|b| &self.text[b.inner_start..b.inner_end])
    }

    /// Parsed top-level entries in written order. No frontmatter → empty.
    pub fn entries(&self) -> Result<Vec<(String, JsValue)>, FmError> {
        let Some(raw) = self.raw_block() else {
            return Ok(Vec::new());
        };
        let normalized = raw.replace("\r\n", "\n");
        if normalized.trim().is_empty() {
            return Ok(Vec::new());
        }
        match parse_frontmatter(&normalized).map_err(FmError::InvalidYaml)? {
            JsValue::Obj(o) => Ok(o
                .entries()
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.clone()))
                .collect()),
            JsValue::Null => Ok(Vec::new()),
            _ => Err(FmError::NotAMapping),
        }
    }

    pub fn get(&self, key: &str) -> Result<Option<JsValue>, FmError> {
        Ok(self
            .entries()?
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v))
    }

    /// Top-level keys in written order.
    pub fn keys(&self) -> Result<Vec<String>, FmError> {
        Ok(self.scan()?.1.into_iter().map(|e| e.key).collect())
    }

    /// Scan entries and cross-check them against ofm's parse.
    fn scan(&self) -> Result<(Option<Block>, Vec<Entry>), FmError> {
        let Some(block) = locate(&self.text) else {
            return Ok((None, Vec::new()));
        };
        let mut entries: Vec<Entry> = Vec::new();
        let mut pos = block.inner_start;
        let mut prelude_continuation = false;
        while pos < block.inner_end {
            let rest = &self.text[pos..block.inner_end];
            let len = rest.find('\n').map_or(rest.len(), |i| i + 1);
            let line = rest[..len].trim_end_matches('\n');
            let next = pos + len;
            if let Some(key) = key_of_line(line) {
                if let Some(last) = entries.last_mut() {
                    last.chunk_end = pos;
                }
                entries.push(Entry {
                    key,
                    start: pos,
                    end: next,
                    chunk_end: next,
                });
            } else if is_continuation(strip_cr(line)) && !strip_cr(line).trim().is_empty() {
                match entries.last_mut() {
                    // Continuation after interleaved blank lines still belongs to the entry.
                    Some(last) => last.end = next,
                    None => prelude_continuation = true,
                }
            } else if let Some(last) = entries.last_mut() {
                last.chunk_end = next;
            }
            pos = next;
        }
        if let Some(last) = entries.last_mut() {
            last.chunk_end = block.inner_end;
        }
        if prelude_continuation {
            return Err(FmError::UnsupportedLayout(
                "indented content before the first key".into(),
            ));
        }
        let parsed: Vec<String> = self.entries()?.into_iter().map(|(k, _)| k).collect();
        let mut scanned: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        let mut parsed_sorted: Vec<&str> = parsed.iter().map(String::as_str).collect();
        scanned.sort_unstable();
        scanned.dedup();
        parsed_sorted.sort_unstable();
        if scanned != parsed_sorted || scanned.len() != entries.len() {
            return Err(FmError::UnsupportedLayout(
                "scanned keys disagree with parsed keys".into(),
            ));
        }
        Ok((Some(block), entries))
    }

    fn splice(&mut self, start: usize, end: usize, with: &str) -> bool {
        if &self.text[start..end] == with {
            return false;
        }
        self.text.replace_range(start..end, with);
        true
    }

    /// Insert a rendered entry at the end of the block (creating the block if absent).
    fn append_entry(&mut self, block: Option<Block>, rendered: &str) -> bool {
        match block {
            Some(b) => {
                let nl = b.newline;
                // A last YAML line without a trailing newline cannot occur (the closing fence
                // follows it), but guard the empty-block case where inner_end == inner_start.
                let entry = format!("{rendered}{nl}");
                self.text.insert_str(b.inner_end, &entry);
                true
            }
            None => {
                let nl = detect_newline(&self.text);
                let head = format!("---{nl}{rendered}{nl}---{nl}");
                self.text.insert_str(0, &head);
                true
            }
        }
    }

    /// Set `key` to `value`. An existing entry whose parsed value already equals `value` is left
    /// byte-identical; otherwise only that entry's lines are replaced. A missing key is appended
    /// at the end of the block; a note without frontmatter gets a new `---` block.
    pub fn set(&mut self, key: &str, value: &JsValue) -> Result<bool, FmError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(FmError::EmptyKey);
        }
        let (block, entries) = self.scan()?;
        let nl = block
            .as_ref()
            .map_or_else(|| detect_newline(&self.text), |b| b.newline);
        match entries.iter().find(|e| e.key == key) {
            Some(e) => {
                if self.get(key)?.as_ref() == Some(value) {
                    return Ok(false);
                }
                let rendered = format!("{}{nl}", render_entry(key, value, nl));
                Ok(self.splice(e.start, e.end, &rendered))
            }
            None => Ok(self.append_entry(block, &render_entry(key, value, nl))),
        }
    }

    /// Crucible `insertFrontmatterPropertyAfter` (`src/frontmatter.ts`): an existing `key` is
    /// [`Self::set`] in place; an absent `key` is inserted directly after `anchor`'s lines (before
    /// the anchor's trailing comments/blank lines), or at the end of the block when `anchor` is
    /// absent (a note without frontmatter gets a new block).
    pub fn insert_after(
        &mut self,
        anchor: &str,
        key: &str,
        value: &JsValue,
    ) -> Result<bool, FmError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(FmError::EmptyKey);
        }
        let (block, entries) = self.scan()?;
        if entries.iter().any(|e| e.key == key) {
            return self.set(key, value);
        }
        let nl = block
            .as_ref()
            .map_or_else(|| detect_newline(&self.text), |b| b.newline);
        let rendered = render_entry(key, value, nl);
        match entries.iter().find(|e| e.key == anchor.trim()) {
            Some(a) => {
                let at = a.end;
                Ok(self.splice(at, at, &format!("{rendered}{nl}")))
            }
            None => Ok(self.append_entry(block, &rendered)),
        }
    }

    /// Crucible `upsertFrontmatterPropertyIfEmpty` (`src/frontmatter.ts:293-300`): set only when
    /// the key is absent, `null`, or the empty string.
    pub fn set_if_empty(&mut self, key: &str, value: &JsValue) -> Result<bool, FmError> {
        let key = key.trim();
        if key.is_empty() {
            return Ok(false);
        }
        match self.get(key)? {
            None | Some(JsValue::Null) => self.set(key, value),
            Some(JsValue::Str(s)) if s.is_empty() => self.set(key, value),
            Some(_) => Ok(false),
        }
    }

    /// Remove `key`'s lines (its trailing comments/blank lines stay). Absent key → no change.
    ///
    /// ofm's split only reads a block whose first non-blank YAML line is a key, so removing the
    /// first key must never leave a comment leading the block: its trailing trivia moves below
    /// the next key's lines; with no key left and non-blank trivia, the edit is refused. Any
    /// result ofm no longer reads as frontmatter (other than an empty block) is reverted.
    pub fn remove(&mut self, key: &str) -> Result<bool, FmError> {
        let (block, entries) = self.scan()?;
        let Some(idx) = entries.iter().position(|e| e.key == key.trim()) else {
            return Ok(false);
        };
        let e = &entries[idx];
        let trivia = self.text[e.end..e.chunk_end].to_owned();
        let trivia_is_blank = trivia.lines().all(|l| strip_cr(l).trim().is_empty());
        let before = self.text.clone();
        if idx == 0 && !trivia_is_blank {
            match entries.get(1) {
                Some(next) => {
                    // [e.start, next.end) = key, trivia, next key lines → next key lines, trivia.
                    let moved = format!("{}{trivia}", &self.text[next.start..next.end]);
                    self.text.replace_range(e.start..next.end, &moved);
                }
                None => {
                    return Err(FmError::UnsupportedLayout(
                        "removing the only key would leave a comment-only block".into(),
                    ));
                }
            }
        } else if entries.len() == 1 {
            // Last key with blank trivia: drop the blanks too, leaving a clean empty block.
            self.text.replace_range(e.start..e.chunk_end, "");
        } else {
            self.text.replace_range(e.start..e.end, "");
        }
        if block.is_some() && !self.still_frontmatter() {
            self.text = before;
            return Err(FmError::UnsupportedLayout(
                "edit would leave a block ofm no longer reads as frontmatter".into(),
            ));
        }
        Ok(self.text != before)
    }

    /// True when ofm still reads the block, or it is an empty/blank-only block.
    fn still_frontmatter(&self) -> bool {
        if split_frontmatter(&self.text).0.is_some() {
            return true;
        }
        locate(&self.text).is_some_and(|b| {
            self.text[b.inner_start..b.inner_end]
                .lines()
                .all(|l| strip_cr(l).trim().is_empty())
        })
    }

    /// Rename `old` to `new` in place, keeping the value bytes; fails over to set+remove when
    /// the key token is quoted. Returns false if `old` is absent.
    pub fn rename_key(&mut self, old: &str, new: &str) -> Result<bool, FmError> {
        let (_, entries) = self.scan()?;
        let Some(e) = entries.iter().find(|e| e.key == old) else {
            return Ok(false);
        };
        let line = &self.text[e.start..e.end];
        if line.starts_with(old) {
            let start = e.start;
            let new_key = render_key(new);
            return Ok(self.splice(start, start + old.len(), &new_key));
        }
        let value = self.get(old)?.unwrap_or(JsValue::Null);
        self.remove(old)?;
        self.set(new, &value)
    }

    /// Append `item` to the list at `key` unless already present. Absent key → `[item]`. A block
    /// list gains one `- item` line in its own indentation (existing lines untouched); a flow
    /// list or scalar is rewritten as a block list (scalar becomes its first element).
    pub fn append_to_list(&mut self, key: &str, item: &JsValue) -> Result<bool, FmError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(FmError::EmptyKey);
        }
        let current = self.get(key)?;
        let items = match current {
            None | Some(JsValue::Null) => {
                return self.set(key, &JsValue::Arr(vec![item.clone()]));
            }
            Some(JsValue::Str(ref s)) if s.is_empty() => {
                return self.set(key, &JsValue::Arr(vec![item.clone()]));
            }
            Some(JsValue::Arr(items)) => items,
            Some(other) => vec![other],
        };
        if items.contains(item) {
            return Ok(false);
        }
        let (block, entries) = self.scan()?;
        let nl = block.as_ref().map_or("\n", |b| b.newline);
        let Some(e) = entries.iter().find(|e| e.key == key).cloned() else {
            return Ok(false);
        };
        // Block list: the entry's continuation lines are all `- ` items at one indentation.
        let raw = self.text[e.start..e.end].to_owned();
        let mut lines = raw.split_inclusive('\n');
        let head = lines.next().unwrap_or("");
        let is_block_list = strip_cr(head.trim_end_matches('\n'))
            .trim_end()
            .ends_with(':');
        let item_lines: Vec<&str> = lines.collect();
        let indent = item_lines
            .iter()
            .map(|l| strip_cr(l.trim_end_matches('\n')))
            .find(|l| !l.trim().is_empty())
            .and_then(|l| {
                let t = l.trim_start();
                t.starts_with('-')
                    .then(|| l[..l.len() - t.len()].to_owned())
            });
        if is_block_list && let Some(indent) = indent {
            let line = format!("{indent}- {}{nl}", render_scalar(item));
            self.text.insert_str(e.end, &line);
            return Ok(true);
        }
        let mut all = items;
        all.push(item.clone());
        self.set(key, &JsValue::Arr(all))
    }

    /// Crucible `sortFrontmatterProperties` (`src/frontmatter.ts:325-343`): priority keys first in
    /// priority order, the rest in written order. Entries move as whole chunks (with their
    /// trailing comments); no-op when already ordered.
    pub fn sort_keys(&mut self, priority: &[String]) -> Result<bool, FmError> {
        let (Some(block), entries) = self.scan()? else {
            return Ok(false);
        };
        if entries.is_empty() {
            return Ok(false);
        }
        let mut order: Vec<usize> = Vec::with_capacity(entries.len());
        for p in priority {
            if let Some(i) = entries.iter().position(|e| &e.key == p)
                && !order.contains(&i)
            {
                order.push(i);
            }
        }
        for i in 0..entries.len() {
            if !order.contains(&i) {
                order.push(i);
            }
        }
        if order.iter().enumerate().all(|(a, b)| a == *b) {
            return Ok(false);
        }
        let first = entries[0].start;
        let last = entries[entries.len() - 1].chunk_end;
        let mut out = String::with_capacity(last - first);
        for i in order {
            let chunk = &self.text[entries[i].start..entries[i].chunk_end];
            out.push_str(chunk);
            if !chunk.ends_with('\n') {
                out.push_str(block.newline);
            }
        }
        Ok(self.splice(first, last, &out))
    }

    /// Crucible `blank-line-after-yaml` (`src/lint.ts` LINT_STEPS): exactly one blank line
    /// between the closing fence and the body (body's leading whitespace trimmed).
    pub fn ensure_blank_line_after(&mut self) -> bool {
        let Some(b) = locate(&self.text) else {
            return false;
        };
        let nl = b.newline;
        let close_line = self.text[b.inner_end..b.close_end].trim_end().to_owned();
        let body = self.text[b.close_end..].trim_start().to_owned();
        let rebuilt = format!("{close_line}{nl}{nl}{body}");
        let start = b.inner_end;
        let len = self.text.len();
        self.splice(start, len, &rebuilt)
    }
}

fn render_key(key: &str) -> String {
    if key_of_line(&format!("{key}: x")).as_deref() == Some(key) && !key.contains(['"', '\'']) {
        key.to_owned()
    } else {
        json_stringify(&JsValue::Str(key.to_owned()))
    }
}

/// Render a scalar as YAML: plain when ofm parses it back to the same value, else JSON
/// (JSON is valid YAML flow syntax, so strings get double quotes).
fn render_scalar(value: &JsValue) -> String {
    match value {
        JsValue::Str(s) => {
            let plain_ok = !s.is_empty()
                && !s.contains(['\n', '\r'])
                && s.trim() == s
                && parse_frontmatter(&format!("k: {s}"))
                    .ok()
                    .and_then(|v| match v {
                        JsValue::Obj(o) => o.entries().into_iter().next().map(|(_, v)| v.clone()),
                        _ => None,
                    })
                    .as_ref()
                    == Some(value);
            if plain_ok {
                s.clone()
            } else {
                json_stringify(value)
            }
        }
        JsValue::Null => String::new(),
        other => json_stringify(other),
    }
}

/// Render `key: value` (block list for a non-empty array). No trailing newline.
fn render_entry(key: &str, value: &JsValue, nl: &str) -> String {
    let k = render_key(key);
    match value {
        JsValue::Arr(items) if !items.is_empty() => {
            let mut out = format!("{k}:");
            for item in items {
                out.push_str(nl);
                out.push_str("  - ");
                out.push_str(&render_scalar(item));
            }
            out
        }
        JsValue::Null => format!("{k}:"),
        other => format!("{k}: {}", render_scalar(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> JsValue {
        JsValue::Str(v.into())
    }

    const DOC: &str = "---\ntitle: 'Quoted Title'   # trailing\n# own-line comment\n\ntags:\n  - a\n  - b\nn: 0x1F\n---\n\nBody text.\n";

    #[test]
    fn set_existing_touches_only_that_entry() {
        let mut d = FrontmatterDoc::new(DOC);
        assert!(d.set("n", &JsValue::Num(5.0)).unwrap());
        assert_eq!(d.text(), DOC.replace("n: 0x1F\n", "n: 5\n"));
    }

    #[test]
    fn set_equal_value_is_byte_identical() {
        let mut d = FrontmatterDoc::new(DOC);
        assert!(!d.set("title", &s("Quoted Title")).unwrap());
        assert!(!d.set("n", &JsValue::Num(31.0)).unwrap());
        assert_eq!(d.text(), DOC);
    }

    #[test]
    fn set_new_key_appends_before_fence() {
        let mut d = FrontmatterDoc::new(DOC);
        assert!(d.set("word-count", &JsValue::Num(2.0)).unwrap());
        assert_eq!(
            d.text(),
            DOC.replace("n: 0x1F\n", "n: 0x1F\nword-count: 2\n")
        );
    }

    #[test]
    fn insert_after_places_new_key_after_anchor() {
        let link = JsValue::Arr(vec![s("[[m/a]]")]);
        let mut d = FrontmatterDoc::new("---\na: 1\nyt-video-id: x\nz: 2\n---\nB\n");
        assert!(d.insert_after("yt-video-id", "yt-metadata", &link).unwrap());
        assert_eq!(
            d.text(),
            "---\na: 1\nyt-video-id: x\nyt-metadata:\n  - \"[[m/a]]\"\nz: 2\n---\nB\n"
        );
        // Anchor absent → end of block.
        let mut d = FrontmatterDoc::new("---\na: 1\n---\n");
        assert!(d.insert_after("nope", "k", &s("v")).unwrap());
        assert_eq!(d.text(), "---\na: 1\nk: v\n---\n");
        // Existing key → set in place.
        let mut d = FrontmatterDoc::new("---\nk: v\nyt-video-id: x\n---\n");
        assert!(!d.insert_after("yt-video-id", "k", &s("v")).unwrap());
    }

    #[test]
    fn set_creates_block_when_absent() {
        let mut d = FrontmatterDoc::new("Just body.\n");
        assert!(d.set("title", &s("note")).unwrap());
        assert_eq!(d.text(), "---\ntitle: note\n---\nJust body.\n");
    }

    #[test]
    fn set_into_empty_block() {
        let mut d = FrontmatterDoc::new("---\n---\nBody\n");
        assert!(d.set("a", &s("b")).unwrap());
        assert_eq!(d.text(), "---\na: b\n---\nBody\n");
    }

    #[test]
    fn set_quotes_ambiguous_strings() {
        let mut d = FrontmatterDoc::new("---\na: 1\n---\n");
        d.set("b", &s("true")).unwrap();
        d.set("c", &s("x: y")).unwrap();
        d.set("d", &s("2026-07-29")).unwrap();
        assert_eq!(
            d.text(),
            "---\na: 1\nb: \"true\"\nc: \"x: y\"\nd: 2026-07-29\n---\n"
        );
        assert_eq!(d.get("b").unwrap(), Some(s("true")));
        assert_eq!(d.get("c").unwrap(), Some(s("x: y")));
    }

    #[test]
    fn remove_keeps_neighbours_and_comments() {
        let mut d = FrontmatterDoc::new(DOC);
        assert!(d.remove("tags").unwrap());
        assert_eq!(d.text(), DOC.replace("tags:\n  - a\n  - b\n", ""));
        assert!(!d.remove("absent").unwrap());
    }

    #[test]
    fn remove_first_key_keeps_block_key_first() {
        let src = "---\ntitle: T\n# note about tags\ntags:\n  - a\nkind: x\n---\nbody\n";
        let mut d = FrontmatterDoc::new(src);
        assert!(d.remove("title").unwrap());
        assert_eq!(
            d.text(),
            "---\ntags:\n  - a\n# note about tags\nkind: x\n---\nbody\n"
        );
        assert!(split_frontmatter(d.text()).0.is_some());
        assert_eq!(
            d.keys().unwrap(),
            vec!["tags".to_owned(), "kind".to_owned()]
        );
        assert_eq!(d.body(), "body\n");
    }

    #[test]
    fn remove_only_key_with_comment_is_refused() {
        let src = "---\ntitle: T\n# keep me\n---\nbody\n";
        let mut d = FrontmatterDoc::new(src);
        assert!(matches!(
            d.remove("title"),
            Err(FmError::UnsupportedLayout(_))
        ));
        assert_eq!(d.text(), src);
    }

    #[test]
    fn remove_only_key_leaves_empty_block() {
        let mut d = FrontmatterDoc::new("---\ntitle: T\n\n---\nbody\n");
        assert!(d.remove("title").unwrap());
        assert_eq!(d.text(), "---\n---\nbody\n");
        assert!(d.set("a", &s("x")).unwrap());
        assert_eq!(d.text(), "---\na: x\n---\nbody\n");
    }

    #[test]
    fn append_to_block_list_adds_one_line() {
        let mut d = FrontmatterDoc::new(DOC);
        assert!(d.append_to_list("tags", &s("c")).unwrap());
        assert_eq!(d.text(), DOC.replace("  - b\n", "  - b\n  - c\n"));
        assert!(!d.append_to_list("tags", &s("a")).unwrap());
    }

    #[test]
    fn append_to_flow_list_and_absent() {
        let mut d = FrontmatterDoc::new("---\nx: 1\ntags: [a]\n---\n");
        assert!(d.append_to_list("tags", &s("b")).unwrap());
        assert_eq!(d.text(), "---\nx: 1\ntags:\n  - a\n  - b\n---\n");
        assert!(d.append_to_list("new", &s("z")).unwrap());
        assert_eq!(d.get("new").unwrap(), Some(JsValue::Arr(vec![s("z")])));
    }

    #[test]
    fn sort_moves_whole_chunks() {
        let mut d = FrontmatterDoc::new("---\nb: 1 # c1\nc:\n  - x\na: 'q'\n---\nB\n");
        assert!(d.sort_keys(&["a".into(), "c".into()]).unwrap());
        assert_eq!(d.text(), "---\na: 'q'\nc:\n  - x\nb: 1 # c1\n---\nB\n");
        assert!(!d.sort_keys(&["a".into(), "c".into()]).unwrap());
    }

    #[test]
    fn crlf_preserved() {
        let mut d = FrontmatterDoc::new("---\r\na: 1\r\n---\r\nB\r\n");
        d.set("b", &JsValue::Num(2.0)).unwrap();
        assert_eq!(d.text(), "---\r\na: 1\r\nb: 2\r\n---\r\nB\r\n");
    }

    #[test]
    fn rename_in_place_keeps_value_bytes() {
        let mut d = FrontmatterDoc::new("---\nvideo_ids: ['x'] # keep\nz: 1\n---\n");
        assert!(d.rename_key("video_ids", "yt-video-ids").unwrap());
        assert_eq!(d.text(), "---\nyt-video-ids: ['x'] # keep\nz: 1\n---\n");
    }

    #[test]
    fn unsupported_layout_refused() {
        let mut d = FrontmatterDoc::new("---\n{a: 1}\n---\n");
        assert!(d.set("b", &s("c")).is_err());
    }

    #[test]
    fn blank_line_after_yaml() {
        let mut d = FrontmatterDoc::new("---\na: 1\n---\nBody\n");
        assert!(d.ensure_blank_line_after());
        assert_eq!(d.text(), "---\na: 1\n---\n\nBody\n");
        assert!(!d.ensure_blank_line_after());
    }
}
