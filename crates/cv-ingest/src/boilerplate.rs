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

/// FNV-1a 64 of the normalized line, exactly as written: stable across runs and platforms. This
/// is the **stored** key only (`removal_labels.line_key`, written as before); every tier —
/// learning, recurrence and label lookup — keys lines by [`masked_key`].
pub fn line_key(normalized: &str) -> u64 {
    normalized.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// English month and weekday names (full and three-letter forms) masked by [`mask`].
const DATE_WORDS: [&str; 38] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    "jan",
    "feb",
    "mar",
    "apr",
    "jun",
    "jul",
    "aug",
    "sep",
    "sept",
    "oct",
    "nov",
    "dec",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "mon",
    "tue",
    "wed",
    "thu",
    "fri",
    "sat",
    "sun",
];
/// Placeholder for a digit run (a control char, so it never collides with literal text).
const DIGITS: char = '\u{1}';
/// Placeholder for a month or weekday name.
const DATE_WORD: char = '\u{2}';

/// The date-masked form of a normalized line (plan `abstract-removal-confidence-deletion` WP-4,
/// `docs/spikes/removal-tiers/mask_sim.pl`): each digit run (URLs included) becomes one
/// placeholder, and each whole word (a maximal run of alphanumerics and `_`) that is an English
/// month or weekday name, case-insensitively, becomes another. A word mixing letters and digits
/// (`Jan5`) only has its digits masked, as the simulation's `\b` does.
pub fn mask(normalized: &str) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(normalized.len());
    let mut rest = normalized;
    while let Some(c) = rest.chars().next() {
        if !word(c) {
            out.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        }
        let end = rest.find(|c: char| !word(c)).unwrap_or(rest.len());
        let w = &rest[..end];
        rest = &rest[end..];
        if DATE_WORDS.iter().any(|d| d.eq_ignore_ascii_case(w)) {
            out.push(DATE_WORD);
            continue;
        }
        let mut in_digits = false;
        for ch in w.chars() {
            if ch.is_ascii_digit() {
                if !in_digits {
                    out.push(DIGITS);
                }
                in_digits = true;
            } else {
                out.push(ch);
                in_digits = false;
            }
        }
    }
    out
}

/// The one tier key of a normalized line: [`line_key`] of its [`mask`]ed form. Learning
/// ([`learn`], the persisted `mail_boilerplate` sets), recurrence and label lookup ([`Labels`])
/// all go through it, for mail and RSS.
pub fn masked_key(normalized: &str) -> u64 {
    line_key(&mask(normalized))
}

