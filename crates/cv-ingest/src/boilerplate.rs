//! Per-sender newsletter boilerplate: lines that recur across messages of one sender are learned
//! into a [`LineSet`] and stripped from the anchored head and tail of later bodies. Pure: the
//! caller (the daemon) persists the set.
//!
//! The caller learns from each message's pre-strip cleaned body, then strips. A message that was
//! learned into the set sees its own lines at count 1, so a line is boilerplate (count >= 2) only
//! when it appeared in at least one other message.

use std::collections::{HashMap, HashSet};

/// Lines of at most this many characters are neutral: never counted, never break a run.
const MIN_CHARS: usize = 3;
/// Non-boilerplate lines tolerated inside a head/tail run.
const MAX_GAP: usize = 2;
/// Count at which a line is boilerplate.
const THRESHOLD: u32 = 2;

/// Trims and collapses inner whitespace; `None` for blank or very short (<= 3 chars) lines.
pub fn normalize_line(line: &str) -> Option<String> {
    let n = line.split_whitespace().collect::<Vec<_>>().join(" ");
    (n.chars().count() > MIN_CHARS).then_some(n)
}

/// FNV-1a 64 of the normalized line: stable across runs and platforms.
pub fn line_key(normalized: &str) -> u64 {
    normalized.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// How many messages of one sender contained each normalized line, keyed by [`line_key`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineSet {
    counts: HashMap<u64, u32>,
}

impl LineSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuilds a set from persisted `(key, count)` entries.
    pub fn from_entries(entries: impl IntoIterator<Item = (u64, u32)>) -> Self {
        Self {
            counts: entries.into_iter().collect(),
        }
    }

    /// The `(key, count)` entries, for persistence.
    pub fn iter(&self) -> impl Iterator<Item = (u64, u32)> + '_ {
        self.counts.iter().map(|(k, c)| (*k, *c))
    }

    pub fn len(&self) -> usize {
        self.counts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }

    pub fn count(&self, normalized: &str) -> u32 {
        self.counts.get(&line_key(normalized)).copied().unwrap_or(0)
    }
}

/// Which lines sit inside (or on the delimiter of) a fenced code block (also the mail-clean pass's
/// fence mask: one copy in the crate).
pub(crate) fn fence_mask(lines: &[&str]) -> Vec<bool> {
    let mut open: Option<&str> = None;
    lines
        .iter()
        .map(|l| {
            let t = l.trim_start();
            let fence = ["```", "~~~"].into_iter().find(|f| t.starts_with(f));
            match (open, fence) {
                (Some(o), Some(f)) if f == o => {
                    open = None;
                    true
                }
                (None, Some(f)) => {
                    open = Some(f);
                    true
                }
                (o, _) => o.is_some(),
            }
        })
        .collect()
}

