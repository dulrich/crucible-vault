//! One publication record per publication (plan `sources-publication-model` WP-1).
//!
//! `[[publication]]` carries everything one publication is matched by (feeds, mail senders and
//! list ids, clip hosts, URL prefixes, an author, a YouTube channel id). The legacy tables still
//! load through [`legacy`]. [`SourcesConfig::by_mail`], [`SourcesConfig::by_feed`] and
//! [`SourcesConfig::by_channel`] resolve tracker items. A note or clip resolves through one order,
//! [`SourcesConfig::matches`]: any record's URL prefix, then any record's host (canonical form),
//! then a prefix-less record's author, the first record in config order winning each step.
//! [`SourcesConfig::resolve`] is its first match (typed [`PubMatch`]), [`SourcesConfig::by_note`]
//! that match's publication and [`SourcesConfig::by_host`] its host step alone.

pub(crate) mod legacy;

use serde::{Deserialize, Serialize};

use crate::pipelines::{
    BlogCanon, BodyMode, Priority, SerialStatus, SourcesConfig, author_key, publication_of,
};
use cv_text::mail_match::{mail_matches, normalize_list_id, substack_host};
use cv_text::url_canon::{bare_host_name, canonical_bare_host, configured_host, http_host};

/// The `from` matcher field of a `[[publication]]` record.
pub const FIELD_FROM: &str = "from";
/// The `list_id` matcher field.
pub const FIELD_LIST_ID: &str = "list_id";
/// The `feeds` matcher field.
pub const FIELD_FEEDS: &str = "feeds";
/// The `hosts` matcher field.
pub const FIELD_HOSTS: &str = "hosts";
/// The `prefix` matcher field.
pub const FIELD_PREFIX: &str = "prefix";
/// The `channel_id` matcher field.
pub const FIELD_CHANNEL_ID: &str = "channel_id";
/// The `author` field (a matcher when no prefix is set).
pub const FIELD_AUTHOR: &str = "author";

/// The legacy per-type table names, in display order.
pub const LEGACY_TABLE_NAMES: [&str; 4] = ["youtube", "blog", "newsletter", "serial"];

/// One `[[publication]]` record. Scalars are `None` when unset (the reader applies the default:
/// [`Publication::priority`], [`Publication::method`], …); `works` absent means "the kind
/// default" (plan WP-5).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Publication {
    /// Display name (unique, non-empty).
    pub name: String,
    /// Publication name shown in the Author column (default: `name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short: Option<String>,
    /// The `kinds.toml` kind its items file as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Display author; matches by author only when no prefix is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Works-view status (default `active`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SerialStatus>,
    /// Tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Priority (default `normal`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// Listed in the Works view; absent = the kind default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub works: Option<bool>,
    /// RSS/Atom feed URLs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub feeds: Vec<String>,
    /// Feed fetch method (default `rss`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Feed post-id canonicalization (default `auto`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canon: Option<BlogCanon>,
    /// Feed body mode (default `auto`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<BodyMode>,
    /// Body phrases (case-insensitive) marking a post or mail incomplete.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paywall_markers: Vec<String>,
    /// Mail sender matchers: `a@b.example` or `@b.example`, case-insensitive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub from: Vec<String>,
    /// Mail `List-Id` matchers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub list_id: Vec<String>,
    /// Web-clip hosts (`www.` ignored).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    /// Source-URL prefixes; when any is set the record matches clips by prefix alone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefix: Vec<String>,
    /// YouTube channel id (`UC…`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
}

/// How a note or clip matched a publication record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PubMatch<'a> {
    /// A non-blank prefix of the record starts the source (the record's author is ignored).
    Prefix(&'a Publication),
    /// The source's host is one of the record's [`Publication::hosts`]; `feed` is the record's
    /// feed on that host, else its first feed.
    Host {
        /// The record.
        publication: &'a Publication,
        /// The feed the item is contained under.
        feed: Option<&'a str>,
    },
    /// A prefix-less record whose author equals one of the note's.
    Author(&'a Publication),
}

impl<'a> PubMatch<'a> {
    /// The matched record.
    #[must_use]
    pub fn publication(self) -> &'a Publication {
        match self {
            Self::Prefix(p) | Self::Host { publication: p, .. } | Self::Author(p) => p,
        }
    }
}

pub(crate) fn nonblank(s: &str) -> Option<&str> {
    Some(s.trim()).filter(|s| !s.is_empty())
}

pub(crate) fn bare_host(h: &str) -> Option<String> {
    bare_host_name(h)
}

fn feed_host(link: &str) -> Option<String> {
    http_host(link.trim()).and_then(|h| bare_host(&h))
}

impl Publication {
    /// The `publication` frontmatter value (`short`, else `name`).
    #[must_use]
    pub fn publication(&self) -> &str {
        publication_of(self.short.as_deref(), &self.name)
    }

    /// The priority (default `normal`).
    #[must_use]
    pub fn priority(&self) -> Priority {
        self.priority.unwrap_or_default()
    }

    /// The feed fetch method (default `rss`).
    #[must_use]
    pub fn method(&self) -> &str {
        self.method.as_deref().unwrap_or("rss")
    }