/// How many messages of one sender contained each normalized line, keyed by [`masked_key`].
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
        self.counts
            .get(&masked_key(normalized))
            .copied()
            .unwrap_or(0)
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
        let k = masked_key(&n);
        if seen.insert(k) {
            *set.counts.entry(k).or_insert(0) += 1;
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Neutral,
    /// Boilerplate; `delete` = a Delete label (not only recurrence) made it so.
    Boiler {
        delete: bool,
    },
    Content,
    /// Content because a Keep label vetoed it (never removed, even as a run's gap line).
    Kept,
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
            Kind::Boiler { .. } => {
                end = pos + 1;
                gap = 0;
            }
            Kind::Content | Kind::Kept => {
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
/// The publication's removal labels apply ([`plan`] with [`Recur::Mail`]): a Keep-labelled line is never stripped, a Delete-labelled line is boilerplate in an anchored run.
pub fn strip_tiered(body: &str, set: &LineSet, labels: &Labels) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let p = plan(&lines, &[], set, labels, Recur::Mail);
    if !p.actions.contains(&Action::Strip) {
        return body.to_owned();
    }
    let kept: Vec<&str> = lines
        .iter()
        .zip(&p.actions)
        .filter(|(_, a)| **a != Action::Strip)
        .map(|(l, _)| *l)
        .collect();
    let start = kept.iter().position(|l| !l.trim().is_empty());
    let last = kept.iter().rposition(|l| !l.trim().is_empty());
    match (start, last) {
        (Some(s), Some(e)) => kept[s..=e].join("\n"),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------------------------
// Tiers (plan `intake-removals-sources-rescan` WP-5)
// ---------------------------------------------------------------------------------------------

/// RSS recurrence: a line is recurring once it was seen in at least this many learned posts…
pub const RSS_MIN_POSTS: u32 = 3;
/// …and in at least this percentage of the publication's learned posts.
pub const RSS_MIN_PERCENT: u32 = 30;

/// One publication's removal labels as [`masked_key`] sets. Precedence per masked key:
/// **Keep** (never stripped, never folded; vetoes a Delete label and recurrence) > **Delete**
/// (boilerplate in an anchored head/tail run: stripped outright) > **recurrence** (mail:
/// stripped, RSS: folded). Mid-body lines are never touched, whatever their tier. Because the key
/// is masked, a Delete recorded on a dated header applies to the same header with another date
/// (anchored runs only), and a Keep on either date wins (plan
/// `abstract-removal-confidence-deletion` Decision 6).
///
/// Build it from the stored label lines with [`Labels::from_lines`] (never from the stored
/// exact `line_key` column); ask [`Labels::is_kept`] / [`Labels::is_deleted`] about a body line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Labels {
    pub keep: HashSet<u64>,
    pub delete: HashSet<u64>,
}

impl Labels {
    /// Keys the stored label lines (`removal_labels.line`, as `cv_state::PublicationLabels`
    /// returns them) with [`masked_key`]. A line [`normalize_line`] rejects has no key and is
    /// skipped.
    pub fn from_lines<'a>(
        keep: impl IntoIterator<Item = &'a str>,
        delete: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let keys = |v: &mut dyn Iterator<Item = &'a str>| -> HashSet<u64> {
            v.filter_map(normalize_line)
                .map(|n| masked_key(&n))
                .collect()
        };
        Self {
            keep: keys(&mut keep.into_iter()),
            delete: keys(&mut delete.into_iter()),
        }
    }

    /// The masked key of a raw body `line`; `None` when [`normalize_line`] rejects it.
    fn key_of(line: &str) -> Option<u64> {
        normalize_line(line).map(|n| masked_key(&n))
    }

    /// Whether the raw body `line`'s masked key has a Keep label.
    pub fn is_kept(&self, line: &str) -> bool {
        Self::key_of(line).is_some_and(|k| self.keep.contains(&k))
    }

    /// Whether the raw body `line`'s masked key has a Delete label and no Keep label (Keep wins).
    pub fn is_deleted(&self, line: &str) -> bool {
        Self::key_of(line).is_some_and(|k| self.delete.contains(&k) && !self.keep.contains(&k))
    }
}

/// The source kind deciding the recurrence tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recur {
    /// Mail: recurring at count >= 2 (one other message); recurring lines are stripped.
    Mail,
    /// RSS: recurring at count >= [`RSS_MIN_POSTS`] and >= [`RSS_MIN_PERCENT`] % of `posts`
    /// (the publication's learned posts); recurring lines are folded, never stripped.
    Rss { posts: u32 },
}

impl Recur {
    fn recurs(self, count: u32) -> bool {
        match self {
            Self::Mail => count >= THRESHOLD,
            Self::Rss { posts } => {
                count >= RSS_MIN_POSTS
                    && u64::from(count) * 100 >= u64::from(posts) * u64::from(RSS_MIN_PERCENT)
            }
        }
    }
}

/// What happens to one body line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Stays in the body.
    Keep,
    /// Removed outright.
    Strip,
    /// Moved into the `[!removed]` fold as part of the head (`chrome`) run (RSS only).
    FoldHead,
    /// Moved into the `[!removed]` fold as part of the tail (`footer`) run (RSS only).
    FoldTail,
}

/// [`plan`]'s result: one action per input line plus per-tier line counts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub actions: Vec<Action>,
    /// Lines a Keep label vetoed (a Delete label or recurrence would have made them boilerplate).
    pub vetoed: usize,
    /// Non-blank Delete-labelled lines stripped.
    pub deleted: usize,
    /// Non-blank lines removed by recurrence (mail: stripped, RSS: folded), gap lines included.
    pub recurred: usize,
    /// An anchored run existed but the half-body guard refused it.
    pub guarded: bool,
}

