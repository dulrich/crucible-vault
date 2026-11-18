//! The fold layout: parse a fold block into groups, render and rebuild it, and group identity.
//! Decisions recorded here:
//! - **Fold layout**: disjoint groups are separated by a lone `>` line; each original line is
//!   prefixed `> ` (a blank line becomes `>`). A blank line precedes each close marker so the
//!   marker is not a lazy continuation of the callout (it would render as text inside it).

use cv_text::blocks;

use super::meta::{Anchor, MalformedMeta, Meta, parse_meta};
use crate::enrich::{MARK_CLOSE, MARK_OPEN, SpanKind, squeeze};

/// The one spelling of the fold callout's kind, so the kind, its marker and the header agree.
macro_rules! fold_kind {
    () => {
        "removed"
    };
}

/// The fold callout's kind (`removed`).
pub const FOLD_KIND: &str = fold_kind!();
/// The fold callout's marker (`[!removed]`).
pub const FOLD_MARKER: &str = concat!("[!", fold_kind!(), "]");
pub(crate) const FOLD_HEADER: &str = concat!("> [!", fold_kind!(), "]-");

/// The writer of every `> [!removed]-` fold block: `head` (the open marker, then the lines before
/// the header — metadata and any kept lines), the header, the content `chunks` (each non-empty,
/// stored `> `-quoted; a lone `>` separates consecutive chunks), then `tail` (the lines after the
/// content through the close marker).
pub(crate) fn write_fold(
    head: impl IntoIterator<Item = String>,
    header: &str,
    chunks: impl IntoIterator<Item = Vec<String>>,
    tail: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let mut out: Vec<String> = head.into_iter().collect();
    out.push(header.to_owned());
    for (k, c) in chunks.into_iter().enumerate() {
        if k > 0 {
            out.push(">".to_owned());
        }
        out.extend(c);
    }
    out.extend(tail);
    out
}

/// The tail of a written fold: a blank line ending the callout (a close marker right after a
/// `> ` line is a lazy continuation of it), then the close marker.
pub(crate) fn closing() -> [String; 2] {
    [String::new(), MARK_CLOSE.to_owned()]
}

/// A removed line as stored in the fold (`> line`, a blank one as a bare `>`).
pub(crate) fn quote(l: &str) -> String {
    if l.trim().is_empty() {
        ">".to_owned()
    } else {
        format!("> {l}")
    }
}

/// A fold content line back to the original line (inverse of [`quote`]; hand-written lines without
/// the `> ` prefix are kept as they are).
fn unquote(l: &str) -> &str {
    if l.trim_end() == ">" {
        ""
    } else {
        l.strip_prefix("> ").unwrap_or(l)
    }
}

/// One group of a parsed fold block.
#[derive(Debug, Clone)]
pub(super) struct PGroup {
    /// The metadata line (new layout); `None` for an old-layout group.
    pub(super) meta: Option<String>,
    /// Content lines as stored (`> `-prefixed), without separators.
    pub(super) raw: Vec<String>,
    pub(super) kind: Option<SpanKind>,
    pub(super) anchor: Option<Anchor>,
}

impl PGroup {
    pub(super) fn lines(&self) -> Vec<String> {
        self.raw.iter().map(|l| unquote(l).to_owned()).collect()
    }
    pub(super) fn has_text(&self) -> bool {
        self.raw.iter().any(|l| !unquote(l).trim().is_empty())
    }
}

/// A well-formed `%% cv-enrich %%` block holding a `> [!removed]-` callout.
#[derive(Debug, Clone)]
pub(super) struct PFold {
    pub(super) open: usize,
    pub(super) close: usize,
    /// Non-metadata lines between the open marker and the header (kept on rebuild).
    pub(super) pre: Vec<String>,
    pub(super) header: String,
    pub(super) groups: Vec<PGroup>,
}

/// Old-layout content: groups split at lone `>` lines (the pre-metadata reader rule).
fn old_groups(content: &[&str], groups: &mut Vec<PGroup>) {
    let mut cur: Vec<String> = Vec::new();
    let mut flush = |cur: &mut Vec<String>| {
        let g = PGroup {
            meta: None,
            raw: std::mem::take(cur),
            kind: None,
            anchor: None,
        };
        if g.has_text() {
            groups.push(g);
        }
    };
    for l in content {
        if l.trim_end() == ">" {
            flush(&mut cur);
        } else {
            cur.push((*l).to_owned());
        }
    }
    flush(&mut cur);
}

