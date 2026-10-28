//! Near-duplicate detection for article text: hashed word shingles with containment and Jaccard.
//!
//! Used to decide whether a short stub (an RSS excerpt) is contained in a fuller copy of the same article (a newsletter email, a clip), so the fuller one can supersede it. Pure, sync and I/O-free.
//!
//! Pipeline: [`normalize`] strips markdown/HTML residue, lowercases and keeps only alphanumeric word tokens; [`Shingles`] hashes every run of [`SHINGLE_WORDS`] consecutive tokens (FNV-1a 64, stable across runs and platforms, never `DefaultHasher`).
//!
//! Why k = 4: shorter shingles (1-2 words) match unrelated prose on common phrases and make unrelated articles look similar; longer ones (6+) lose a whole window of shingles per differing word, so small edits (a link title, an inserted "Read more") hurt the score. Four words is long enough that unrelated articles share almost none, and short enough that one differing word costs only 4 shingles.
//!
//! Short texts: a text with fewer than [`SHINGLE_WORDS`] tokens has no shingles, so comparisons involving it fall back to its token set (word-level containment/Jaccard). Empty texts yield 0.0 (never NaN). Scripts without word spaces (CJK) tokenize as one token per unbroken run; no segmentation is attempted.
//!
//! Guidance for callers (measured by this module's tests on synthetic articles, not asserted from theory): a stub taken from the start of the same article scores containment 1.0 against the full text (also with newsletter boilerplate around it); the same article through markdown and HTML conversion scores 1.0 both ways; eight paragraphs with two swapped keep Jaccard 0.90; two unrelated articles score 0.00. Suggested thresholds: stub-in-full >= 0.8, treat <= 0.1 as unrelated; the 0.1-0.8 band is ambiguous and must not auto-merge.

use std::collections::HashSet;

use crate::template::fold_nfkd_ascii;

/// Words per shingle (see the module doc for the choice).
pub const SHINGLE_WORDS: usize = 4;

/// Strip markdown/HTML residue, case-fold, and return the alphanumeric word tokens joined by single spaces.
///
/// Removed: HTML tags and entities, whole image embeds (`![alt](target)` and `![[target]]`, alt text included), markdown link targets (`](...)`), bare URLs (`http(s)://`, `www.`); markdown markers (`#`, `*`, `_`, `>`, `-`, backticks) are non-alphanumeric and so act as separators.
pub fn normalize(text: &str) -> String {
    tokens(text).join(" ")
}