    /// The Works-view status (default `active`).
    #[must_use]
    pub fn status(&self) -> SerialStatus {
        self.status.unwrap_or_default()
    }

    /// The feed post-id canonicalization (default `auto`).
    #[must_use]
    pub fn canon(&self) -> BlogCanon {
        self.canon.unwrap_or_default()
    }

    /// The feed body mode (default `auto`).
    #[must_use]
    pub fn body(&self) -> BodyMode {
        self.body.unwrap_or_default()
    }

    /// The kind its items file as (default `blog`, the old `[[newsletter]]` default).
    #[must_use]
    pub fn kind_or_blog(&self) -> &str {
        self.kind.as_deref().and_then(nonblank).unwrap_or("blog")
    }

    /// Every host of the record, as written: each feed's host (paired with that feed), then each
    /// non-blank `hosts` entry, trimmed (paired with `None`). Callers compare in their own
    /// `www.`-free lowercase form.
    pub fn hosts(&self) -> impl Iterator<Item = (String, Option<&str>)> {
        self.feeds
            .iter()
            .filter_map(|f| http_host(f.trim()).map(|h| (h, Some(f.as_str()))))
            .chain(
                self.hosts
                    .iter()
                    .filter_map(|h| nonblank(h).map(|h| (h.to_owned(), None))),
            )
    }

    /// The record's feed whose host is `host` (`www.` ignored), else its first feed: the blog
    /// tracker key a mail or clip of this publication is contained under.
    #[must_use]
    pub fn feed_for_host(&self, host: Option<&str>) -> Option<&str> {
        let want = host.and_then(bare_host);
        want.as_ref()
            .and_then(|w| self.feeds.iter().find(|f| feed_host(f).as_ref() == Some(w)))
            .or_else(|| self.feeds.first())
            .map(|f| f.trim())
    }

    /// Whether the record matches notes by URL prefix or author (a work record).
    #[must_use]
    pub fn is_work(&self) -> bool {
        self.prefixes().next().is_some() || self.author.as_deref().and_then(nonblank).is_some()
    }

    /// Whether [`Self::work_match`] matches.
    #[must_use]
    pub fn matches_note(&self, authors: &[&str], source: &str) -> bool {
        self.work_match(authors, source).is_some()
    }

    /// The work matcher: a record with any non-blank prefix matches by prefix alone (some prefix
    /// starts the trimmed `source`); else one whose author equals one of `authors` (normalized by
    /// `author_key`); else nothing.
    fn work_match(&self, authors: &[&str], source: &str) -> Option<PubMatch<'_>> {
        let src = source.trim();
        if self.prefixes().next().is_some() {
            return self
                .prefixes()
                .any(|p| src.starts_with(p))
                .then_some(PubMatch::Prefix(self));
        }
        self.author_key()
            .is_some_and(|w| authors.iter().any(|c| author_key(c) == w))
            .then_some(PubMatch::Author(self))
    }

    /// The host step for one record: `host` (`canonical_bare_host` form) is the canonical host of
    /// one of its feeds or a `hosts` entry through `configured_host`; contained under its feed on
    /// that host, else its first feed.
    fn host_match(&self, host: &str) -> Option<PubMatch<'_>> {
        let on_host = |f: &&String| canonical_bare_host(f.trim()).as_deref() == Some(host);
        let feed = self.feeds.iter().find(on_host);
        let hit = feed.is_some()
            || self
                .hosts
                .iter()
                .filter_map(|h| nonblank(h))
                .any(|h| configured_host(h).as_deref() == Some(host));
        hit.then(|| PubMatch::Host {
            publication: self,
            feed: feed.or_else(|| self.feeds.first()).map(|f| f.trim()),
        })
    }

    fn author_key(&self) -> Option<String> {
        self.author
            .as_deref()
            .map(author_key)
            .filter(|a| !a.is_empty())
    }

    /// Whether some matcher is set (feeds, from, list_id, hosts, prefix, channel_id, author).
    #[must_use]
    pub fn has_matcher(&self) -> bool {
        let any = |v: &[String]| v.iter().any(|s| nonblank(s).is_some());
        any(&self.feeds)
            || any(&self.from)
            || any(&self.list_id)
            || any(&self.hosts)
            || any(&self.prefix)
            || self.channel_id.as_deref().and_then(nonblank).is_some()
            || self.author.as_deref().and_then(nonblank).is_some()
    }

    pub(crate) fn prefixes(&self) -> impl Iterator<Item = &str> {
        self.prefix.iter().filter_map(|p| nonblank(p))
    }
}

impl SourcesConfig {
    /// Every publication record in order (`[[publication]]` rows, then legacy rows folded in
    /// table order blog, newsletter, serial, youtube).
    #[must_use]
    pub fn publications(&self) -> &[Publication] {
        &self.folded
    }

    /// The load notice of a fold that merged legacy rows (one per file), else `None`. Surfaced as
    /// a load notice by `ConfigSet::notices` (never an error).
    #[must_use]
    pub fn fold_notice(&self) -> Option<String> {
        self.notice.clone()
    }