/// The new-layout groups own the tail of the content, last metadata line ↔ last group, each
/// `lines=` long and preceded by a lone `>` separator when anything precedes it. Returns the
/// length of the old-layout prefix and each group's range, or `None` when the counts do not fit.
fn split_new(content: &[&str], metas: &[Meta]) -> Option<(usize, Vec<(usize, usize)>)> {
    let mut end = content.len();
    let mut ranges = Vec::with_capacity(metas.len());
    for m in metas.iter().rev() {
        let s = end.checked_sub(m.n.get())?;
        ranges.push((s, end));
        end = s;
        if end > 0 {
            if content[end - 1].trim_end() != ">" {
                return None;
            }
            end -= 1;
        }
    }
    ranges.reverse();
    Some((end, ranges))
}

pub(super) fn parse_folds(raw: &[&str]) -> Vec<PFold> {
    let mut folds = Vec::new();
    for (i, j) in blocks::generated(raw).blocks {
        let inner = &raw[i + 1..j];
        if let Some(h) = inner.iter().position(|l| l.trim_end() == FOLD_HEADER) {
            let mut content = &inner[h + 1..];
            while content.last().is_some_and(|l| l.trim().is_empty()) {
                content = &content[..content.len() - 1];
            }
            let mut pre = Vec::new();
            let mut metas: Vec<(String, Result<Meta, MalformedMeta>)> = Vec::new();
            for l in &inner[..h] {
                match parse_meta(l) {
                    Some(m) => metas.push(((*l).to_owned(), m)),
                    None => pre.push((*l).to_owned()),
                }
            }
            let ms: Option<Vec<Meta>> = metas.iter().map(|(_, m)| m.clone().ok()).collect();
            let mut groups = Vec::new();
            let split = ms
                .filter(|ms| !ms.is_empty())
                .and_then(|ms| split_new(content, &ms).map(|sp| (ms, sp)));
            match split {
                Some((ms, (old, ranges))) => {
                    old_groups(&content[..old], &mut groups);
                    let lines = metas.into_iter().map(|(l, _)| l);
                    for ((line, m), (s, e)) in lines.zip(ms).zip(ranges) {
                        let g = PGroup {
                            meta: Some(line),
                            raw: content[s..e].iter().map(|l| (*l).to_owned()).collect(),
                            kind: m.kind,
                            anchor: m.anchor,
                        };
                        if g.has_text() {
                            groups.push(g);
                        }
                    }
                }
                None => old_groups(content, &mut groups),
            }
            folds.push(PFold {
                open: i,
                close: j,
                pre,
                header: inner[h].to_owned(),
                groups,
            });
        }
    }
    folds
}

/// FNV-1a (64-bit) folded to 32 bits: `cv-ingest` has no hash crate on its normal edges.
fn fnv1a(s: &str) -> u32 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    #[allow(clippy::cast_possible_truncation)]
    let folded = (h ^ (h >> 32)) as u32;
    folded
}

/// Group identities in order: 8 hex digits of the squeezed lines' hash, `-2`, `-3`, … for a
/// repeat of the same content.
pub(super) fn group_ids<'a>(groups: impl Iterator<Item = &'a PGroup>) -> Vec<String> {
    let mut seen: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    groups
        .map(|g| {
            let key: Vec<String> = g.raw.iter().map(|l| squeeze(unquote(l))).collect();
            let base = format!("{:08x}", fnv1a(&key.join("\n")));
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            if *n == 1 { base } else { format!("{base}-{n}") }
        })
        .collect()
}

pub(super) fn rebuild_fold(f: &PFold, close: &str) -> Vec<String> {
    let head = std::iter::once(MARK_OPEN.to_owned())
        .chain(f.pre.iter().cloned())
        .chain(f.groups.iter().filter_map(|g| g.meta.clone()));
    // Old-layout groups first: the metadata owns the tail of the content.
    let (old, new): (Vec<&PGroup>, Vec<&PGroup>) = f.groups.iter().partition(|g| g.meta.is_none());
    let chunks = old.into_iter().chain(new).map(|g| g.raw.clone());
    write_fold(head, &f.header, chunks, [String::new(), close.to_owned()])
}
