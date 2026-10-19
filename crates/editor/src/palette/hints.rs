//! Palette hints: the thin `Entry` adapter over [`editor_model::hints`] (WP-R18 moved the pure
//! minimum-subsequence search there).

pub use editor_model::hints::MAX_LEN;

use super::{Entry, haystacks};

/// Each entry's hint (see [`editor_model::hints::compute`]).
pub fn compute(entries: &[Entry]) -> Vec<Option<String>> {
    let titles: Vec<&str> = entries.iter().map(|e| e.title.as_str()).collect();
    editor_model::hints::compute(&titles, &haystacks(entries))
}
