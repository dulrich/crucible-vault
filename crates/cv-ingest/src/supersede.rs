//! Pure scoring for the intake supersede engine (plan `auto-intake-sprint` WP-3).
//!
//! [`identity_keys`] reduces a URL to the forms two captures of one post share (canonical URL,
//! `open.substack.com/pub/<pub>/p/<slug>` → `https://<pub>.substack.com/p/<slug>`, the blog
//! tracker's post-id). [`decide`] picks what happens when an incoming item meets an existing
//! note. Containment (not symmetric similarity) is the measure: a stub is a prefix of the full
//! text. When a rule is ambiguous the outcome keeps both files ([`Decision::Distinct`]) or the
//! existing one ([`Decision::Keep`]), never a replacement.

use cv_text::similar::Shingles;
use std::collections::HashMap;

use cv_text::url_canon::{
    PostIdOptions, bare_host_name, canonicalize_url, http_host, post_id_from_url,
};

/// What the engine does with an incoming item against one existing note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The existing note stays as it is; the incoming item is a duplicate of it.
    Keep,
    /// The incoming body replaces the existing note's body in place.
    Replace,
    /// Not the same item: both stay.
    Distinct,
}

/// One side of a comparison: word count, known completeness, and the body's shingles.
#[derive(Debug, Clone, Default)]
pub struct Side {
    /// Prose word count of the body.
    pub words: u64,
    /// `complete` (`None` = unknown).
    pub complete: Option<bool>,
    /// The body's shingles.
    pub shingles: Shingles,
}

impl Side {
    /// A side over `body` (shingled here).
    #[must_use]
    pub fn new(words: u64, complete: Option<bool>, body: &str) -> Self {
        Self {
            words,
            complete,
            shingles: Shingles::from_text(body),
        }
    }
}

/// The "clearly larger" factor of rule 3, as a ratio of integers (incoming ≥ 1.2 × existing).
const LARGER_NUM: u64 = 12;
const LARGER_DEN: u64 = 10;

/// Whether containment `c` reaches `threshold_percent`.
fn reaches(c: f64, threshold_percent: u32) -> bool {
    c * 100.0 >= f64::from(threshold_percent.min(100))
}

/// Decides `incoming` against `existing`. `identity` is whether the pair matched by URL / post id
/// (otherwise it is a content-only candidate). Rules, in order:
///
/// 1. existing complete → [`Decision::Keep`];
/// 2. incoming complete, existing known incomplete (`Some(false)`; `None` is unknown, not
///    incomplete), and identity or existing ⊂ incoming → `Replace`;
/// 3. incoming words ≥ 1.2 × existing words and existing ⊂ incoming → `Replace`;
/// 4. incoming ⊂ existing → `Keep`;
/// 5. identity → `Keep`, else [`Decision::Distinct`].
///
/// `⊂` is containment ≥ `threshold_percent`.
#[must_use]
pub fn decide(
    existing: &Side,
    incoming: &Side,
    identity: bool,
    threshold_percent: u32,
) -> Decision {
    if existing.complete == Some(true) {
        return Decision::Keep;
    }
    let e_in_i = reaches(
        existing.shingles.containment_in(&incoming.shingles),
        threshold_percent,
    );
    if existing.complete == Some(false) && incoming.complete == Some(true) && (identity || e_in_i) {
        return Decision::Replace;
    }
    if incoming.words.saturating_mul(LARGER_DEN) >= existing.words.saturating_mul(LARGER_NUM)
        && e_in_i
    {
        return Decision::Replace;
    }
    if reaches(
        incoming.shingles.containment_in(&existing.shingles),
        threshold_percent,
    ) {
        return Decision::Keep;
    }
    if identity {
        Decision::Keep
    } else {
        Decision::Distinct
    }
}

/// Best containment either way (`max(a ⊂ b, b ⊂ a)`), for ranking content candidates.
#[must_use]
pub fn overlap(a: &Shingles, b: &Shingles) -> f64 {
    a.containment_in(b).max(b.containment_in(a))
}

