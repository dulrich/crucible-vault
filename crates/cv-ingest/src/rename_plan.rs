//! The rename-chain planner (plan `tn-review-2026-11-10-remediation` WP-R8, review S3; chain-safe
//! backfill renames from `round-23` WP-8). Pure: callers rename.

/// Why a planned backfill rename is withheld.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Withheld {
    /// Two or more notes compute the same target.
    Duplicate,
    /// The target exists and its holder is not moving.
    Exists,
    /// The renames form a cycle (each target held by the next mover).
    Cycle,
}

impl Withheld {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Duplicate => "duplicate target",
            Self::Exists => "exists",
            Self::Cycle => "cycle",
        }
    }
}

/// A withheld rename: `(src, dst, why)`.
pub type WithheldRename = (String, String, Withheld);

/// Chain-safe ordering of planned renames `(src, dst)` (pure; `exists` = a path is taken now):
/// returns the renames to run, holders first (a target held by a note that itself moves is
/// freed before it is claimed), and the withheld ones with their reason.
///
/// Every src moves to one dst and (after duplicate removal) every dst has one claimer, so
/// "dst is held by mover m" is a functional graph: cycles are withheld, then any rename whose
/// target is taken by a non-mover (fixpoint: a withheld mover holds its path) is withheld, and
/// the rest are emitted depth-first, the holder before its claimer.
#[must_use]
pub fn order_renames(
    renames: &[(String, String)],
    exists: &dyn Fn(&str) -> bool,
) -> (Vec<(String, String)>, Vec<WithheldRename>) {
    use std::collections::{BTreeMap, HashMap};
    let mut claims: HashMap<&str, usize> = HashMap::new();
    for (_, d) in renames {
        *claims.entry(d.as_str()).or_default() += 1;
    }
    let mut withheld: BTreeMap<usize, Withheld> = BTreeMap::new();
    for (i, (_, d)) in renames.iter().enumerate() {
        if claims[d.as_str()] > 1 {
            withheld.insert(i, Withheld::Duplicate);
        }
    }
    let by_src: HashMap<&str, usize> = renames
        .iter()
        .enumerate()
        .map(|(i, (s, _))| (s.as_str(), i))
        .collect();
    // The mover holding rename i's target, if any (and not withheld).
    let next = |i: usize, w: &BTreeMap<usize, Withheld>| {
        by_src
            .get(renames[i].1.as_str())
            .copied()
            .filter(|j| !w.contains_key(j))
    };
    // Cycles among the live renames.
    for start in 0..renames.len() {
        if withheld.contains_key(&start) {
            continue;
        }
        let mut seen = vec![start];
        let mut cur = start;
        while let Some(j) = next(cur, &withheld) {
            if let Some(pos) = seen.iter().position(|&s| s == j) {
                if pos == 0 {
                    for &m in &seen {
                        withheld.insert(m, Withheld::Cycle);
                    }
                }
                break;
            }
            seen.push(j);
            cur = j;
        }
    }
    // Targets taken by a note that does not move.
    loop {
        let mut changed = false;
        for (i, (_, dst)) in renames.iter().enumerate() {
            if !withheld.contains_key(&i) && exists(dst) && next(i, &withheld).is_none() {
                withheld.insert(i, Withheld::Exists);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut done = vec![false; renames.len()];
    let mut order = Vec::new();
    for start in 0..renames.len() {
        let mut stack = Vec::new();
        let mut cur = Some(start);
        while let Some(i) = cur.filter(|i| !done[*i] && !withheld.contains_key(i)) {
            stack.push(i);
            done[i] = true;
            cur = next(i, &withheld);
        }
        while let Some(i) = stack.pop() {
            order.push(renames[i].clone());
        }
    }
    let withheld = withheld
        .into_iter()
        .map(|(i, w)| (renames[i].0.clone(), renames[i].1.clone(), w))
        .collect();
    (order, withheld)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_renames_withholds_a_chain_behind_a_withheld_holder() {
        let r = |s: &str, d: &str| (s.to_owned(), d.to_owned());
        // c -> b is held by b, whose own target a is held by a non-mover.
        let renames = [r("b", "a"), r("c", "b")];
        let (order, held) = order_renames(&renames, &|p| p == "a" || p == "b");
        assert!(order.is_empty());
        assert_eq!(held.len(), 2);
        assert!(held.iter().all(|(_, _, w)| *w == Withheld::Exists));
        let (order, held) = order_renames(&renames, &|p| p == "b");
        assert_eq!(order, vec![r("b", "a"), r("c", "b")]);
        assert!(held.is_empty());
    }
}
