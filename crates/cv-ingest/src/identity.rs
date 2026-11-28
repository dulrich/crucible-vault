//! Match keys (plan `intake-foundations` WP-2): the one definition of the keys an item is
//! *matched* by — a note (its frontmatter facts), a tracker row, a feed item, a mail. Every
//! matching site (the supersede engine and its finalize fallback, `intake_finalize`, reconcile
//! phases A and E, `dupes-scan`) derives both sides here and compares by key intersection
//! ([`intersects`]); no site keeps its own URL normalisation.
//!
//! Key forms (all derived at match time, never stored; durable ids — lint's `post-id`, the blog
//! tracker's per-feed id and GUID preference, stored tracker `item_id`s — are not touched):
//!
//! - **canonical URL** (`cv_text::canonicalize_url`: tracking params stripped, host lowercased,
//!   trailing slash trimmed, YouTube → `www.youtube.com/watch?v=`, arXiv pdf → abs, X → `x.com`);
//! - **aliases**: the `open.substack.com/pub/<pub>/p/<slug>` → `<pub>.substack.com/p/<slug>`
//!   mapping and the blog post-id of each form (default options, plus the caller's configured
//!   `PostIdOptions` when given); a non-http(s) value (a stored `post-id`) is its trimmed self;
//! - **video id**: a YouTube row's `item_id` keys as its canonical watch URL;
//! - **message id**: `cv-message:<id>` ([`MESSAGE_KEY_PREFIX`], angle brackets trimmed);
//! - **publication-scoped path**: `cv-publication:<name>:<path>` when the host belongs to a
//!   publication in [`PublicationHosts`] (the only way publication hosts reach the keys).

use std::collections::{BTreeSet, HashMap};

use cv_text::url_canon::{
    PostIdOptions, bare_host_name, canonicalize_url, http_host, post_id_from_url,
};

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

/// A set of match keys. Two items match when their sets intersect ([`intersects`]).
pub type MatchKeys = BTreeSet<String>;

/// The prefix of a message-id key (no `://`, so it never equals a URL or a stored `post-id`).
pub const MESSAGE_KEY_PREFIX: &str = "cv-message:";

/// The canonical watch URL of YouTube video `id` (a YouTube row's video-id key).
#[must_use]
pub fn video_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={}", id.trim())
}

/// The keys of one value (a URL or a stored id): [`identity_keys_in`], plus the post-id under
/// `opts` (the caller's configured blog `canon` rules) when given.
#[must_use]
pub fn value_keys(value: &str, pubs: &PublicationHosts, opts: Option<&PostIdOptions>) -> MatchKeys {
    let mut out: MatchKeys = identity_keys_in(value, pubs).into_iter().collect();
    if let Some(o) = opts
        && !value.trim().is_empty()
    {
        out.insert(post_id_from_url(value.trim(), o));
    }
    out.retain(|k| !k.is_empty());
    out
}

fn message_key(id: &str) -> Option<String> {
    let id = id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    (!id.is_empty()).then(|| format!("{MESSAGE_KEY_PREFIX}{id}"))
}

/// A note's matching facts, read from its frontmatter by the caller.
#[derive(Debug, Clone, Default)]
pub struct NoteFacts<'a> {
    /// `source` values.
    pub sources: &'a [String],
    /// `post-id` values.
    pub post_ids: &'a [String],
    /// `message-id` values.
    pub message_ids: &'a [String],
}

/// A note's keys: every `source` and `post-id` value's [`value_keys`], and its message ids.
#[must_use]
pub fn note_keys(
    facts: &NoteFacts<'_>,
    pubs: &PublicationHosts,
    opts: Option<&PostIdOptions>,
) -> MatchKeys {
    let mut out = MatchKeys::new();
    for v in facts.sources.iter().chain(facts.post_ids) {
        out.extend(value_keys(v, pubs, opts));
    }
    out.extend(facts.message_ids.iter().filter_map(|m| message_key(m)));
    out
}

/// A tracker row's keys: its `url`'s [`value_keys`]; a `youtube` row adds its video id
/// (`item_id`, as [`video_url`]). The stored `item_id` itself is never reinterpreted otherwise.
#[must_use]
pub fn row_keys(kind: &str, url: &str, item_id: &str, pubs: &PublicationHosts) -> MatchKeys {
    let mut out = value_keys(url, pubs, None);
    if kind == "youtube" && !item_id.trim().is_empty() {
        out.extend(value_keys(&video_url(item_id), pubs, None));
    }
    out
}

/// A feed item's keys: its link's [`value_keys`], plus its GUID's when the GUID is URL-shaped (a
/// plain GUID is a per-feed id, not a match key).
#[must_use]
pub fn feed_item_keys(link: &str, guid: Option<&str>, pubs: &PublicationHosts) -> MatchKeys {
    let mut out = value_keys(link, pubs, None);
    if let Some(g) = guid.filter(|g| canonicalize_url(g).is_some()) {
        out.extend(value_keys(g, pubs, None));
    }
    out
}

/// A mail's keys: its Message-ID and its canonical post URL's [`value_keys`].
#[must_use]
pub fn mail_keys(
    message_id: Option<&str>,
    post_url: Option<&str>,
    pubs: &PublicationHosts,
) -> MatchKeys {
    let mut out = MatchKeys::new();
    out.extend(message_id.and_then(message_key));
    if let Some(u) = post_url {
        out.extend(value_keys(u, pubs, None));
    }
    out
}

/// Whether two key sets share a key.
#[must_use]
pub fn intersects(a: &MatchKeys, b: &MatchKeys) -> bool {
    let (s, l) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    s.iter().any(|k| l.contains(k))
}

/// A substring every stored URL that can share a key with `value` contains (for a pre-filter
/// over stored rows; callers then compare [`MatchKeys`]): the YouTube video id, else the last
/// non-empty path segment of the canonical form (an arXiv id, a slug, a status id). `None` when
/// no such segment exists (a host root; such a URL matches only by exact keys).
#[must_use]
pub fn url_probe(value: &str) -> Option<String> {
    let c = canonicalize_url(value)?;
    if let Some(id) = c.youtube_video_id {
        return Some(id);
    }
    let path = canonical_path(&c.canonical)?;
    let seg = path.rsplit('/').find(|s| !s.is_empty())?;
    let seg = seg.strip_suffix(".pdf").unwrap_or(seg);
    (!seg.is_empty()).then(|| seg.to_owned())
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