/// Classifies `lines` (one body) against the learned `set` and the publication's `labels`.
/// `protected[i]` (missing = false) marks a line that is never boilerplate and never removed
/// (generated blocks, an existing fold). The anchored head/tail run, gap and half-body guards are
/// [`strip`]'s. In a run: mail strips every line; RSS strips Delete-labelled lines and folds the
/// rest (gap and blank lines included, so a group is contiguous).
pub fn plan(
    lines: &[&str],
    protected: &[bool],
    set: &LineSet,
    labels: &Labels,
    recur: Recur,
) -> Plan {
    let fenced = fence_mask(lines);
    let prot = |i: usize| protected.get(i).copied().unwrap_or(false);
    let mut vetoed = 0;
    let kinds: Vec<Kind> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if fenced[i] || prot(i) {
                return Kind::Content;
            }
            let Some(n) = normalize_line(l) else {
                return Kind::Neutral;
            };
            let k = masked_key(&n);
            let deleted = labels.delete.contains(&k);
            let recurs = recur.recurs(set.counts.get(&k).copied().unwrap_or(0));
            if labels.keep.contains(&k) {
                vetoed += usize::from(deleted || recurs);
                Kind::Kept
            } else if deleted || recurs {
                Kind::Boiler { delete: deleted }
            } else {
                Kind::Content
            }
        })
        .collect();
    let n = lines.len();
    let fwd: Vec<usize> = (0..n).collect();
    let back: Vec<usize> = (0..n).rev().collect();
    let head = run_len(&fwd, &kinds);
    let tail = run_len(&back, &kinds);
    let mut actions = vec![Action::Keep; n];
    let blank = |i: usize| lines[i].trim().is_empty();
    for (i, a) in actions.iter_mut().enumerate() {
        let (in_head, in_tail) = (i < head, i >= n - tail);
        // A Keep-labelled line stays even as a gap line inside a run.
        if !(in_head || in_tail) || prot(i) || kinds[i] == Kind::Kept {
            continue;
        }
        *a = match recur {
            Recur::Mail => Action::Strip,
            Recur::Rss { .. } if kinds[i] == (Kind::Boiler { delete: true }) => Action::Strip,
            Recur::Rss { .. } if in_head => Action::FoldHead,
            Recur::Rss { .. } => Action::FoldTail,
        };
    }
    // A fold group neither starts nor ends with a blank line (they stay in the body).
    let mut i = 0;
    while i < n {
        let a = actions[i];
        if !matches!(a, Action::FoldHead | Action::FoldTail) {
            i += 1;
            continue;
        }
        let s = i;
        while i < n && actions[i] == a {
            i += 1;
        }
        let mut e = i;
        let mut b = s;
        while b < e && blank(b) {
            actions[b] = Action::Keep;
            b += 1;
        }
        while e > b && blank(e - 1) {
            actions[e - 1] = Action::Keep;
            e -= 1;
        }
    }
    let nonblank = (0..n).filter(|&i| !blank(i)).count();
    let removed = (0..n)
        .filter(|&i| actions[i] != Action::Keep && !blank(i))
        .count();
    if removed == 0 || removed * 2 > nonblank {
        return Plan {
            actions: vec![Action::Keep; n],
            vetoed,
            guarded: removed > 0,
            ..Plan::default()
        };
    }
    let deleted = (0..n)
        .filter(|&i| {
            kinds[i] == (Kind::Boiler { delete: true }) && actions[i] == Action::Strip && !blank(i)
        })
        .count();
    Plan {
        recurred: removed - deleted,
        actions,
        vetoed,
        deleted,
        guarded: false,
    }
}