    /// The publication a mail belongs to: the first record whose `from` (exact address or
    /// `@domain` suffix, case-insensitive) or `list_id` (normalized) matches, else the first
    /// record with a feed on the message's `<pub>.substack.com` host.
    #[must_use]
    pub fn by_mail(&self, from_addr: &str, list_id: Option<&str>) -> Option<&Publication> {
        let list = list_id.map(normalize_list_id).filter(|l| !l.is_empty());
        if let Some(p) = self
            .folded
            .iter()
            .find(|p| mail_matches(from_addr, list.as_deref(), &p.from, &p.list_id))
        {
            return Some(p);
        }
        let host = substack_host(from_addr, list_id)?;
        let host = bare_host(&host)?;
        self.folded.iter().find(|p| {
            p.feeds
                .iter()
                .any(|f| feed_host(f).is_some_and(|h| h == host))
        })
    }

    /// The first record (in order) with a non-blank prefix that starts the trimmed `source`.
    #[must_use]
    pub fn by_prefix(&self, source: &str) -> Option<&Publication> {
        let src = source.trim();
        self.folded
            .iter()
            .find(|p| p.prefixes().any(|pre| src.starts_with(pre)))
    }

    /// Every record a note (its `author` entries and `source` URL) matches, in the one order:
    /// each record (config order) with a non-blank prefix that starts the trimmed source, then
    /// each record (config order) on the source's `canonical_bare_host` ([`Self::by_host`]), then
    /// each prefix-less record whose author equals one of the note's (`[[ ]]` and `#` ignored).
    /// A record matched by more than one step appears once per step.
    pub fn matches<'a, 'b>(
        &'a self,
        authors: &'b [&'b str],
        source: &'b str,
    ) -> impl Iterator<Item = PubMatch<'a>> + use<'a, 'b> {
        let src = source.trim();
        let host = canonical_bare_host(src);
        let keys: Vec<String> = authors.iter().map(|a| author_key(a)).collect();
        let prefix = self
            .folded
            .iter()
            .filter(move |p| p.prefixes().any(|pre| src.starts_with(pre)))
            .map(PubMatch::Prefix);
        let hosts = host.into_iter().flat_map(move |h| self.host_matches(h));
        let author = self
            .folded
            .iter()
            .filter(move |p| {
                p.prefixes().next().is_none() && p.author_key().is_some_and(|w| keys.contains(&w))
            })
            .map(PubMatch::Author);
        prefix.chain(hosts).chain(author)
    }

    /// The record a note belongs to: the first of [`Self::matches`].
    #[must_use]
    pub fn resolve(&self, authors: &[&str], source: &str) -> Option<PubMatch<'_>> {
        self.matches(authors, source).next()
    }

    /// Every record (config order) the note matches by [`Publication::matches_note`]: its prefix,
    /// else its author (the fanfic title's serial pick).
    pub fn work_matches<'a, 'b>(
        &'a self,
        authors: &'b [&'b str],
        source: &'b str,
    ) -> impl Iterator<Item = PubMatch<'a>> + use<'a, 'b> {
        self.folded
            .iter()
            .filter_map(move |p| p.work_match(authors, source))
    }

    /// The publication a note belongs to: the record of [`Self::resolve`].
    #[must_use]
    pub fn by_note(&self, authors: &[&str], source: &str) -> Option<&Publication> {
        self.resolve(authors, source).map(PubMatch::publication)
    }

    fn host_matches(&self, host: String) -> impl Iterator<Item = PubMatch<'_>> {
        self.folded.iter().filter_map(move |p| p.host_match(&host))
    }

    /// The host step of [`Self::matches`] alone: the first record (config order) one of whose
    /// feeds has the canonical host `host` (`canonical_bare_host` form) or one of whose `hosts`
    /// entries is `host` through `configured_host`, contained under its feed on that host, else
    /// its first feed.
    #[must_use]
    pub fn by_host(&self, host: &str) -> Option<PubMatch<'_>> {
        self.host_matches(host.to_owned()).next()
    }

    /// The publication whose feeds include `link` (trimmed, exact).
    #[must_use]
    pub fn by_feed(&self, link: &str) -> Option<&Publication> {
        let link = link.trim();
        self.folded
            .iter()
            .find(|p| p.feeds.iter().any(|f| f.trim() == link))
    }

    /// Every `(feed link, record)` pair, records in order and feeds in record order.
    pub fn feeds(&self) -> impl Iterator<Item = (&str, &Publication)> {
        self.folded
            .iter()
            .flat_map(|p| p.feeds.iter().map(move |f| (f.as_str(), p)))
    }

    /// Every record with a YouTube channel id, as `(id, record)`.
    pub fn channels(&self) -> impl Iterator<Item = (&str, &Publication)> {
        self.folded
            .iter()
            .filter_map(|p| p.channel_id.as_deref().map(|c| (c.trim(), p)))
    }

    /// The publication with YouTube channel id `id`.
    #[must_use]
    pub fn by_channel(&self, id: &str) -> Option<&Publication> {
        let id = id.trim();
        self.folded
            .iter()
            .find(|p| p.channel_id.as_deref().map(str::trim) == Some(id))
    }

    /// Whether `host` (any form, `www.` ignored) appears in a configured matcher of any record:
    /// a feed's host, a `hosts` entry or the host of a `prefix` (plan `intake-foundations` WP-3).
    /// A reserved host is never learned by any record. Reservation is not a match: it changes no
    /// resolution.
    #[must_use]
    pub fn is_reserved_host(&self, host: &str) -> bool {
        let forms: Vec<String> = [bare_host(host), configured_host(host)]
            .into_iter()
            .flatten()
            .collect();
        if forms.is_empty() {
            return true;
        }
        self.folded.iter().any(|p| {
            p.feeds
                .iter()
                .chain(p.prefix.iter())
                .filter_map(|u| nonblank(u))
                .flat_map(|u| [feed_host(u), canonical_bare_host(u)])
                .chain(
                    p.hosts
                        .iter()
                        .filter_map(|h| nonblank(h))
                        .flat_map(|h| [bare_host(h), configured_host(h)]),
                )
                .flatten()
                .any(|h| forms.contains(&h))
        })
    }

    /// The ownership rule for learning, written once (plan `intake-foundations` WP-3): record
    /// `name` may learn `host` when the record exists and the host is not reserved
    /// ([`Self::is_reserved_host`]). That the first publication to prove a host keeps it is the
    /// store's rule (one row per host); [`Self::effective_learned`] applies this rule on read and
    /// a reload drops the rows it now rejects.
    #[must_use]
    pub fn may_learn(&self, host: &str, name: &str) -> bool {
        self.folded.iter().any(|p| p.name == name) && !self.is_reserved_host(host)
    }

    /// The learned rows that still hold under [`Self::may_learn`], host in comparison form, in
    /// the given order (the first row for a host wins).
    #[must_use]
    pub fn effective_learned<'a>(
        &'a self,
        learned: &[LearnedRow],
    ) -> Vec<(String, &'a Publication)> {
        let mut out: Vec<(String, &Publication)> = Vec::new();
        for r in learned {
            let Some(h) = bare_host(&r.host) else {
                continue;
            };
            if out.iter().any(|(o, _)| *o == h) || !self.may_learn(&h, &r.publication) {
                continue;
            }
            if let Some(p) = self.folded.iter().find(|p| p.name == r.publication) {
                out.push((h, p));
            }
        }
        out
    }

    /// A publication's effective hosts, the one place they are computed (plan
    /// `intake-foundations` WP-3): every record (config order) with its configured hosts
    /// ([`Publication::hosts`], as written) then its learned hosts that hold under
    /// [`Self::may_learn`]. The match keys (`publication_hosts` in the `cv` binary) and
    /// [`Self::resolve_learned`] both read it.
    #[must_use]
    pub fn effective_hosts<'a>(
        &'a self,
        learned: &[LearnedRow],
    ) -> Vec<(&'a Publication, Vec<String>)> {
        let learned = self.effective_learned(learned);
        self.folded
            .iter()
            .map(|p| {
                let mut hosts: Vec<String> = p.hosts().map(|(h, _)| h).collect();
                hosts.extend(
                    learned
                        .iter()
                        .filter(|(_, q)| std::ptr::eq(*q, p))
                        .map(|(h, _)| h.clone()),
                );
                (p, hosts)
            })
            .collect()
    }

    /// The host step over learned hosts alone: the record that learned `host` (comparison form),
    /// contained under its first feed.
    #[must_use]
    pub fn by_learned_host(&self, host: &str, learned: &[LearnedRow]) -> Option<PubMatch<'_>> {
        let want = bare_host(host)?;
        self.effective_learned(learned)
            .into_iter()
            .find(|(h, _)| *h == want)
            .map(|(_, p)| PubMatch::Host {
                publication: p,
                feed: p.feeds.first().map(|f| f.trim()),
            })
    }

    /// [`Self::by_host`], else [`Self::by_learned_host`].
    #[must_use]
    pub fn by_host_learned(&self, host: &str, learned: &[LearnedRow]) -> Option<PubMatch<'_>> {
        self.by_host(host)
            .or_else(|| self.by_learned_host(host, learned))
    }

    /// [`Self::resolve`], else the source's learned host: a learned host never changes which
    /// record a configured matcher selects (D36 precedence unchanged), and an empty `learned` is
    /// exactly [`Self::resolve`].
    #[must_use]
    pub fn resolve_learned(
        &self,
        authors: &[&str],
        source: &str,
        learned: &[LearnedRow],
    ) -> Option<PubMatch<'_>> {
        self.resolve(authors, source).or_else(|| {
            canonical_bare_host(source.trim()).and_then(|h| self.by_learned_host(&h, learned))
        })
    }
}