fn tokens(text: &str) -> Vec<String> {
    let cleaned = strip_residue(text);
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in cleaned.chars() {
        let mut any = false;
        for lc in c.to_lowercase() {
            if let Some(f) = fold_nfkd_ascii(lc) {
                cur.push_str(f);
                any = true;
            } else if lc.is_alphanumeric() {
                cur.push(lc);
                any = true;
            }
        }
        if !any && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Replace tags, entities, link targets and bare URLs with a space.
fn strip_residue(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let starts = |i: usize, pat: &str| {
        pat.chars()
            .enumerate()
            .all(|(j, p)| chars.get(i + j).is_some_and(|c| c.eq_ignore_ascii_case(&p)))
    };
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let prev_alnum = i > 0 && chars[i - 1].is_alphanumeric();
        if c == '<'
            && chars
                .get(i + 1)
                .is_some_and(|n| n.is_ascii_alphabetic() || matches!(n, '/' | '!'))
            && let Some(rel) = chars[i..].iter().position(|&x| x == '>')
        {
            out.push(' ');
            i += rel + 1;
        } else if c == '&'
            && let Some(rel) = entity_len(&chars[i..])
        {
            out.push(' ');
            i += rel;
        } else if c == '!'
            && chars.get(i + 1) == Some(&'[')
            && let Some(len) = image_embed_len(&chars[i..])
        {
            // Image embed (`![alt](target)`, `![[target]]`): alt and target contribute nothing,
            // so a clip's verdict is the same before and after localization (plan round-16 WP-5).
            out.push(' ');
            i += len;
        } else if c == ']' && chars.get(i + 1) == Some(&'(') {
            // Link/image target: skip to the closing paren on the same line.
            let end = chars[i + 2..]
                .iter()
                .position(|&x| x == ')' || x == '\n')
                .map_or(chars.len(), |p| i + 2 + p + 1);
            out.push(' ');
            i = end;
        } else if !prev_alnum
            && (starts(i, "http://") || starts(i, "https://") || starts(i, "www."))
        {
            while i < chars.len()
                && !chars[i].is_whitespace()
                && !matches!(chars[i], '<' | '>' | ')' | ']')
            {
                i += 1;
            }
            out.push(' ');
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// Length (in chars) of an image embed at the start of `s` (which starts with `![`), if it is one
/// closed on the same line: `![[target]]`, or `![alt](target)`.
fn image_embed_len(s: &[char]) -> Option<usize> {
    let line = s.iter().position(|&c| c == '\n').unwrap_or(s.len());
    let s = &s[..line];
    if s.get(2) == Some(&'[') {
        let close = s[3..].windows(2).position(|w| w == [']', ']'])?;
        return Some(3 + close + 2);
    }
    let alt_end = 2 + s[2..].iter().position(|&c| c == ']')?;
    if s.get(alt_end + 1) != Some(&'(') {
        return None;
    }
    let close = s[alt_end + 2..].iter().position(|&c| c == ')')?;
    Some(alt_end + 2 + close + 1)
}

/// Length (in chars) of an entity `&name;` / `&#123;` / `&#x1f;` at the start of `s`, if it is one.
fn entity_len(s: &[char]) -> Option<usize> {
    let semi = s.iter().take(12).position(|&c| c == ';')?;
    let body = &s[1..semi];
    let ok = !body.is_empty()
        && (body.iter().all(|c| c.is_ascii_alphanumeric())
            || (body[0] == '#'
                && body.len() > 1
                && body[1..].iter().all(|c| c.is_ascii_alphanumeric())));
    ok.then_some(semi + 1)
}

fn fnv1a(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (n, p) in parts.iter().enumerate() {
        if n > 0 {
            h ^= 0x20;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        for b in p.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// A text reduced to its hashed k-shingle set (plus its token set for the short-text fallback), precomputed for comparing one text against many.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shingles {
    shingles: HashSet<u64>,
    tokens: HashSet<u64>,
    token_count: usize,
}

impl Shingles {
    /// Normalize `text` and hash its shingles.
    pub fn from_text(text: &str) -> Self {
        let toks = tokens(text);
        let refs: Vec<&str> = toks.iter().map(String::as_str).collect();
        let shingles = refs.windows(SHINGLE_WORDS).map(fnv1a).collect();
        let tokens = refs.iter().map(|t| fnv1a(&[t])).collect();
        Self {
            shingles,
            tokens,
            token_count: toks.len(),
        }
    }

    /// Number of distinct shingles, or distinct tokens for a text shorter than [`SHINGLE_WORDS`].
    pub fn len(&self) -> usize {
        self.comparable().len()
    }

    /// The hashed set [`Self::len`] counts (shingles, or tokens for a text shorter than
    /// [`SHINGLE_WORDS`]), in no particular order: for callers that index many texts compactly.
    pub fn hashes(&self) -> impl Iterator<Item = u64> + '_ {
        self.comparable().iter().copied()
    }

    /// True when the text had no word tokens at all.
    pub fn is_empty(&self) -> bool {
        self.token_count == 0
    }

    /// The set used when comparing this text alone (shingles, or tokens when too short).
    fn comparable(&self) -> &HashSet<u64> {
        if self.token_count < SHINGLE_WORDS {
            &self.tokens
        } else {
            &self.shingles
        }
    }

    /// The pair of sets to compare with `other`: shingles if both have them, otherwise token sets.
    fn pair<'a>(&'a self, other: &'a Self) -> (&'a HashSet<u64>, &'a HashSet<u64>) {
        if self.token_count < SHINGLE_WORDS || other.token_count < SHINGLE_WORDS {
            (&self.tokens, &other.tokens)
        } else {
            (&self.shingles, &other.shingles)
        }
    }

    /// |self ∩ large| / |self|, in [0, 1]; 0.0 when `self` is empty.
    pub fn containment_in(&self, large: &Self) -> f64 {
        let (a, b) = self.pair(large);
        if a.is_empty() {
            return 0.0;
        }
        ratio(a.intersection(b).count(), a.len())
    }

    /// |self ∩ other| / |self ∪ other|, in [0, 1]; 0.0 when both are empty.
    pub fn jaccard(&self, other: &Self) -> f64 {
        let (a, b) = self.pair(other);
        let inter = a.intersection(b).count();
        let union = a.len() + b.len() - inter;
        if union == 0 {
            return 0.0;
        }
        ratio(inter, union)
    }
}

#[allow(clippy::cast_precision_loss)]
fn ratio(num: usize, den: usize) -> f64 {
    num as f64 / den as f64
}

/// Fraction of `small`'s shingles that also occur in `large`; 0.0 for an empty `small`.
pub fn containment(small: &str, large: &str) -> f64 {
    Shingles::from_text(small).containment_in(&Shingles::from_text(large))
}

/// Jaccard similarity of the two texts' shingle sets; 0.0 when both are empty.
pub fn jaccard(a: &str, b: &str) -> f64 {
    Shingles::from_text(a).jaccard(&Shingles::from_text(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VOCAB_A: &[&str] = &[
        "harbor",
        "lantern",
        "copper",
        "winter",
        "ferry",
        "salt",
        "granite",
        "orchard",
        "signal",
        "market",
        "glacier",
        "timber",
        "violin",
        "anchor",
        "meadow",
        "kettle",
        "compass",
        "quarry",
        "lighthouse",
        "blanket",
        "river",
        "cobble",
        "furnace",
        "pasture",
        "harvest",
        "tunnel",
        "bridge",
        "candle",
        "village",
        "engine",
        "valley",
        "ledger",
        "paddle",
        "thicket",
        "cellar",
        "mortar",
        "saddle",
        "tavern",
        "spindle",
        "marble",
    ];
    const VOCAB_B: &[&str] = &[
        "quantum",
        "protocol",
        "latency",
        "kernel",
        "compiler",
        "register",
        "bandwidth",
        "cluster",
        "firmware",
        "scheduler",
        "pipeline",
        "gradient",
        "tensor",
        "cipher",
        "payload",
        "socket",
        "mutex",
        "hashing",
        "replica",
        "shard",
        "thread",
        "buffer",
        "packet",
        "router",
        "daemon",
        "syntax",
        "lexer",
        "parser",
        "binary",
        "vector",
        "matrix",
        "cache",
        "stack",
        "heap",
        "branch",
        "commit",
        "rebase",
        "linker",
        "loader",
        "driver",
    ];

    /// Deterministic pseudo-random paragraphs drawn from `vocab`.
    fn paragraphs(vocab: &[&str], seed: u64, count: usize, words: usize) -> Vec<String> {
        let mut s = seed;
        (0..count)
            .map(|_| {
                (0..words)
                    .map(|_| {
                        s = s
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        vocab[usize::try_from(s >> 33).unwrap() % vocab.len()]
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    fn markdown(paras: &[String]) -> String {
        let mut out = String::from("# Title words here\n\n");
        for p in paras {
            let body: Vec<String> = p
                .split(' ')
                .enumerate()
                .map(|(i, w)| match i % 10 {
                    3 => format!("**{w}**"),
                    7 => format!("[{w}](https://example.com/p/{w}?utm=1)"),
                    _ => (*w).to_owned(),
                })
                .collect();
            out.push_str(&format!("> - {}\n\n", body.join(" ")));
        }
        out
    }

    fn html(paras: &[String]) -> String {
        let mut out = String::from("<h1>Title words here</h1>");
        for p in paras {
            let body: Vec<String> = p
                .split(' ')
                .enumerate()
                .map(|(i, w)| match i % 10 {
                    3 => format!("<b>{w}</b>&nbsp;"),
                    7 => format!("<a href=\"https://example.com/p/{w}\">{w}</a> &amp;"),
                    _ => (*w).to_owned(),
                })
                .collect();
            out.push_str(&format!("<p>{}</p>\n", body.join(" ")));
        }
        out
    }

    #[test]
    fn stub_is_contained_in_full() {
        let paras = paragraphs(VOCAB_A, 1, 10, 50);
        let full = paras.join("\n\n");
        let stub: String = full.split(' ').take(100).collect::<Vec<_>>().join(" ");
        let c = containment(&stub, &full);
        println!(
            "stub in full: {c:.3}; full in stub: {:.3}",
            containment(&full, &stub)
        );
        assert!(c >= 0.99);
        assert!(containment(&full, &stub) < 0.3);
    }

    #[test]
    fn unrelated_articles_score_near_zero() {
        let a = paragraphs(VOCAB_A, 2, 8, 50).join("\n\n");
        let b = paragraphs(VOCAB_B, 3, 8, 50).join("\n\n");
        let (c, j) = (containment(&a, &b), jaccard(&a, &b));
        println!("unrelated: containment {c:.3}, jaccard {j:.3}");
        assert!(c <= 0.1 && j <= 0.1);
    }

    #[test]
    fn markdown_and_html_forms_match_both_ways() {
        let paras = paragraphs(VOCAB_A, 4, 8, 50);
        let (md, ht) = (markdown(&paras), html(&paras));
        let (a, b) = (containment(&md, &ht), containment(&ht, &md));
        println!("md in html {a:.3}, html in md {b:.3}");
        assert!(a >= 0.9 && b >= 0.9);
        assert_eq!(
            normalize("[see](https://x.io/a_b) &amp; <i>it</i>"),
            "see it"
        );
    }

    #[test]
    fn stub_survives_newsletter_boilerplate() {
        let paras = paragraphs(VOCAB_A, 5, 10, 50);
        let full = paras.join("\n\n");
        let stub = paras[..2].join("\n\n");
        let mail = format!(
            "View this email in your browser\nThe Weekly Harbor | Issue 42\n\n{full}\n\nYou are receiving this because you subscribed.\nUnsubscribe: https://example.com/u/abc123\n(c) 2026 Example Media, 1 Main St"
        );
        let c = containment(&stub, &mail);
        println!("stub in boilerplated mail: {c:.3}");
        assert!(c >= 0.8);
    }

    #[test]
    fn reordered_paragraphs_keep_jaccard_high() {
        let mut paras = paragraphs(VOCAB_A, 6, 8, 50);
        let before = paras.join("\n\n");
        paras.swap(1, 6);
        paras.swap(2, 4);
        let j = jaccard(&before, &paras.join("\n\n"));
        println!("reordered jaccard: {j:.3}");
        assert!(j >= 0.8);
    }

    #[test]
    fn empty_and_punctuation_inputs_are_total() {
        assert_eq!(containment("", "anything at all here"), 0.0);
        assert_eq!(containment("", ""), 0.0);
        assert_eq!(jaccard("", ""), 0.0);
        assert_eq!(containment("!!! --- ***", "some words in this text"), 0.0);
        assert_eq!(normalize("<unclosed & [x]( https:// "), "unclosed x");
        assert_eq!(Shingles::from_text("日本語の文章 テスト").len(), 2);
        assert!(Shingles::from_text("").is_empty());
    }

    #[test]
    fn short_texts_fall_back_to_token_sets() {
        let large = "the quick brown fox jumps over the lazy dog";
        assert!(Shingles::from_text("brown fox").len() == 2);
        assert!((containment("brown fox", large) - 1.0).abs() < f64::EPSILON);
        assert!((containment("brown cat", large) - 0.5).abs() < f64::EPSILON);
        assert!((jaccard("a b", "a b") - 1.0).abs() < f64::EPSILON);
    }

    /// Plan round-16 WP-5 (4): a remote image with alt text and its localized wiki embed (or
    /// localized markdown target) normalize identically, so localization never moves a verdict.
    #[test]
    fn image_embeds_contribute_nothing() {
        let remote = "Intro words here.\n\n![A chart of quarterly widget sales](https://cdn.example/a.png)\n\nOutro words.";
        let wiki = "Intro words here.\n\n![[attachments/clip/a.png]]\n\nOutro words.";
        let local_md = "Intro words here.\n\n![](attachments/clip/a.png)\n\nOutro words.";
        let want = "intro words here outro words";
        assert_eq!(normalize(remote), want);
        assert_eq!(normalize(wiki), want);
        assert_eq!(normalize(local_md), want);
        // A linked image keeps neither alt nor either target; a plain link keeps its text.
        assert_eq!(
            normalize("[![alt text](i.png)](https://x.example) see [more](u)"),
            "see more"
        );
        // An unclosed embed is ordinary text.
        assert_eq!(normalize("![open alt"), "open alt");
    }

    #[test]
    fn hashes_are_deterministic() {
        let t = paragraphs(VOCAB_B, 7, 3, 40).join("\n\n");
        assert_eq!(Shingles::from_text(&t), Shingles::from_text(&t));
        // Pinned FNV-1a 64 of "a" (published test vector), so the hash can never silently change.
        assert_eq!(fnv1a(&["a"]), 0xaf63_dc4c_8601_ec8c);
    }
}