/// Applies the tiers to one note body: `None` when nothing changes. Only lines outside generated
/// blocks are classified; the anchored runs are taken over those lines. Mail strips (a body
/// without generated blocks goes through [`strip_tiered`] with its surrounding newlines kept);
/// RSS strips Delete-labelled lines and folds the rest of each run as `chrome` (head) / `footer`
/// (tail) groups via [`crate::enrich::fold_ranges`], which may refuse the fold.
pub fn apply(
    body: &str,
    set: &LineSet,
    labels: &Labels,
    recur: Recur,
) -> Result<(Option<String>, Plan), crate::enrich::FoldError> {
    use crate::enrich::{FoldRange, SpanKind, fold_ranges};
    let raw: Vec<&str> = body.split('\n').collect();
    let classes = cv_text::blocks::generated(&raw).classes;
    if recur == Recur::Mail && !classes.iter().any(|c| c.is_generated()) {
        let lines: Vec<&str> = body.lines().collect();
        let plan = plan(&lines, &[], set, labels, recur);
        return Ok((strip_plain_mail(body, set, labels), plan));
    }
    let plain: Vec<usize> = (0..raw.len())
        .filter(|&i| !classes[i].is_generated())
        .collect();
    let lines: Vec<&str> = plain.iter().map(|&i| raw[i]).collect();
    let plan = plan(&lines, &[], set, labels, recur);
    let mut act = vec![Action::Keep; raw.len()];
    for (k, &i) in plain.iter().enumerate() {
        act[i] = plan.actions[k];
    }
    if act.iter().all(|a| *a == Action::Keep) {
        return Ok((None, plan));
    }
    // Remove the stripped lines; a blank line left next to a cut collapses into its neighbour.
    let mut kept: Vec<(usize, &str)> = Vec::with_capacity(raw.len());
    let mut cut = false;
    for (i, l) in raw.iter().enumerate() {
        if act[i] == Action::Strip {
            cut = true;
            continue;
        }
        let blank = l.trim().is_empty() && !classes[i].is_generated();
        if blank && cut && kept.last().is_none_or(|(_, p)| p.trim().is_empty()) {
            continue;
        }
        if !blank {
            cut = false;
        }
        kept.push((i, l));
    }
    let new_body: String = kept.iter().map(|(_, l)| *l).collect::<Vec<_>>().join("\n");
    let mut ranges: Vec<FoldRange> = Vec::new();
    for (k, (i, _)) in kept.iter().enumerate() {
        let kind = match act[*i] {
            Action::FoldHead => SpanKind::Chrome,
            Action::FoldTail => SpanKind::Footer,
            _ => continue,
        };
        match ranges.last_mut() {
            Some(r) if r.last + 1 == k && kept[r.last].0 + 1 == *i && r.kind == kind => {
                r.last = k;
            }
            _ => ranges.push(FoldRange {
                first: k,
                last: k,
                kind,
            }),
        }
    }
    let out = fold_ranges(&new_body, &ranges)?;
    Ok(((out != body).then_some(out), plan))
}

