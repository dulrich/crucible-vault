//! Local-vs-companion comparison — pure, so the shadow feed and WP-14's `cv shadow parity` (P3)
//! share one definition of "this path is up to date".
//!
//! Crucible's skip rule (`SearchManager.indexFiles` + `embeddingCoverageSatisfied`,
//! `src/search/SearchManager.ts:404-416,621-624`): unchanged iff the stored `contentHash`
//! equals the prepared one **and**, when semantic indexing is on, every chunk is embedded in the
//! active space. Audit item E3 adds the run-level half: `/health`'s `embeddingModel` /
//! `embeddingDim` must equal the configured embedder's ([`EmbedCheck::health_matches`]).

use std::collections::{BTreeMap, BTreeSet};

/// The companion's view of one path (from `/v1/paths`, refined by `/v1/files/state`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteEntry {
    /// Path.
    pub path: String,
    /// Stored content hash.
    pub content_hash: Option<String>,
    /// Every chunk embedded.
    pub fully_embedded: bool,
    /// The path's single embedding space, when the companion reports one.
    pub embedding_space: Option<String>,
}

/// The embedding half of the up-to-date rule (absent = lexical-only: hashes decide alone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedCheck {
    /// The configured model id (= the space id; no precision is known, `embeddingSpaceId`).
    pub model: String,
    /// `/health` reported this model (and, live, the probed dimension).
    pub health_matches: bool,
}

/// Per-path classification.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompareReport {
    /// Hash equal (and embeddings current when checked).
    pub matched: Vec<String>,
    /// Hash differs.
    pub mismatched: Vec<String>,
    /// Local path the companion does not hold.
    pub missing_remote: Vec<String>,
    /// Companion path the vault no longer has.
    pub missing_local: Vec<String>,
    /// Hash equal but vectors missing, partial, in another space, or the run-level model/dim
    /// check failed (E3).
    pub embedding_mismatch: Vec<String>,
}

/// Aggregate counts of a [`CompareReport`] — what shadow mode may report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompareCounts {
    /// See [`CompareReport::matched`].
    pub matched: usize,
    /// See [`CompareReport::mismatched`].
    pub mismatched: usize,
    /// See [`CompareReport::missing_remote`].
    pub missing_remote: usize,
    /// See [`CompareReport::missing_local`].
    pub missing_local: usize,
    /// See [`CompareReport::embedding_mismatch`].
    pub embedding_mismatch: usize,
}

impl CompareReport {
    /// The counts.
    #[must_use]
    pub fn counts(&self) -> CompareCounts {
        CompareCounts {
            matched: self.matched.len(),
            mismatched: self.mismatched.len(),
            missing_remote: self.missing_remote.len(),
            missing_local: self.missing_local.len(),
            embedding_mismatch: self.embedding_mismatch.len(),
        }
    }

    /// Paths a live run must (re-)upsert.
    #[must_use]
    pub fn to_upsert(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .mismatched
            .iter()
            .chain(&self.missing_remote)
            .chain(&self.embedding_mismatch)
            .cloned()
            .collect();
        v.sort();
        v
    }
}

impl CompareCounts {
    /// Adds `other` into `self`.
    pub fn add(&mut self, other: CompareCounts) {
        self.matched += other.matched;
        self.mismatched += other.mismatched;
        self.missing_remote += other.missing_remote;
        self.missing_local += other.missing_local;
        self.embedding_mismatch += other.embedding_mismatch;
    }
}

/// Classifies `local` `(path, contentHash)` pairs against `remote`. With `full_remote` the
/// remote list is the vault's whole index (`/v1/paths`), so remote-only paths are
/// `missing_local`; otherwise (a single-path upsert) remote-only paths are ignored.
#[must_use]
pub fn compare(
    local: &[(String, String)],
    remote: &[RemoteEntry],
    embed: Option<&EmbedCheck>,
    full_remote: bool,
) -> CompareReport {
    let remote_by: BTreeMap<&str, &RemoteEntry> =
        remote.iter().map(|r| (r.path.as_str(), r)).collect();
    let local_paths: BTreeSet<&str> = local.iter().map(|(p, _)| p.as_str()).collect();
    let mut report = CompareReport::default();
    let mut sorted: Vec<&(String, String)> = local.iter().collect();
    sorted.sort();
    sorted.dedup_by(|a, b| a.0 == b.0);
    for (path, hash) in sorted {
        let Some(r) = remote_by.get(path.as_str()) else {
            report.missing_remote.push(path.clone());
            continue;
        };
        if r.content_hash.as_deref() != Some(hash.as_str()) {
            report.mismatched.push(path.clone());
            continue;
        }
        let embedded_ok = embed.is_none_or(|e| {
            e.health_matches
                && r.fully_embedded
                && r.embedding_space.as_deref().is_none_or(|s| s == e.model)
        });
        if embedded_ok {
            report.matched.push(path.clone());
        } else {
            report.embedding_mismatch.push(path.clone());
        }
    }
    if full_remote {
        report.missing_local = remote_by
            .keys()
            .filter(|p| !local_paths.contains(*p))
            .map(|p| (*p).to_owned())
            .collect();
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(path: &str, hash: &str, embedded: bool, space: Option<&str>) -> RemoteEntry {
        RemoteEntry {
            path: path.into(),
            content_hash: Some(hash.into()),
            fully_embedded: embedded,
            embedding_space: space.map(Into::into),
        }
    }

    #[test]
    fn classifies_every_case() {
        let local = vec![
            ("a.md".to_owned(), "h1".to_owned()),
            ("b.md".to_owned(), "h2".to_owned()),
            ("c.md".to_owned(), "h3".to_owned()),
            ("d.md".to_owned(), "h4".to_owned()),
        ];
        let remote = vec![
            r("a.md", "h1", true, Some("m")),
            r("b.md", "XX", true, Some("m")),
            r("d.md", "h4", true, Some("other")),
            r("gone.md", "h9", true, None),
        ];
        let e = EmbedCheck {
            model: "m".into(),
            health_matches: true,
        };
        let rep = compare(&local, &remote, Some(&e), true);
        assert_eq!(rep.matched, ["a.md"]);
        assert_eq!(rep.mismatched, ["b.md"]);
        assert_eq!(rep.missing_remote, ["c.md"]);
        assert_eq!(rep.embedding_mismatch, ["d.md"]);
        assert_eq!(rep.missing_local, ["gone.md"]);
        assert_eq!(rep.to_upsert(), ["b.md", "c.md", "d.md"]);
        // Lexical-only: hashes decide alone.
        let lex = compare(&local, &remote, None, false);
        assert_eq!(lex.matched, ["a.md", "d.md"]);
        assert!(lex.missing_local.is_empty());
        // Run-level model mismatch (E3) demotes every hash match.
        let bad = EmbedCheck {
            model: "m".into(),
            health_matches: false,
        };
        assert_eq!(
            compare(&local, &remote, Some(&bad), true)
                .counts()
                .embedding_mismatch,
            2
        );
    }
}
