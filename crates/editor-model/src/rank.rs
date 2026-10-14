//! Recency/derank layer over `nucleo-matcher` for the switcher and palette (WP-4).
//!
//! Score (higher is better), for a non-empty query:
//! `base = max(fuzzy(path), fuzzy(basename) + BASENAME_BONUS) + EXACT_BONUS·[stem == query]`,
//! then `+ recency` (`RECENCY_MAX · (cap − rank) / cap` for a file in the recent list), then a
//! derank `÷ DERANK_DIV` for paths the caller flags (attachment / skipped folders such as
//! `_resources/`). Paths that match neither path nor basename are dropped. Empty query: recent
//! files first (most recent first), then the rest by path. Ties break by path.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Bonus for a basename match over a path-only match.
pub const BASENAME_BONUS: i64 = 40;
/// Bonus when the basename stem equals the query (case-insensitive).
pub const EXACT_BONUS: i64 = 10_000;
/// Largest recency bonus (most recently opened file).
pub const RECENCY_MAX: i64 = 30;
/// Divisor applied to deranked paths.
pub const DERANK_DIV: i64 = 4;

/// One ranked candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    pub path: String,
    pub score: i64,
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn stem(path: &str) -> &str {
    let b = basename(path);
    b.strip_suffix(".md").unwrap_or(b)
}

/// Ranks `paths` against `query`, returning at most `limit` results best-first.
pub fn rank(
    query: &str,
    paths: &[String],
    recent: &[String],
    deranked: impl Fn(&str) -> bool,
    limit: usize,
) -> Vec<Ranked> {
    let query = query.trim();
    let cap = i64::try_from(recent.len().max(1)).unwrap_or(i64::MAX);
    let recency = |p: &str| -> i64 {
        recent
            .iter()
            .position(|r| r == p)
            .and_then(|i| i64::try_from(i).ok())
            .map_or(0, |i| RECENCY_MAX * (cap - i) / cap)
    };
    let mut out: Vec<Ranked> = if query.is_empty() {
        paths
            .iter()
            .map(|p| Ranked {
                path: p.clone(),
                score: recency(p),
            })
            .collect()
    } else {
        let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let q_lower = query.to_lowercase();
        let mut buf = Vec::new();
        paths
            .iter()
            .filter_map(|p| {
                let full = pattern
                    .score(Utf32Str::new(p, &mut buf), &mut matcher)
                    .map(i64::from);
                let base = pattern
                    .score(Utf32Str::new(basename(p), &mut buf), &mut matcher)
                    .map(|s| i64::from(s) + BASENAME_BONUS);
                let mut score = full.max(base)?;
                if stem(p).to_lowercase() == q_lower || basename(p).to_lowercase() == q_lower {
                    score += EXACT_BONUS;
                }
                score += recency(p);
                if deranked(p) {
                    score /= DERANK_DIV;
                }
                Some(Ranked {
                    path: p.clone(),
                    score,
                })
            })
            .collect()
    };
    if query.is_empty() {
        for r in &mut out {
            if deranked(&r.path) {
                r.score -= 1;
            }
        }
    }
    out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    fn none(_: &str) -> bool {
        false
    }

    fn res(p: &str) -> bool {
        p.starts_with("_resources/")
    }

    #[test]
    fn exact_basename_beats_fuzzy_path() {
        let paths = s(&["projects/works/plan.md", "core/Works.md", "w/o/r/k/s.md"]);
        let r = rank("works", &paths, &[], none, 50);
        assert_eq!(r[0].path, "core/Works.md");
        assert!(r.iter().any(|x| x.path == "projects/works/plan.md"));
    }

    #[test]
    fn recency_breaks_ties() {
        let paths = s(&["a/note.md", "b/note.md"]);
        let r = rank("note", &paths, &s(&["b/note.md"]), none, 50);
        assert_eq!(r[0].path, "b/note.md");
        let r = rank("note", &paths, &[], none, 50);
        assert_eq!(r[0].path, "a/note.md", "path order without recency");
        let r = rank("", &paths, &s(&["b/note.md"]), none, 50);
        assert_eq!(r[0].path, "b/note.md");
    }

    #[test]
    fn resources_deranked() {
        let paths = s(&["_resources/x/photo.md", "z/photo.md"]);
        let r = rank("photo", &paths, &[], res, 50);
        assert_eq!(r[0].path, "z/photo.md");
        assert!(r[0].score > r[1].score);
        let r = rank("", &paths, &[], res, 50);
        assert_eq!(r[0].path, "z/photo.md");
    }

    #[test]
    fn misses_dropped_and_limit_applies() {
        let paths: Vec<String> = (0..100).map(|i| format!("n/{i}.md")).collect();
        assert!(rank("zzzq", &paths, &[], none, 50).is_empty());
        assert_eq!(rank("", &paths, &[], none, 50).len(), 50);
    }
}
