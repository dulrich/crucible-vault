//! Access tier and completeness policy for tracked blog posts (plan `auto-intake-sprint` WP-2).
//!
//! The Substack post probe (`/api/v1/posts/<slug>`, parsed in `cv`) reports an `audience` and a
//! full `wordcount`; [`completeness`] decides whether the feed-delivered body is the whole post.
//! Pure: no I/O, no JSON.

/// A post's access tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Readable by everyone.
    Free,
    /// Paid subscribers only.
    Paid,
    /// Founding members only.
    Founding,
    /// Probe failed or reported an unrecognised audience.
    Unknown,
}

impl Access {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Free => "free",
            Access::Paid => "paid",
            Access::Founding => "founding",
            Access::Unknown => "unknown",
        }
    }

    /// Inverse of [`Access::as_str`].
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "free" => Some(Access::Free),
            "paid" => Some(Access::Paid),
            "founding" => Some(Access::Founding),
            "unknown" => Some(Access::Unknown),
            _ => None,
        }
    }

    /// Maps a Substack `audience` (`everyone`, `only_paid`, `founding`; anything else unknown).
    #[must_use]
    pub fn from_substack_audience(audience: &str) -> Self {
        match audience {
            "everyone" => Access::Free,
            "only_paid" => Access::Paid,
            "founding" => Access::Founding,
            _ => Access::Unknown,
        }
    }
}

/// Whether the delivered body is the complete post (`None` = unknown, the item stays open).
///
/// In order: a paywall marker hit → incomplete; no body → incomplete; a known full word count →
/// `delivered * 100 >= full * complete_percent`; paid/founding → incomplete; a failed probe
/// (`Unknown`) → unknown; free or not probed (`None`) → complete.
#[must_use]
pub fn completeness(
    access: Option<Access>,
    delivered_words: u32,
    full_words: Option<u32>,
    has_body: bool,
    marker_hit: bool,
    complete_percent: u32,
) -> Option<bool> {
    if marker_hit || !has_body {
        return Some(false);
    }
    if let Some(n) = full_words.filter(|n| *n > 0) {
        return Some(
            u64::from(delivered_words) * 100 >= u64::from(n) * u64::from(complete_percent),
        );
    }
    match access {
        Some(Access::Paid | Access::Founding) => Some(false),
        Some(Access::Unknown) => None,
        Some(Access::Free) | None => Some(true),
    }
}

/// Whether `body` contains any of `markers` (case-insensitive substring; empty markers skip).
#[must_use]
pub fn marker_hit(body: &str, markers: &[String]) -> bool {
    let hay = body.to_lowercase();
    markers
        .iter()
        .filter(|m| !m.trim().is_empty())
        .any(|m| hay.contains(&m.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_round_trips_and_maps_audience() {
        for a in [
            Access::Free,
            Access::Paid,
            Access::Founding,
            Access::Unknown,
        ] {
            assert_eq!(Access::parse(a.as_str()), Some(a));
        }
        assert_eq!(Access::from_substack_audience("everyone"), Access::Free);
        assert_eq!(Access::from_substack_audience("only_paid"), Access::Paid);
        assert_eq!(Access::from_substack_audience("founding"), Access::Founding);
        assert_eq!(Access::from_substack_audience("only_free"), Access::Unknown);
    }

    #[test]
    fn marker_hit_wins_over_everything() {
        assert_eq!(
            completeness(Some(Access::Free), 100, Some(100), true, true, 80),
            Some(false)
        );
    }

    #[test]
    fn no_body_is_incomplete() {
        assert_eq!(completeness(None, 0, None, false, false, 80), Some(false));
    }

    #[test]
    fn full_word_count_ratio_decides() {
        assert_eq!(
            completeness(Some(Access::Paid), 80, Some(100), true, false, 80),
            Some(true)
        );
        assert_eq!(
            completeness(Some(Access::Free), 79, Some(100), true, false, 80),
            Some(false)
        );
    }

    #[test]
    fn paid_and_founding_without_count_are_incomplete() {
        assert_eq!(
            completeness(Some(Access::Paid), 10, None, true, false, 80),
            Some(false)
        );
        assert_eq!(
            completeness(Some(Access::Founding), 10, Some(0), true, false, 80),
            Some(false)
        );
    }

    #[test]
    fn failed_probe_is_unknown() {
        assert_eq!(
            completeness(Some(Access::Unknown), 10, None, true, false, 80),
            None
        );
    }

    #[test]
    fn free_or_unprobed_with_body_is_complete() {
        assert_eq!(
            completeness(Some(Access::Free), 10, None, true, false, 80),
            Some(true)
        );
        assert_eq!(completeness(None, 10, None, true, false, 80), Some(true));
    }

    #[test]
    fn markers_match_case_insensitively() {
        let m = vec!["Subscribe to Read".to_owned(), String::new()];
        assert!(marker_hit("…please SUBSCRIBE TO READ more", &m));
        assert!(!marker_hit("free text", &m));
        assert!(!marker_hit("anything", &[]));
    }
}