/// Maps `https://open.substack.com/pub/<pub>/p/<slug>…` to `https://<pub>.substack.com/p/<slug>`.
fn open_substack(canonical: &str) -> Option<String> {
    let rest = canonical
        .strip_prefix("https://open.substack.com/pub/")
        .or_else(|| canonical.strip_prefix("http://open.substack.com/pub/"))?;
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let mut parts = rest.split('/');
    let publication = parts.next().filter(|s| !s.is_empty())?;
    if parts.next() != Some("p") {
        return None;
    }
    let slug = parts.next().filter(|s| !s.is_empty())?;
    Some(format!(
        "https://{}.substack.com/p/{slug}",
        publication.to_lowercase()
    ))
}

/// The identity keys of `url`: its canonical form (tracking params stripped, host lowercased,
/// trailing slash trimmed), the `open.substack.com` mapping, and the blog post-id of each.
/// A non-http(s) input yields its trimmed self (a stored `post-id` value). Sorted, deduped.
#[must_use]
pub fn identity_keys(url: &str) -> Vec<String> {
    let raw = url.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let Some(c) = canonicalize_url(raw) else {
        out.push(raw.to_owned());
        return out;
    };
    let mut forms = vec![c.canonical.clone()];
    if let Some(m) = open_substack(&c.canonical) {
        forms.push(m);
    }
    let opts = PostIdOptions::default();
    for f in forms {
        out.push(post_id_from_url(&f, &opts));
        out.push(f);
    }
    out.push(post_id_from_url(raw, &opts));
    out.sort();
    out.dedup();
    out
}

/// The prefix of a publication-scoped identity key. It holds no `://`, so the key can never
/// equal a URL, and no stored `post-id` carries it.
pub const PUBLICATION_KEY_PREFIX: &str = "cv-publication:";

/// Host → publication record `name` (hosts in comparison form, `www.` ignored), built by the
/// caller from its sources config (plan sources-publication-model WP-3). Empty = no
/// publication-scoped keys, exactly the plain [`identity_keys`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublicationHosts {
    by_host: HashMap<String, String>,
}

impl PublicationHosts {
    /// Records `host` (any case, `www.` ignored) as belonging to publication `name`; the first
    /// record to claim a host keeps it.
    pub fn insert(&mut self, host: &str, name: &str) {
        if let Some(h) = bare_host_name(host)
            && !name.trim().is_empty()
        {
            self.by_host
                .entry(h)
                .or_insert_with(|| name.trim().to_owned());
        }
    }

    /// The publication `name` owning `host` (`www.` ignored).
    #[must_use]
    pub fn publication_of(&self, host: &str) -> Option<&str> {
        self.by_host.get(&bare_host_name(host)?).map(String::as_str)
    }

    /// Whether no host is mapped.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_host.is_empty()
    }
}

/// The path of a canonical `scheme://host/path?…` URL, trailing slash trimmed; `None` for the
/// root.
fn canonical_path(canonical: &str) -> Option<&str> {
    let rest = canonical.split_once("://")?.1;
    let path = &rest[rest.find('/')?..];
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let path = path.trim_end_matches('/');
    (!path.is_empty()).then_some(path)
}