/// Counts each distinct normalized line of one message once. Call with the pre-strip cleaned body.
pub fn learn(set: &mut LineSet, body: &str) {
    let mut seen = HashSet::new();
    for n in body.lines().filter_map(normalize_line) {
        let k = line_key(&n);
        if seen.insert(k) {
            *set.counts.entry(k).or_insert(0) += 1;
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Neutral,
    Boiler,
    Content,
}

/// Length of the anchored run walking `order` (indices from one end): boilerplate lines, with up
/// to [`MAX_GAP`] content lines tolerated when a boilerplate line follows. Returns the number of
/// positions of `order` covered.
fn run_len(order: &[usize], kinds: &[Kind]) -> usize {
    let mut end = 0;
    let mut gap = 0;
    for (pos, &i) in order.iter().enumerate() {
        match kinds[i] {
            Kind::Neutral => {}
            Kind::Boiler => {
                end = pos + 1;
                gap = 0;
            }
            Kind::Content => {
                gap += 1;
                if gap > MAX_GAP {
                    break;
                }
            }
        }
    }
    end
}

/// Strips the anchored head and tail runs of boilerplate lines (count >= 2 in `set`, so seen in
/// at least one other message when the caller learned this body first). Mid-body boilerplate is
/// kept; if head+tail would remove more than half of the non-blank lines the body is unchanged.
pub fn strip(body: &str, set: &LineSet) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let fenced = fence_mask(&lines);
    let kinds: Vec<Kind> = lines
        .iter()
        .zip(&fenced)
        .map(|(l, &f)| match normalize_line(l) {
            _ if f => Kind::Content,
            None => Kind::Neutral,
            Some(n) if set.count(&n) >= THRESHOLD => Kind::Boiler,
            Some(_) => Kind::Content,
        })
        .collect();
    let n = lines.len();
    let fwd: Vec<usize> = (0..n).collect();
    let back: Vec<usize> = (0..n).rev().collect();
    let head = run_len(&fwd, &kinds);
    let tail = run_len(&back, &kinds);
    let mut remove = vec![false; n];
    remove[..head].iter_mut().for_each(|r| *r = true);
    remove[n - tail..].iter_mut().for_each(|r| *r = true);
    let nonblank = lines.iter().filter(|l| !l.trim().is_empty()).count();
    let removed = lines
        .iter()
        .zip(&remove)
        .filter(|(l, r)| **r && !l.trim().is_empty())
        .count();
    if removed == 0 || removed * 2 > nonblank {
        return body.to_owned();
    }
    let kept: Vec<&str> = lines
        .iter()
        .zip(&remove)
        .filter(|(_, r)| !**r)
        .map(|(l, _)| *l)
        .collect();
    let start = kept.iter().position(|l| !l.trim().is_empty());
    let last = kept.iter().rposition(|l| !l.trim().is_empty());
    match (start, last) {
        (Some(s), Some(e)) => kept[s..=e].join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_of(msgs: &[&str]) -> LineSet {
        let mut s = LineSet::new();
        msgs.iter().for_each(|m| learn(&mut s, m));
        s
    }

    const FOOT: &str = "You are receiving this newsletter\nManage your preferences here\nCompany Inc, 1 Main Street";

    #[test]
    fn shared_footer_is_stripped_from_a_third_message() {
        let a = format!("Story one body line\n\n{FOOT}");
        let b = format!("Different story entirely\n\n{FOOT}");
        let c =
            format!("Third story with words\nmore of the third story\nand a third line\n\n{FOOT}");
        let mut s = set_of(&[&a, &b]);
        learn(&mut s, &c);
        assert_eq!(
            strip(&c, &s),
            "Third story with words\nmore of the third story\nand a third line"
        );
    }

    #[test]
    fn mid_body_repeats_are_kept_and_gaps_tolerated() {
        let a = "# Weekly Digest\nalpha alpha alpha\n# Weekly Digest\nbeta beta beta\nSign off line\nUnsub link here";
        let s = set_of(&[a, a]);
        let x = "# Weekly Digest\nreal content one\nreal content two\nreal content three\nreal content four\n# Weekly Digest\nmore real one\nmore real two\nmore real three\nSign off line\nUnsub link here";
        let out = strip(x, &s);
        assert!(out.starts_with("real content one"));
        assert!(out.contains("real content four\n# Weekly Digest\nmore real one"));
        assert!(!out.contains("Sign off line") && !out.contains("Unsub link"));
        let g = "# Weekly Digest\ngap line one\nSign off line\nbody one body\nbody two body\nbody three body\nbody four body\nbody five body";
        assert!(!strip(g, &s).contains("gap line one"));
    }

    #[test]
    fn near_total_match_is_refused_and_single_sender_unchanged() {
        let a = "Header line one\nHeader line two\nFooter line one";
        let s = set_of(&[a, a]);
        assert_eq!(strip(a, &s), a);
        let one = set_of(&[a]);
        assert_eq!(strip(a, &one), a);
    }

    #[test]
    fn strip_is_idempotent_and_fences_survive() {
        let a = format!("Body of story one\n\n{FOOT}");
        let b = format!("Body of story two\n```\n{FOOT}\n```\nmore text here\n\n{FOOT}");
        let s = set_of(&[&a, &a, &b]);
        let once = strip(&b, &s);
        assert!(once.contains("```\nYou are receiving"));
        assert_eq!(strip(&once, &s), once);
    }

    #[test]
    fn normalize_and_hash_are_stable() {
        assert_eq!(
            normalize_line("  a   b\tcdef  ").as_deref(),
            Some("a b cdef")
        );
        assert_eq!(normalize_line("abc"), None);
        assert_eq!(normalize_line("   "), None);
        assert_eq!(line_key("hello world"), 0x779a_65e7_023c_d2e7);
        let s = set_of(&["hello world line"]);
        assert_eq!(LineSet::from_entries(s.iter().collect::<Vec<_>>()), s);
    }
}
