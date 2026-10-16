//! Palette minimum-subsequence hints (plan editor-ui-round-3 WP-8).
//!
//! For each visible entry: the shortest lower-cased subsequence of its title (length ≤
//! [`MAX_LEN`]) that makes the palette's own ranking ([`super::rank_entries`] — `rank` over the
//! same `"title group"` haystacks) put that entry **strictly first**: its score is greater than
//! every other entry's score. An equal top score is a tie and does not qualify, even though
//! `rank`'s path tie-break would order the two (Crucible's `s >= own` rejection,
//! `commandPaletteHints.ts` ~190-205). Entries with no hint within the bound get none.
//!
//! Ported from Crucible `https://github.com/dulrich/obsidian-crucible/blob/master/src/commandPaletteHints.ts`:
//! - kept: `maxLen` 6 (~31-36); the tie weights of `charWeights` (~53-85) with the default
//!   knobs (`prefixPenalty` 1 per `": "` segment away from the leaf, `positionBias` 0); the
//!   iterative-deepening search over increasing index combinations, shortest first, minimum
//!   weight (first found on equal weight) within a length (~98-129); the 20 000 node budget
//!   per entry (~105); the "top match" acceptance (~182-205) against our ranking (audit N2:
//!   our ranking is fuzzy, so candidates are subsequences, not substrings).
//! - added: candidates draw only alphanumeric characters (Crucible's `allowedChar` knob, ~23):
//!   whitespace would split the nucleo pattern into atoms and `^ $ ! '` are pattern syntax;
//!   a rendered candidate is ranked once per length (its verdict is cached); a cheap subsequence prefilter
//!   scores only the entries whose haystack can match the candidate (a nucleo match requires a
//!   case-insensitive subsequence).
//! - dropped (Obsidian-induced): patching Obsidian's palette DOM to show the hint
//!   (`commandPalette.ts` ~93-107 — we render it in our own row), and the hint-charset /
//!   weight tuning settings (`settings/sections/commands.ts` ~324-370; D20 drops "hint
//!   tuning").
//!
//! Computed once per entry-set change ([`super::Palette`]'s `rebuild`) and cached — never per
//! keystroke or per frame.

use editor_model::rank::rank;

use super::{Entry, haystacks};

/// Longest hint (Crucible `maxLen`).
pub const MAX_LEN: usize = 6;
/// Node visits per entry (Crucible's global guard).
const BUDGET: usize = 20_000;

fn is_subsequence(q: &[char], hay: &[char]) -> bool {
    let mut it = hay.iter();
    q.iter().all(|c| it.any(|h| h == c))
}

/// Tie weight per char of the lower-cased title: its `": "` segment depth from the leaf.
fn weights(t: &[char]) -> Vec<u32> {
    let s: String = t.iter().collect();
    let segs: Vec<&str> = s.split(": ").collect();
    let last = segs.len() - 1;
    let mut w = Vec::with_capacity(t.len());
    for (i, seg) in segs.iter().enumerate() {
        let depth = u32::try_from(last - i).unwrap_or(u32::MAX);
        let n = seg.chars().count() + if i < last { 2 } else { 0 };
        w.extend(std::iter::repeat_n(depth, n));
    }
    w.resize(t.len(), 0);
    w
}

/// Hints for `entries`, parallel to it.
pub fn compute(entries: &[Entry]) -> Vec<Option<String>> {
    let hays = haystacks(entries);
    let lower: Vec<Vec<char>> = hays
        .iter()
        .map(|h| h.to_lowercase().chars().collect())
        .collect();
    (0..entries.len())
        .map(|i| hint_for(i, entries, &hays, &lower))
        .collect()
}

/// Whether `q` ranks entry `target` strictly first.
fn strictly_first(q: &[char], target: usize, hays: &[String], lower: &[Vec<char>]) -> bool {
    let cands: Vec<String> = lower
        .iter()
        .enumerate()
        .filter(|(_, h)| is_subsequence(q, h))
        .map(|(j, _)| hays[j].clone())
        .collect();
    let query: String = q.iter().collect();
    let r = rank(&query, &cands, &[], |_| false, 2);
    match r.as_slice() {
        [a] => a.path == hays[target],
        [a, b] => a.path == hays[target] && a.score > b.score,
        _ => false,
    }
}

fn hint_for(
    target: usize,
    entries: &[Entry],
    hays: &[String],
    lower: &[Vec<char>],
) -> Option<String> {
    let t: Vec<char> = entries[target].title.to_lowercase().chars().collect();
    let w = weights(&t);
    let allowed: Vec<usize> = (0..t.len()).filter(|&j| t[j].is_alphanumeric()).collect();
    let mut budget = BUDGET;
    for bound in 1..=MAX_LEN.min(allowed.len()) {
        let mut seen = std::collections::HashMap::new();
        let mut best: Option<(u32, Vec<char>)> = None;
        let mut chosen = Vec::with_capacity(bound);
        dfs(
            &mut Search {
                t: &t,
                w: &w,
                allowed: &allowed,
                bound,
                budget: &mut budget,
                seen: &mut seen,
                best: &mut best,
                accept: &|q: &[char]| strictly_first(q, target, hays, lower),
            },
            0,
            &mut chosen,
        );
        if let Some((_, q)) = best {
            return Some(q.into_iter().collect());
        }
        if budget == 0 {
            return None;
        }
    }
    None
}

struct Search<'a> {
    t: &'a [char],
    w: &'a [u32],
    allowed: &'a [usize],
    bound: usize,
    budget: &'a mut usize,
    seen: &'a mut std::collections::HashMap<Vec<char>, bool>,
    best: &'a mut Option<(u32, Vec<char>)>,
    accept: &'a dyn Fn(&[char]) -> bool,
}

fn dfs(s: &mut Search<'_>, start: usize, chosen: &mut Vec<usize>) {
    if chosen.len() == s.bound {
        let weight: u32 = chosen.iter().map(|&j| s.w[j]).sum();
        if s.best.as_ref().is_some_and(|(bw, _)| weight >= *bw) {
            return;
        }
        let q: Vec<char> = chosen.iter().map(|&j| s.t[j]).collect();
        let ok = match s.seen.get(&q) {
            Some(&ok) => ok,
            None => {
                let ok = (s.accept)(&q);
                s.seen.insert(q.clone(), ok);
                ok
            }
        };
        if ok {
            *s.best = Some((weight, q));
        }
        return;
    }
    for k in start..s.allowed.len() {
        if *s.budget == 0 {
            return;
        }
        *s.budget -= 1;
        chosen.push(s.allowed[k]);
        dfs(s, k + 1, chosen);
        chosen.pop();
    }
}