/// One learned host as plain data (the `cv` binary reads it from `state.db`): `host` owned by
/// the record named `publication`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearnedRow {
    /// The host (any case, `www.` ignored).
    pub host: String,
    /// The record `name` that proved it.
    pub publication: String,
}

/// The host a feed item's link proves for the feed's publication (plan `intake-foundations`
/// WP-3, deferred audit item E1): `Some` only when `url` is http(s), is provably the item's link
/// and not its GUID — it differs from `item_id` as written and canonically (the blog tracker's
/// `url` is `link || guid` and its `item_id` is `guid`, else the canonical link, so a row with
/// no link has `url == item_id`) — and its canonical host differs from the feed's. The ownership
/// rule ([`SourcesConfig::may_learn`]) is the caller's next check.
#[must_use]
pub fn feed_link_host(url: &str, item_id: &str, feed: &str) -> Option<String> {
    let (url, id) = (url.trim(), item_id.trim());
    http_host(url)?;
    if url == id {
        return None;
    }
    let canon = |s: &str| cv_text::url_canon::canonicalize_url(s).map(|c| c.canonical);
    if canon(url).is_some_and(|c| Some(c) == canon(id)) {
        return None;
    }
    let host = canonical_bare_host(url)?;
    (canonical_bare_host(feed.trim()).as_deref() != Some(host.as_str())).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<SourcesConfig, String> {
        SourcesConfig::from_table(&s.parse::<toml::Table>().unwrap())
    }

    fn one(c: &SourcesConfig) -> &Publication {
        assert_eq!(c.publications().len(), 1, "{:?}", c.publications());
        &c.publications()[0]
    }

    #[test]
    fn newsletter_and_serial_fold_with_serial_kind_over_the_default() {
        let c = parse(
            "[[newsletter]]\nname = \"P\"\nfrom = [\"@p.example\"]\ntags = [\"a\"]\n[[serial]]\nname = \"P\"\nprefix = [\"https://p.example/s/\"]\nkind = \"fanfic\"\nworks = true\n",
        )
        .unwrap();
        let p = one(&c);
        assert_eq!(p.kind.as_deref(), Some("fanfic"));
        assert_eq!(p.from, ["@p.example"]);
        assert_eq!(p.prefix, ["https://p.example/s/"]);
        assert_eq!(p.works, Some(true));
        assert!(c.fold_notice().unwrap().contains("newsletter+serial"));
        // Explicit newsletter kind wins over the serial's, and the notice says so.
        let c = parse(
            "[[newsletter]]\nname = \"P\"\nfrom = [\"@p.example\"]\nkind = \"post\"\n[[serial]]\nname = \"P\"\nauthor = \"A\"\nkind = \"fanfic\"\n",
        )
        .unwrap();
        assert_eq!(one(&c).kind.as_deref(), Some("post"));
        assert!(c.fold_notice().unwrap().contains("keeps newsletter kind"));
    }

    #[test]
    fn blog_and_newsletter_fold_by_name_and_by_blog_link() {
        for nl in [
            "name = \"B\"\nfrom = [\"b@b.example\"]",
            "name = \"B Letter\"\nblog = \"B\"\nfrom = [\"b@b.example\"]",
        ] {
            let c = parse(&format!(
                "[[blog]]\nname = \"B\"\nlink = \"https://b.example/feed\"\npaywall_markers = [\"x\"]\n[[newsletter]]\n{nl}\npaywall_markers = [\"x\", \"y\"]\n"
            ))
            .unwrap();
            let p = one(&c);
            assert_eq!(p.name, "B");
            assert_eq!(p.feeds, ["https://b.example/feed"]);
            assert_eq!(p.from, ["b@b.example"]);
            assert_eq!(p.paywall_markers, ["x", "y"]);
            assert_eq!(p.kind.as_deref(), Some("blog"));
        }
    }

    #[test]
    fn blog_and_serial_and_two_newsletters_fold() {
        let c = parse(
            "[[blog]]\nname = \"B\"\nlink = \"https://b.example/feed\"\n[[serial]]\nname = \"S\"\nshort = \"B\"\nauthor = \"Ann\"\nkind = \"fanfic\"\n",
        )
        .unwrap();
        let p = one(&c);
        assert_eq!(p.author.as_deref(), Some("Ann"));
        assert_eq!(p.publication(), "B");
        let c = parse(
            "[[newsletter]]\nname = \"N1\"\nshort = \"N\"\nfrom = [\"a@n.example\"]\n[[newsletter]]\nname = \"N2\"\nshort = \"N\"\nlist_id = \"l.n.example\"\n",
        )
        .unwrap();
        let p = one(&c);
        assert_eq!(p.name, "N1");
        assert_eq!(p.list_id, ["l.n.example"]);
        assert_eq!(c.origins()[0].len(), 2);
    }

    #[test]
    fn conflicting_scalars_and_works_are_load_errors() {
        let e = parse(
            "[[blog]]\nname = \"B\"\nlink = \"https://b.example/feed\"\npriority = \"high\"\n[[youtube]]\nchannel = \"B\"\nid = \"UC1\"\npriority = \"low\"\n",
        )
        .unwrap_err();
        assert!(e.contains("\"B\"") && e.contains("`priority`"), "{e}");
        let e = parse(
            "[[newsletter]]\nname = \"N\"\nfrom = [\"a@n.example\"]\nkind = \"post\"\n[[newsletter]]\nname = \"M\"\nshort = \"N\"\nfrom = [\"b@n.example\"]\nkind = \"essay\"\n",
        )
        .unwrap_err();
        assert!(e.contains("`kind`"), "{e}");
        let e = parse(
            "[[publication]]\nname = \"P\"\nauthor = \"A\"\nworks = false\n[[serial]]\nname = \"P\"\nauthor = \"A\"\nworks = true\n",
        )
        .unwrap_err();
        assert!(e.contains("`works`"), "{e}");
    }

    #[test]
    fn invariant_errors() {
        // No matcher.
        assert!(
            parse("[[publication]]\nname = \"P\"\nkind = \"blog\"\n")
                .unwrap_err()
                .contains("matcher")
        );
        // Empty and duplicate names.
        assert!(parse("[[publication]]\nname = \" \"\nauthor = \"A\"\n").is_err());
        assert!(
            parse("[[publication]]\nname = \"P\"\nauthor = \"A\"\n[[publication]]\nname = \"P\"\nauthor = \"B\"\n")
                .unwrap_err()
                .contains("duplicate")
        );
        // A matcher owned by two records names both.
        for (a, b) in [
            ("from = [\"X@n.example\"]", "from = [\"x@n.example\"]"),
            ("list_id = [\"l.example\"]", "list_id = [\"<L.example>\"]"),
            ("feeds = [\"https://f/x\"]", "feeds = [\"https://f/x\"]"),
            ("hosts = [\"www.h.example\"]", "hosts = [\"h.example\"]"),
            ("prefix = [\"https://p/\"]", "prefix = [\" https://p/\"]"),
            ("channel_id = \"UC1\"", "channel_id = \"UC1\""),
        ] {
            let e = parse(&format!(
                "[[publication]]\nname = \"P\"\n{a}\n[[publication]]\nname = \"Q\"\n{b}\n"
            ))
            .unwrap_err();
            assert!(e.contains("\"P\"") && e.contains("\"Q\""), "{e}");
        }
    }

    #[test]
    fn author_only_record_is_valid_and_prefix_beats_author_and_host() {
        let c = parse(
            "[[publication]]\nname = \"ByAuthor\"\nauthor = \"[[Ann]]\"\n[[publication]]\nname = \"ByHost\"\nhosts = [\"s.example\"]\n[[publication]]\nname = \"ByPrefix\"\nauthor = \"Bob\"\nprefix = [\"https://s.example/s/1\"]\n",
        )
        .unwrap();
        let name = |a: &[&str], s: &str| c.by_note(a, s).map(|p| p.name.as_str());
        assert_eq!(name(&["Ann"], "https://other.example/"), Some("ByAuthor"));
        assert_eq!(name(&["Ann"], "https://s.example/s/1/c2"), Some("ByPrefix"));
        assert_eq!(name(&["Ann"], "https://www.s.example/x"), Some("ByHost"));
        // A prefixed record's author is ignored.
        assert_eq!(name(&["Bob"], "https://none.example/"), None);
    }

    #[test]
    fn mail_feed_and_channel_resolvers() {
        let c = parse(
            "[[blog]]\nname = \"B\"\nlink = \"https://b.substack.com/feed\"\n[[newsletter]]\nname = \"N\"\nfrom = [\"@n.example\"]\nlist_id = \"\\\"N\\\" <l.n.example>\"\n[[youtube]]\nchannel = \"Y\"\nid = \"UCy\"\n",
        )
        .unwrap();
        let mail = |f: &str, l: Option<&str>| c.by_mail(f, l).map(|p| p.name.as_str());
        assert_eq!(mail("A@N.example", None), Some("N"));
        assert_eq!(mail("x@o.example", Some("l.n.example")), Some("N"));
        assert_eq!(mail("b@substack.com", None), Some("B"));
        assert_eq!(mail("x@o.example", Some("b.substack.com")), Some("B"));
        assert_eq!(mail("x@o.example", None), None);
        assert_eq!(
            c.by_feed(" https://b.substack.com/feed")
                .map(|p| p.name.as_str()),
            Some("B")
        );
        assert_eq!(c.by_channel("UCy").map(|p| p.name.as_str()), Some("Y"));
        assert!(c.fold_notice().is_none());
    }

    /// A legacy file with no shared publication resolves through the new resolver exactly as
    /// through the per-type rows.
    #[test]
    fn unmerged_legacy_config_resolves_like_the_old_rows() {
        let c = parse(
            "[[blog]]\nname = \"B\"\nshort = \"BB\"\nlink = \"https://b.example/feed\"\n[[newsletter]]\nname = \"N\"\nfrom = [\"@n.example\"]\nhosts = [\"n.example\"]\n[[serial]]\nname = \"S\"\nauthor = \"Ann\"\nkind = \"fanfic\"\n[[serial]]\nname = \"T\"\nprefix = [\"https://t.example/\"]\n[[youtube]]\nchannel = \"Y\"\nid = \"UCy\"\n",
        )
        .unwrap();
        assert_eq!(c.publications().len(), 5);
        assert!(c.fold_notice().is_none());
        assert_eq!(
            c.by_feed(&c.blog[0].link).unwrap().publication(),
            c.blog[0].publication()
        );
        assert_eq!(
            c.by_mail("x@n.example", None).unwrap().publication(),
            c.newsletter[0].publication()
        );
        for (s, a, src) in [
            (&c.serial[0], "Ann", "https://z.example/"),
            (&c.serial[1], "Zed", "https://t.example/1"),
        ] {
            assert!(s.matches(&[a], src));
            assert_eq!(c.by_note(&[a], src).unwrap().publication(), s.publication());
        }
        assert_eq!(
            c.by_channel("UCy").unwrap().publication(),
            c.youtube[0].publication()
        );
    }

    /// The typed resolver: prefix, host (with its feed), author, no match.
    #[test]
    fn resolve_is_typed_in_one_order() {
        let c = parse(
            "[[publication]]\nname = \"ByAuthor\"\nauthor = \"Ann\"\n[[publication]]\nname = \"ByPrefix\"\nprefix = [\"https://s.example/s/\"]\n[[publication]]\nname = \"ByHost\"\nfeeds = [\"https://h.example/feed\", \"https://www.k.example/rss\"]\nhosts = [\"s.example\"]\n",
        )
        .unwrap();
        let name = |m: Option<PubMatch<'_>>| m.map(|m| m.publication().name.clone());
        let host = c.resolve(&[], "https://k.example/p/1");
        assert!(matches!(
            host,
            Some(PubMatch::Host {
                feed: Some("https://www.k.example/rss"),
                ..
            })
        ));
        assert_eq!(name(host).as_deref(), Some("ByHost"));
        // Prefix beats the author record listed before it.
        let pre = c.resolve(&["Ann"], "https://s.example/s/1");
        assert!(matches!(pre, Some(PubMatch::Prefix(_))));
        assert_eq!(name(pre).as_deref(), Some("ByPrefix"));
        // Host beats author.
        let src = "https://s.example/other";
        assert_eq!(name(c.resolve(&["Ann"], src)).as_deref(), Some("ByHost"));
        assert!(matches!(
            c.resolve(&["Ann"], "https://other.example/"),
            Some(PubMatch::Author(_))
        ));
        assert_eq!(name(c.resolve(&["Bob"], "https://none.example/")), None);
        // Every match, in order: prefix, host, author.
        let all: Vec<String> = c
            .matches(&["Ann"], "https://s.example/s/1")
            .map(|m| m.publication().name.clone())
            .collect();
        assert_eq!(all, ["ByPrefix", "ByHost", "ByAuthor"]);
        // `by_host`: a `hosts` entry, under the record's first feed.
        assert!(matches!(
            c.by_host("s.example"),
            Some(PubMatch::Host {
                feed: Some("https://h.example/feed"),
                ..
            })
        ));
        assert!(c.by_host("none.example").is_none());
    }

    fn lr(host: &str, publication: &str) -> LearnedRow {
        LearnedRow {
            host: host.into(),
            publication: publication.into(),
        }
    }

    /// Plan intake-foundations WP-3: an empty learned table changes no resolution.
    #[test]
    fn empty_learned_table_changes_no_resolution() {
        let c = parse(
            "[[publication]]\nname = \"ByAuthor\"\nauthor = \"Ann\"\n[[publication]]\nname = \"ByPrefix\"\nprefix = [\"https://s.example/s/\"]\n[[publication]]\nname = \"ByHost\"\nfeeds = [\"https://h.example/feed\"]\nhosts = [\"s.example\"]\n",
        )
        .unwrap();
        for (a, s) in [
            (&["Ann"][..], "https://s.example/s/1"),
            (&[][..], "https://h.example/p/1"),
            (&["Ann"][..], "https://other.example/"),
            (&["Bob"][..], "https://none.example/"),
        ] {
            assert_eq!(c.resolve_learned(a, s, &[]), c.resolve(a, s));
        }
        for (p, hosts) in c.effective_hosts(&[]) {
            assert_eq!(hosts, p.hosts().map(|(h, _)| h).collect::<Vec<_>>());
        }
        assert!(c.by_host_learned("none.example", &[]).is_none());
    }

    /// A learned host resolves only where no configured matcher does; a host configured on B is
    /// never learned for A; an unknown record learns nothing.
    #[test]
    fn learned_hosts_follow_the_ownership_rule() {
        let c = parse(
            "[[publication]]\nname = \"A\"\nfrom = [\"@a.example\"]\nfeeds = [\"https://a.example/feed\"]\n[[publication]]\nname = \"B\"\nhosts = [\"b.example\"]\n[[publication]]\nname = \"Ann\"\nauthor = \"Ann\"\n",
        )
        .unwrap();
        assert!(c.may_learn("a.substack.com", "A"));
        assert!(!c.may_learn("www.b.example", "A"));
        assert!(!c.may_learn("a.example", "A"));
        assert!(!c.may_learn("a.substack.com", "Gone"));
        let learned = [lr("b.example", "A"), lr("a.substack.com", "A")];
        let eff = c.effective_learned(&learned);
        assert_eq!(eff.len(), 1);
        assert_eq!(eff[0].0, "a.substack.com");
        let name = |m: Option<PubMatch<'_>>| m.map(|m| m.publication().name.clone());
        assert_eq!(
            name(c.resolve_learned(&[], "https://a.substack.com/p/x", &learned)).as_deref(),
            Some("A")
        );
        // B keeps its configured host.
        assert_eq!(
            name(c.resolve_learned(&[], "https://b.example/p/x", &learned)).as_deref(),
            Some("B")
        );
        // A configured author match is never overridden by a learned host.
        assert_eq!(
            name(c.resolve_learned(&["Ann"], "https://a.substack.com/p/x", &learned)).as_deref(),
            Some("Ann")
        );
        // The first row for a host wins.
        let eff = c.effective_learned(&[lr("x.example", "A"), lr("x.example", "B")]);
        assert_eq!(eff.len(), 1);
        assert_eq!(eff[0].1.name, "A");
        let hosts: Vec<Vec<String>> = c
            .effective_hosts(&learned)
            .into_iter()
            .map(|(_, h)| h)
            .collect();
        assert_eq!(hosts[0], ["a.example", "a.substack.com"]);
        assert_eq!(hosts[1], ["b.example"]);
    }

    /// Two records sharing a platform host through disjoint prefixes resolve as before, and the
    /// host is reserved against learning.
    #[test]
    fn shared_prefix_host_is_reserved_and_resolves_as_today() {
        let c = parse(
            "[[publication]]\nname = \"P\"\nprefix = [\"https://plat.example/u/p/\"]\n[[publication]]\nname = \"Q\"\nprefix = [\"https://plat.example/u/q/\"]\n",
        )
        .unwrap();
        assert!(c.is_reserved_host("plat.example"));
        assert!(c.is_reserved_host("www.plat.example"));
        assert!(!c.may_learn("plat.example", "P") && !c.may_learn("plat.example", "Q"));
        let learned = [lr("plat.example", "P")];
        for (src, want) in [
            ("https://plat.example/u/p/1", Some("P")),
            ("https://plat.example/u/q/1", Some("Q")),
            ("https://plat.example/u/r/1", None),
        ] {
            let got = c.resolve_learned(&[], src, &learned);
            assert_eq!(got, c.resolve(&[], src));
            assert_eq!(got.map(|m| m.publication().name.as_str()), want);
        }
    }

    /// Deferred audit item E1: only a link that is provably not the GUID proves a host.
    #[test]
    fn feed_link_host_needs_a_link_that_is_not_the_guid() {
        let feed = "https://feeds.example/p/rss";
        assert_eq!(
            feed_link_host("https://blog.example/p/1", "guid-1", feed).as_deref(),
            Some("blog.example")
        );
        // GUID-as-URL on a foreign host (no link): url == item_id.
        assert_eq!(
            feed_link_host("https://guid.example/x/1", "https://guid.example/x/1", feed),
            None
        );
        // Canonically equal (tracking param, trailing slash).
        assert_eq!(
            feed_link_host(
                "https://blog.example/p/1/?utm_source=x",
                "https://blog.example/p/1",
                feed
            ),
            None
        );
        // Same host as the feed, and not http(s).
        assert_eq!(feed_link_host("https://feeds.example/p/1", "g", feed), None);
        assert_eq!(feed_link_host("tag:blog.example,1", "g", feed), None);
    }

    #[test]
    fn hosts_are_feed_hosts_then_listed_hosts_as_written() {
        let p = Publication {
            feeds: vec![" https://WWW.F.example/feed".into(), "not a url".into()],
            hosts: vec![" www.h.example ".into(), " ".into()],
            ..Publication::default()
        };
        let got: Vec<(String, Option<&str>)> = p.hosts().collect();
        assert_eq!(
            got,
            [
                (
                    "www.f.example".to_owned(),
                    Some(" https://WWW.F.example/feed")
                ),
                ("www.h.example".to_owned(), None),
            ]
        );
    }

    #[test]
    fn legacy_table_names_are_the_non_publication_table_names() {
        use legacy::Table;
        let names = [
            Table::Youtube,
            Table::Blog,
            Table::Newsletter,
            Table::Serial,
        ]
        .map(Table::name);
        assert_eq!(names, LEGACY_TABLE_NAMES);
    }

    #[test]
    fn legacy_tables_keep_their_names_and_answer_intent() {
        use legacy::Table;
        let names: Vec<String> = [
            Table::Publication,
            Table::Blog,
            Table::Newsletter,
            Table::Serial,
            Table::Youtube,
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(
            names,
            ["publication", "blog", "newsletter", "serial", "youtube"]
        );
        let c = parse(
            "[[publication]]\nname = \"P\"\nauthor = \"A\"\n[[newsletter]]\nname = \"N\"\nfrom = [\"@n.example\"]\n[[serial]]\nname = \"N\"\nauthor = \"B\"\n",
        )
        .unwrap();
        assert_eq!(c.origins()[1][0].table.name(), "newsletter");
        assert!(!c.was_serial(0) && !c.was_newsletter(0));
        assert!(c.was_serial(1) && c.was_newsletter(1));
        assert!(!c.was_serial(9));
    }
}