/// [`identity_keys`] plus, when the URL's host belongs to a publication in `pubs`, one key
/// `cv-publication:<name>:<path>` shared by every host copy of one post of that publication.
/// The key is derived at match time and never stored.
#[must_use]
pub fn identity_keys_in(url: &str, pubs: &PublicationHosts) -> Vec<String> {
    let mut out = identity_keys(url);
    if pubs.is_empty() {
        return out;
    }
    let Some(c) = canonicalize_url(url.trim()) else {
        return out;
    };
    let mut forms = vec![c.canonical.clone()];
    if let Some(m) = open_substack(&c.canonical) {
        forms.push(m);
    }
    for f in forms {
        if let (Some(h), Some(p)) = (http_host(&f), canonical_path(&f))
            && let Some(name) = pubs.publication_of(&h)
        {
            out.push(format!("{PUBLICATION_KEY_PREFIX}{name}:{p}"));
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "the quick brown fox jumps over the lazy dog while the cat sleeps on the warm mat and the bird sings a long song about the river that runs past the old mill";
    const STUB: &str = "the quick brown fox jumps over the lazy dog while the cat sleeps";
    const OTHER: &str = "markets rallied on tuesday as investors weighed new data about inflation employment and the central bank outlook for next year";

    fn side(text: &str, complete: Option<bool>) -> Side {
        Side::new(text.split_whitespace().count() as u64, complete, text)
    }

    #[test]
    fn identity_keys_match_tracking_and_open_substack_forms() {
        let a = identity_keys("https://Ex.substack.com/p/slug/?utm_source=x");
        let b = identity_keys("https://open.substack.com/pub/ex/p/slug?r=1");
        assert!(a.contains(&"https://ex.substack.com/p/slug".to_owned()));
        assert!(b.contains(&"https://ex.substack.com/p/slug".to_owned()));
        assert!(identity_keys("  ").is_empty());
    }

    #[test]
    fn publication_key_joins_host_copies_and_scopes_by_record() {
        let mut pubs = PublicationHosts::default();
        pubs.insert("www.custom.example", "Alpha");
        pubs.insert("alpha.substack.com", "Alpha");
        pubs.insert("beta.substack.com", "Beta");
        let key = |u: &str| {
            identity_keys_in(u, &pubs)
                .into_iter()
                .filter(|k| k.starts_with(PUBLICATION_KEY_PREFIX))
                .collect::<Vec<_>>()
        };
        let a = key("https://custom.example/p/slug/?utm_source=x");
        assert_eq!(a, vec!["cv-publication:Alpha:/p/slug".to_owned()]);
        assert_eq!(key("https://open.substack.com/pub/alpha/p/slug"), a);
        assert_ne!(key("https://beta.substack.com/p/slug"), a);
        // A host of no publication yields exactly the plain keys.
        let u = "https://other.example/p/slug";
        assert_eq!(identity_keys_in(u, &pubs), identity_keys(u));
        assert_eq!(
            identity_keys_in("post-id-x", &pubs),
            identity_keys("post-id-x")
        );
    }
    #[test]
    fn complete_existing_is_kept() {
        let d = decide(&side(STUB, Some(true)), &side(FULL, Some(true)), true, 80);
        assert_eq!(d, Decision::Keep);
    }

    #[test]
    fn complete_incoming_replaces_incomplete_existing() {
        assert_eq!(
            decide(&side(STUB, Some(false)), &side(FULL, Some(true)), false, 80),
            Decision::Replace
        );
        // Identity alone suffices even when the stub is a different teaser.
        assert_eq!(
            decide(&side(OTHER, Some(false)), &side(FULL, Some(true)), true, 80),
            Decision::Replace
        );
    }

    #[test]
    fn complete_incoming_without_identity_or_containment_is_distinct() {
        assert_eq!(
            decide(&side(OTHER, None), &side(FULL, Some(true)), false, 80),
            Decision::Distinct
        );
    }

    #[test]
    fn clearly_larger_containing_body_replaces() {
        assert_eq!(
            decide(&side(STUB, None), &side(FULL, Some(false)), false, 80),
            Decision::Replace
        );
    }

    #[test]
    fn contained_incoming_is_kept() {
        assert_eq!(
            decide(
                &side(FULL, Some(false)),
                &side(STUB, Some(false)),
                false,
                80
            ),
            Decision::Keep
        );
    }

    #[test]
    fn unknown_existing_completeness_is_not_replaced_by_a_complete_identity_match() {
        // `None` is unknown, not incomplete: rule 2 does not apply, and a body that is not
        // clearly larger (or does not contain the existing text) keeps the note.
        assert_eq!(
            decide(&side(FULL, None), &side(FULL, Some(true)), true, 80),
            Decision::Keep
        );
        assert_eq!(
            decide(&side(OTHER, None), &side(FULL, Some(true)), true, 80),
            Decision::Keep
        );
    }

    #[test]
    fn unrelated_identity_keeps_and_content_only_is_distinct() {
        let (e, i) = (side(OTHER, None), side(FULL, None));
        assert_eq!(decide(&e, &i, true, 80), Decision::Keep);
        assert_eq!(decide(&e, &i, false, 80), Decision::Distinct);
    }
}
