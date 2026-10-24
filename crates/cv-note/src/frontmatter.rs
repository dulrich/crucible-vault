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
use std::ops::Range;

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
    open_start: usize,
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

/// Byte length of a leading UTF-8 BOM (0 or 3).
fn bom_len(text: &str) -> usize {
    if text.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    }
}

/// The canonical frontmatter span (B10): the byte range from the opening `---` fence (after a
/// leading BOM, if any) through the closing fence line, its newline included — so
/// `&text[..r.end]` is the block (BOM included) and `&text[r.end..]` the body. The fence rule
/// is ofm's `split_frontmatter` (exact `---` lines, CR tolerated, a YAML-looking first line),
/// plus the empty `---\n---` block. `None` when there is no (closed) block.
pub fn span(text: &str) -> Option<Range<usize>> {
    locate(text).map(|b| b.open_start..b.close_end)
}

/// [`locate_at`] past a leading BOM, with offsets into `text`.
fn locate(text: &str) -> Option<Block> {
    let off = bom_len(text);
    locate_at(&text[off..]).map(|b| Block {
        open_start: off,
        inner_start: b.inner_start + off,
        inner_end: b.inner_end + off,
        close_end: b.close_end + off,
        newline: b.newline,
    })
}

/// Locate the frontmatter block. An empty `---\n---` block (which ofm's split rejects, having
/// no key line) is recognised so a first key is added inside it rather than in a second block.
fn locate_at(text: &str) -> Option<Block> {
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
            open_start: 0,
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
            open_start: 0,
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

    /// The string value of `key` (a `Str` only, untrimmed); absent, any other type or a parse error is `None`.
    pub fn get_str(&self, key: &str) -> Option<String> {
        match self.get(key) {
            Ok(Some(JsValue::Str(s))) => Some(s),
            _ => None,
        }
    }

    /// [`Self::get_str`] trimmed, an empty result being `None`.
    pub fn get_str_trimmed(&self, key: &str) -> Option<String> {
        self.get_str(key)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    /// The boolean value of `key` (a `Bool` only); absent, any other type or a parse error is `None`.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.get(key) {
            Ok(Some(JsValue::Bool(b))) => Some(b),
            _ => None,
        }
    }

    /// `key` as strings: an array's `Str` items (others dropped) or a scalar `Str` as one item; anything else (or a parse error) is empty.
    pub fn get_strings(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Ok(Some(JsValue::Arr(a))) => a
                .into_iter()
                .filter_map(|x| match x {
                    JsValue::Str(s) => Some(s),
                    _ => None,
                })
                .collect(),
            Ok(Some(JsValue::Str(s))) => vec![s],
            _ => Vec::new(),
        }
    }

    /// `key` as stringified entries: an array's every item, a scalar as one item, null/absent/parse error as none.
    pub fn get_texts(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Ok(None | Some(JsValue::Null)) | Err(_) => Vec::new(),
            Ok(Some(JsValue::Arr(a))) => a.iter().map(js_text).collect(),
            Ok(Some(other)) => vec![js_text(&other)],
        }
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
                self.text.insert_str(bom_len(&self.text), &head);
                true
            }
        }
    }

    /// [`Self::set`] as one minimal splice `(range of the current text, replacement)` instead of
    /// an in-place edit, so an editor buffer applies it as a single undo step (B10: the
    /// editor's frontmatter toggle and the daemon's `NoteSetBool` share this writer). A
    /// one-line plain-scalar entry keeps its spacing and trailing `# comment` (only the scalar
    /// token is replaced); any other edit is the common-prefix/suffix diff of [`Self::set`].
    /// `Ok(None)` when nothing changes. A text that opens with a `---` line that is not a
    /// closed block (see [`span`]) is refused rather than given a second block.
    pub fn set_edit(
        &self,
        key: &str,
        value: &JsValue,
    ) -> Result<Option<(Range<usize>, String)>, FmError> {
        let rest = &self.text[bom_len(&self.text)..];
        let first = rest.split('\n').next().unwrap_or("");
        if strip_cr(first) == "---" && span(&self.text).is_none() {
            return Err(FmError::UnsupportedLayout(
                "unterminated frontmatter".into(),
            ));
        }
        let (_, entries) = self.scan()?;
        if let Some(e) = entries.iter().find(|e| e.key == key.trim()) {
            if self.get(key.trim())?.as_ref() == Some(value) {
                return Ok(None);
            }
            if let Some(edit) = self.scalar_edit(e, value) {
                return Ok(Some(edit));
            }
        }
        let mut next = self.clone();
        if !next.set(key, value)? {
            return Ok(None);
        }
        let (a, b) = (self.text.as_bytes(), next.text.as_bytes());
        let mut pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
        while !self.text.is_char_boundary(pre) {
            pre -= 1;
        }
        let max_suf = a.len().min(b.len()) - pre;
        let mut suf = a
            .iter()
            .rev()
            .zip(b.iter().rev())
            .take(max_suf)
            .take_while(|(x, y)| x == y)
            .count();
        while !self.text.is_char_boundary(a.len() - suf) {
            suf -= 1;
        }
        Ok(Some((
            pre..a.len() - suf,
            next.text[pre..b.len() - suf].to_owned(),
        )))
    }

    /// The in-place scalar splice for a one-line `key: <plain token> [# comment]` entry whose
    /// key is written unquoted; `None` for any other shape (the caller falls back to a diff).
    fn scalar_edit(&self, e: &Entry, value: &JsValue) -> Option<(Range<usize>, String)> {
        if matches!(value, JsValue::Arr(_) | JsValue::Obj(_) | JsValue::Null) {
            return None;
        }
        let line = strip_cr(self.text[e.start..e.end].strip_suffix('\n')?);
        let after = line.strip_prefix(e.key.as_str())?.strip_prefix(':')?;
        let colon = e.start + e.key.len() + 1;
        let lead = after.len() - after.trim_start_matches([' ', '\t']).len();
        let tok = &after[lead..];
        let word = if tok.starts_with('#') {
            0
        } else {
            tok.find([' ', '\t']).unwrap_or(tok.len())
        };
        let tail = tok[word..].trim();
        if !(tail.is_empty() || tail.starts_with('#'))
            || tok[..word].starts_with(['"', '\'', '[', '{', '|', '>', '&', '*', '!'])
        {
            return None;
        }
        let rendered = render_scalar(value);
        if word == 0 {
            // `key:` / `key:   # c`: write ` value` after the colon, keep any comment.
            let sep = if tail.is_empty() { "" } else { " " };
            return Some((colon..colon + lead, format!(" {rendered}{sep}")));
        }
        let start = colon + lead;
        Some((start..start + word, rendered))
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

    /// Set only when the key is absent. Unlike [`Self::set_if_empty`], a present `null` or empty
    /// string is a value and is left untouched.
    pub fn set_if_absent(&mut self, key: &str, value: &JsValue) -> Result<bool, FmError> {
        let key = key.trim();
        if key.is_empty() {
            return Ok(false);
        }
        match self.get(key)? {
            None => self.set(key, value),
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
        if split_frontmatter(&self.text[bom_len(&self.text)..])
            .0
            .is_some()
        {
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
/// A scalar stringified (`Str` as is, numbers and booleans by `to_string`, arrays joined with `,`, null/objects empty).
pub fn js_text(v: &JsValue) -> String {
    match v {
        JsValue::Str(s) => s.clone(),
        JsValue::Num(n) => n.to_string(),
        JsValue::Bool(b) => b.to_string(),
        JsValue::Arr(a) => a.iter().map(js_text).collect::<Vec<_>>().join(","),
        JsValue::Null | JsValue::Obj(_) => String::new(),
    }
}

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
#[path = "frontmatter_tests.rs"]
mod tests;