/// A mail body without generated blocks stripped by [`strip_tiered`], its leading and trailing
/// newlines kept byte-for-byte; `None` when nothing changes.
fn strip_plain_mail(body: &str, set: &LineSet, labels: &Labels) -> Option<String> {
    let newline = |c: char| c == '\n' || c == '\r';
    let core = body.trim_matches(newline);
    if core.is_empty() {
        return None;
    }
    let lead = &body[..body.len() - body.trim_start_matches(newline).len()];
    let trail = &body[body.trim_end_matches(newline).len()..];
    let out = format!("{lead}{}{trail}", strip_tiered(core, set, labels));
    (out != body).then_some(out)
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
            strip_tiered(&c, &s, &Labels::default()),
            "Third story with words\nmore of the third story\nand a third line"
        );
    }

    #[test]
    fn mid_body_repeats_are_kept_and_gaps_tolerated() {
        let a = "# Weekly Digest\nalpha alpha alpha\n# Weekly Digest\nbeta beta beta\nSign off line\nUnsub link here";
        let s = set_of(&[a, a]);
        let x = "# Weekly Digest\nreal content one\nreal content two\nreal content three\nreal content four\n# Weekly Digest\nmore real one\nmore real two\nmore real three\nSign off line\nUnsub link here";
        let out = strip_tiered(x, &s, &Labels::default());
        assert!(out.starts_with("real content one"));
        assert!(out.contains("real content four\n# Weekly Digest\nmore real one"));
        assert!(!out.contains("Sign off line") && !out.contains("Unsub link"));
        let g = "# Weekly Digest\ngap line one\nSign off line\nbody one body\nbody two body\nbody three body\nbody four body\nbody five body";
        assert!(!strip_tiered(g, &s, &Labels::default()).contains("gap line one"));
    }

    #[test]
    fn near_total_match_is_refused_and_single_sender_unchanged() {
        let a = "Header line one\nHeader line two\nFooter line one";
        let s = set_of(&[a, a]);
        assert_eq!(strip_tiered(a, &s, &Labels::default()), a);
        let one = set_of(&[a]);
        assert_eq!(strip_tiered(a, &one, &Labels::default()), a);
    }

    #[test]
    fn strip_is_idempotent_and_fences_survive() {
        let a = format!("Body of story one\n\n{FOOT}");
        let b = format!("Body of story two\n```\n{FOOT}\n```\nmore text here\n\n{FOOT}");
        let s = set_of(&[&a, &a, &b]);
        let once = strip_tiered(&b, &s, &Labels::default());
        assert!(once.contains("```\nYou are receiving"));
        assert_eq!(strip_tiered(&once, &s, &Labels::default()), once);
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
    // ---- tiers (plan intake-removals-sources-rescan WP-5) ----

    fn labels(keep: &[&str], delete: &[&str]) -> Labels {
        Labels::from_lines(keep.iter().copied(), delete.iter().copied())
    }

    #[test]
    fn mask_matches_the_simulation() {
        assert_eq!(
            mask("Example News AM: Monday, Nov 3 2025"),
            "Example News AM: \u{2}, \u{2} \u{1} \u{1}"
        );
        assert_eq!(
            mask("see https://x.io/p/12345?a=7 MAY may Jan5 Sept_ Sunday"),
            "see https://x.io/p/\u{1}?a=\u{1} \u{2} \u{2} Jan\u{1} Sept_ \u{2}"
        );
        assert_eq!(mask("Monsoon march-on"), "Monsoon \u{2}-on");
        assert_eq!(masked_key("Issue 12"), masked_key("Issue 9876"));
        assert_ne!(line_key("Issue 12"), line_key("Issue 9876"));
        // Masking never makes a normalized line keyless: the length gate is normalize_line's.
        assert_eq!(normalize_line("12 3").as_deref(), Some("12 3"));
    }

    #[test]
    fn headers_differing_only_by_date_are_both_stripped() {
        let a = "The Daily Brief for Monday, Nov 3 2025\nStory alpha one here\nStory alpha two here\nStory alpha three";
        let b = "The Daily Brief for Friday, Dec 12 2025\nStory beta one here\nStory beta two here\nStory beta three";
        let s = set_of(&[a, b]);
        assert!(strip_tiered(a, &s, &Labels::default()).starts_with("Story alpha one"));
        assert!(strip_tiered(b, &s, &Labels::default()).starts_with("Story beta one"));
    }

    #[test]
    fn exact_labels_still_apply_and_keep_beats_a_masked_delete() {
        let body = "Edition of 3 Nov 2025\nBody a line here\nBody b line here\nBody c line here\nBody d line here";
        let other = "Edition of 12 Dec 2025\nBody a line here\nBody b line here\nBody c line here\nBody d line here";
        // A Delete recorded on another date's header strips this one; a Keep on the exact line vetoes.
        let l = labels(&[], &["Edition of 12 Dec 2025"]);
        assert!(strip_tiered(body, &LineSet::new(), &l).starts_with("Body a line here"));
        assert!(l.is_deleted(body.lines().next().unwrap()) && !l.is_kept(other));
        let l = labels(&["Edition of 3 Nov 2025"], &["Edition of 3 Nov 2025"]);
        assert!(strip_tiered(body, &LineSet::new(), &l).starts_with("Edition of 3 Nov"));
        let l = labels(&["Edition of 3 Nov 2025"], &["Edition of 12 Dec 2025"]);
        assert!(l.is_kept("Edition  of 1 Jan 2020") && !l.is_deleted("Edition of 1 Jan 2020"));
        assert!(strip_tiered(body, &LineSet::new(), &l).starts_with("Edition of 3 Nov"));
        // A Keep beats learned recurrence on the masked key too.
        let s = set_of(&[body, other]);
        assert!(strip_tiered(body, &s, &l).starts_with("Edition of 3 Nov"));
    }

    #[test]
    fn masked_delete_is_ignored_outside_an_anchored_run() {
        let body = "Body a line here\nBody b line here\nBody c line here\nRecap for 4 Mar 2024\nBody d line here\nBody e line here\nBody f line here";
        let l = labels(&[], &["Recap for 9 Apr 2023"]);
        assert_eq!(strip_tiered(body, &LineSet::new(), &l), body);
    }

    const BODY: &str = "Header chrome line\nStory line one here\nStory line two here\nStory line three here\nStory line four here\nSigned off by staff\nFooter one recurring\nFooter two recurring";

    #[test]
    fn mail_tiers_keep_vetoes_delete_strips_anchored_only() {
        let other = "Other story alpha\nOther story beta\nOther story gamma\nFooter one recurring\nFooter two recurring";
        let mut s = set_of(&[other]);
        learn(&mut s, BODY);
        // Recurrence only: the footer goes.
        assert!(!strip_tiered(BODY, &s, &Labels::default()).contains("Footer one"));
        // Keep vetoes recurrence for one line; the other recurring line still goes.
        let l = labels(&["Footer one recurring"], &[]);
        let out = strip_tiered(BODY, &s, &l);
        assert!(out.contains("Footer one recurring") && !out.contains("Footer two"));
        assert_eq!(
            plan(&BODY.lines().collect::<Vec<_>>(), &[], &s, &l, Recur::Mail).vetoed,
            1
        );
        // Delete label: stripped at the head (never seen elsewhere), left alone mid-body.
        let long = "Header chrome line\nBody a line here\nBody b line here\nBody c line here\nBody d line here\nMid label line\nBody e line here\nBody f line here\nBody g line here\nBody h line here";
        let l = labels(&[], &["Header chrome line", "Mid label line"]);
        let out = strip_tiered(long, &LineSet::new(), &l);
        assert!(out.starts_with("Body a line here"));
        assert!(out.contains("Mid label line"));
        // Keep beats Delete for the same line.
        let l = labels(&["Header chrome line"], &["Header chrome line"]);
        assert!(strip_tiered(BODY, &s, &l).starts_with("Header chrome line"));
    }

    #[test]
    fn rss_tiers_fold_recurrence_strip_deletes_and_respect_the_share() {
        let lines: Vec<&str> = BODY.lines().collect();
        let foot = "Story x\nFooter one recurring\nFooter two recurring";
        let three = set_of(&[foot, foot, foot]);
        let two = set_of(&[foot, foot]);
        let none = Labels::default();
        // 2 posts: nothing.
        let p = plan(&lines, &[], &two, &none, Recur::Rss { posts: 2 });
        assert!(p.actions.iter().all(|a| *a == Action::Keep));
        // 3 posts under 30 % (3 of 11): nothing.
        let p = plan(&lines, &[], &three, &none, Recur::Rss { posts: 11 });
        assert!(p.actions.iter().all(|a| *a == Action::Keep));
        // 3 posts at 30 % (3 of 10): the tail is folded as a footer run, never stripped.
        let p = plan(&lines, &[], &three, &none, Recur::Rss { posts: 10 });
        assert_eq!(&p.actions[6..], [Action::FoldTail, Action::FoldTail]);
        assert!(p.actions[..6].iter().all(|a| *a == Action::Keep));
        assert_eq!((p.recurred, p.deleted), (2, 0));
        // A Delete-labelled head line is stripped, a Keep-labelled footer line stays.
        let l = labels(&["Footer two recurring"], &["Header chrome line"]);
        let p = plan(&lines, &[], &three, &l, Recur::Rss { posts: 10 });
        assert_eq!(p.actions[0], Action::Strip);
        assert_eq!(p.actions[6], Action::FoldTail);
        assert_eq!(p.actions[7], Action::Keep);
        assert_eq!((p.deleted, p.recurred, p.vetoed), (1, 1, 1));
    }

    #[test]
    fn plain_mail_apply_is_strip_tiered_with_its_surrounding_newlines() {
        let a = format!("Story one body line\n\n{FOOT}");
        let b = format!("Different story entirely\n\n{FOOT}");
        let c =
            format!("Third story with words\nmore of the third story\nand a third line\n\n{FOOT}");
        let mut s = set_of(&[&a, &b]);
        learn(&mut s, &c);
        let other = "Other story alpha\nOther story beta\nOther story gamma\nFooter one recurring\nFooter two recurring";
        learn(&mut s, other);
        learn(&mut s, BODY);
        let fence = format!("Body of story two\n```\n{FOOT}\n```\nmore text here\n\n{FOOT}");
        let fixtures = [
            c.clone(),
            BODY.to_owned(),
            fence,
            "Header line one\nHeader line two\nFooter line one".to_owned(),
            String::new(),
        ];
        let label_sets = [
            Labels::default(),
            labels(&["Footer one recurring"], &[]),
            labels(&[], &["Header chrome line"]),
        ];
        for f in &fixtures {
            for (lead, trail) in [("", ""), ("\n\n", "\n"), ("", "\n"), ("\r\n", "\r\n")] {
                let body = format!("{lead}{f}{trail}");
                for l in &label_sets {
                    let want = (!f.is_empty())
                        .then(|| format!("{lead}{}{trail}", strip_tiered(f, &s, l)))
                        .filter(|o| *o != body);
                    let lines: Vec<&str> = body.lines().collect();
                    let (got, p) = apply(&body, &s, l, Recur::Mail).expect("mail never folds");
                    assert_eq!(got, want, "{body:?}");
                    assert_eq!(p, plan(&lines, &[], &s, l, Recur::Mail));
                }
            }
        }
        // Pinned: the stripped body keeps its trailing newline.
        let (got, _) = apply(&format!("{c}\n"), &s, &Labels::default(), Recur::Mail).unwrap();
        assert_eq!(
            got.as_deref(),
            Some("Third story with words\nmore of the third story\nand a third line\n")
        );
    }
}
