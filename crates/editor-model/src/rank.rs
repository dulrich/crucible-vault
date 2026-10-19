//! Recency/derank layer over `nucleo-matcher` for the switcher and palette (WP-4).
//!
//! Score (higher is better), for a non-empty query:
//! `base = max(fuzzy(path), fuzzy(basename) + BASENAME_BONUS) + EXACT_BONUS·[stem == query]`,
//! then `+ recency` (`RECENCY_MAX · (cap − rank) / cap` for a file in the recent list), then a
//! derank `÷ DERANK_DIV` for paths the caller flags (attachment / skipped folders such as
//! `_resources/`). Paths that match neither path nor basename are dropped. Empty query: recent
//! files first (most recent first), then the rest by path. Ties break by path.

use cv_text::vpath::{basename, stem};
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

/// How [`rank`] reads its candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankMode {
    /// Vault paths: basename-weighted scoring, stem-exact bonus (the switcher ranking).
    Path,
    /// Free text (palette titles, picker labels): no path semantics, one fuzzy score over the
    /// whole string, exact bonus when the whole string equals the query.
    Plain,
}

/// One ranked candidate: `index` into the slice given to [`rank`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ranked {
    pub index: usize,
    pub score: i64,
}

/// Ranks `items` against `query`, returning at most `limit` results best-first, as indices
/// into `items`. Ties break by text, then by index (duplicates keep their given order).
pub fn rank<S: AsRef<str>>(
    query: &str,
    items: &[S],
    mode: RankMode,
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
    let text = |i: usize| items[i].as_ref();
    let mut out: Vec<Ranked> = if query.is_empty() {
        (0..items.len())
            .map(|index| {
                let p = text(index);
                let score = recency(p) - i64::from(deranked(p));
                Ranked { index, score }
            })
            .collect()
    } else {
        let config = match mode {
            RankMode::Path => Config::DEFAULT.match_paths(),
            RankMode::Plain => Config::DEFAULT,
        };
        let mut matcher = Matcher::new(config);
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let q_lower = query.to_lowercase();
        let mut buf = Vec::new();
        (0..items.len())
            .filter_map(|index| {
                let p = text(index);
                let full = pattern
                    .score(Utf32Str::new(p, &mut buf), &mut matcher)
                    .map(i64::from);
                let mut score = match mode {
                    RankMode::Path => {
                        let base = pattern
                            .score(Utf32Str::new(basename(p), &mut buf), &mut matcher)
                            .map(|s| i64::from(s) + BASENAME_BONUS);
                        let s = full.max(base)?;
                        let exact = stem(p).to_lowercase() == q_lower
                            || basename(p).to_lowercase() == q_lower;
                        s + if exact { EXACT_BONUS } else { 0 }
                    }
                    RankMode::Plain => {
                        let s = full?;
                        s + if p.trim().to_lowercase() == q_lower {
                            EXACT_BONUS
                        } else {
                            0
                        }
                    }
                };
                score += recency(p);
                if deranked(p) {
                    score /= DERANK_DIV;
                }
                Some(Ranked { index, score })
            })
            .collect()
    };
    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| text(a.index).cmp(text(b.index)))
            .then_with(|| a.index.cmp(&b.index))
    });
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ranked texts (Path mode).
    fn rank(
        q: &str,
        paths: &[String],
        recent: &[String],
        d: impl Fn(&str) -> bool,
        limit: usize,
    ) -> Vec<Hit> {
        super::rank(q, paths, RankMode::Path, recent, d, limit)
            .into_iter()
            .map(|r| Hit {
                path: paths[r.index].clone(),
                score: r.score,
            })
            .collect()
    }

    struct Hit {
        path: String,
        score: i64,
    }

    #[test]
    fn plain_mode_has_no_path_semantics_and_dupes_keep_order() {
        let items = ["Open file", "a/b open", "Open file"];
        let r = super::rank("open file", &items, RankMode::Plain, &[], none, 10);
        assert_eq!(r[0].index, 0);
        assert_eq!(r[1].index, 2, "equal duplicates break by index");
        assert!(r[0].score > EXACT_BONUS);
        let r = super::rank("b", &["x/b"], RankMode::Plain, &[], none, 10);
        assert!(r[0].score < EXACT_BONUS, "no basename exact bonus in Plain");
    }

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
