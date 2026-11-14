//! One publication record per publication (plan `sources-publication-model` WP-1).
//!
//! `[[publication]]` carries everything one publication is matched by (feeds, mail senders and
//! list ids, clip hosts, URL prefixes, an author, a YouTube channel id). The legacy
//! `[[blog]]`/`[[newsletter]]`/`[[serial]]`/`[[youtube]]` tables still load: [`fold`] merges them
//! into records keyed by `publication_of(short, name)`, so an un-migrated file keeps working and
//! `cv sources migrate` can rewrite it. [`SourcesConfig`]'s resolver (`by_mail`, `by_note`,
//! `by_feed`, `by_channel`) answers "which publication is this" for every intake path.

use serde::{Deserialize, Serialize};

use crate::pipelines::{
    BlogCanon, BodyMode, Priority, SerialStatus, SourcesConfig, author_key, publication_of,
};

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

/// Where a folded record's row came from: `table` is `publication`, `blog`, `newsletter`,
/// `serial` or `youtube`; `index` is the row's position in that table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowRef {
    /// The table name.
    pub table: &'static str,
    /// The row index within it.
    pub index: usize,
}

fn nonblank(s: &str) -> Option<&str> {
    Some(s.trim()).filter(|s| !s.is_empty())
}

fn bare_host(h: &str) -> Option<String> {
    cv_text::url_canon::bare_host_name(h)
}

fn feed_host(link: &str) -> Option<String> {
    cv_text::url_canon::http_host(link.trim()).and_then(|h| bare_host(&h))
}

/// `"Name" <id.example>` / `<id.example>` / `id.example` → `id.example` (lowercase), as
/// `cv_ingest::mail::normalize_list_id`.
fn normalize_list_id(s: &str) -> String {
    let inner = match (s.rfind('<'), s.rfind('>')) {
        (Some(a), Some(b)) if a < b => &s[a + 1..b],
        _ => s,
    };
    inner.trim().to_lowercase()
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

    fn prefixes(&self) -> impl Iterator<Item = &str> {
        self.prefix.iter().filter_map(|p| nonblank(p))
    }

    /// A mail from `addr` (lowercased) / with `list` (normalized) matches a `from` entry (exact
    /// address or `@domain` suffix) or a `list_id` (`cv_ingest::mail::row_matches`).
    fn mail_matches(&self, addr: &str, list: Option<&str>) -> bool {
        let from_hit = !addr.is_empty()
            && self.from.iter().any(|f| {
                let f = f.trim().to_lowercase();
                if f.is_empty() {
                    false
                } else if f.starts_with('@') {
                    addr.ends_with(&f)
                } else {
                    addr == f
                }
            });
        let list_hit = list.is_some_and(|got| {
            self.list_id.iter().any(|l| {
                let want = normalize_list_id(l);
                !want.is_empty() && want == got
            })
        });
        from_hit || list_hit
    }
}

/// The `<pub>.substack.com` host of a message, as `cv_ingest::mail::substack_host`.
fn substack_host(from_addr: &str, list_id: Option<&str>) -> Option<String> {
    const SUFFIX: &str = ".substack.com";
    let single_label = |h: &str| {
        h.strip_suffix(SUFFIX)
            .is_some_and(|l| !l.is_empty() && !l.contains('.'))
    };
    if let Some(l) = list_id {
        let l = l.trim().to_ascii_lowercase();
        if single_label(&l) {
            return Some(l);
        }
    }
    let (local, domain) = from_addr.trim().rsplit_once('@')?;
    if domain.eq_ignore_ascii_case("substack.com") && !local.is_empty() {
        let h = format!("{}{SUFFIX}", local.to_ascii_lowercase());
        return single_label(&h).then_some(h);
    }
    None
}

impl SourcesConfig {
    /// Every publication record in order (`[[publication]]` rows, then legacy rows folded in
    /// table order blog, newsletter, serial, youtube).
    #[must_use]
    pub fn publications(&self) -> &[Publication] {
        &self.folded
    }

    /// The rows each record of [`Self::publications`] came from.
    #[must_use]
    pub fn origins(&self) -> &[Vec<RowRef>] {
        &self.origins
    }

    /// The load notice of a fold that merged legacy rows (one per file), else `None`. Wired to
    /// the config-error surface by WP-2.
    #[must_use]
    pub fn fold_notice(&self) -> Option<String> {
        self.notice.clone()
    }

    /// Whether the file still has legacy rows (`cv sources migrate` has work to do).
    #[must_use]
    pub fn has_legacy_rows(&self) -> bool {
        !(self.blog.is_empty()
            && self.newsletter.is_empty()
            && self.serial.is_empty()
            && self.youtube.is_empty())
    }

    /// The publication a mail belongs to: the first record whose `from` (exact address or
    /// `@domain` suffix, case-insensitive) or `list_id` (normalized) matches, else the first
    /// record with a feed on the message's `<pub>.substack.com` host.
    #[must_use]
    pub fn by_mail(&self, from_addr: &str, list_id: Option<&str>) -> Option<&Publication> {
        let addr = from_addr.trim().to_lowercase();
        let list = list_id.map(normalize_list_id).filter(|l| !l.is_empty());
        if let Some(p) = self
            .folded
            .iter()
            .find(|p| p.mail_matches(&addr, list.as_deref()))
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

    /// The publication a note (its `author` entries and `source` URL) belongs to: a record whose
    /// non-blank prefix starts the trimmed source (author ignored), else one whose feed host or
    /// `hosts` entry is the source's host (`www.` ignored), else a prefix-less record whose
    /// author equals one of the note's (`[[ ]]` and `#` ignored). First record in order wins at
    /// each step.
    #[must_use]
    pub fn by_note(&self, authors: &[&str], source: &str) -> Option<&Publication> {
        let src = source.trim();
        if let Some(p) = self
            .folded
            .iter()
            .find(|p| p.prefixes().any(|pre| src.starts_with(pre)))
        {
            return Some(p);
        }
        if let Some(host) = feed_host(src)
            && let Some(p) = self.folded.iter().find(|p| {
                p.feeds.iter().any(|f| feed_host(f).as_ref() == Some(&host))
                    || p.hosts.iter().any(|h| bare_host(h).as_ref() == Some(&host))
            })
        {
            return Some(p);
        }
        let keys: Vec<String> = authors.iter().map(|a| author_key(a)).collect();
        self.folded.iter().find(|p| {
            p.prefixes().next().is_none()
                && p.author
                    .as_deref()
                    .map(author_key)
                    .filter(|a| !a.is_empty())
                    .is_some_and(|w| keys.contains(&w))
        })
    }

    /// The publication whose feeds include `link` (trimmed, exact).
    #[must_use]
    pub fn by_feed(&self, link: &str) -> Option<&Publication> {
        let link = link.trim();
        self.folded
            .iter()
            .find(|p| p.feeds.iter().any(|f| f.trim() == link))
    }

    /// The publication with YouTube channel id `id`.
    #[must_use]
    pub fn by_channel(&self, id: &str) -> Option<&Publication> {
        let id = id.trim();
        self.folded
            .iter()
            .find(|p| p.channel_id.as_deref().map(str::trim) == Some(id))
    }
}

/// Where a candidate `kind` came from (the pinned serial/newsletter rule).
#[derive(Clone, Copy, PartialEq, Eq)]
enum KindFrom {
    /// A newsletter's default `blog`: any explicit kind wins over it.
    NewsletterDefault,
    Newsletter,
    Serial,
    Other,
}

struct Acc {
    key: String,
    rec: Publication,
    kind_from: Option<KindFrom>,
    origins: Vec<RowRef>,
}

fn union(into: &mut Vec<String>, from: &[String]) {
    for v in from {
        if !into.contains(v) {
            into.push(v.clone());
        }
    }
}

fn merge_scalar<T: Clone + PartialEq + std::fmt::Debug>(
    into: &mut Option<T>,
    from: Option<&T>,
    publication: &str,
    field: &str,
) -> Result<(), String> {
    match (into.as_ref(), from) {
        (_, None) => Ok(()),
        (None, Some(v)) => {
            *into = Some(v.clone());
            Ok(())
        }
        (Some(a), Some(b)) if a == b => Ok(()),
        (Some(a), Some(b)) => Err(format!(
            "publication {publication:?}: rows give conflicting `{field}` ({a:?} vs {b:?})"
        )),
    }
}

/// Merges `row` (with its kind provenance) into `acc`; `notes` collects fold notices.
fn merge(
    acc: &mut Acc,
    row: Publication,
    kind_from: KindFrom,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let name = acc.key.clone();
    let r = &mut acc.rec;
    if r.short.as_deref().and_then(nonblank).is_none() {
        r.short = row.short.clone().filter(|s| nonblank(s).is_some());
    }
    merge_scalar(&mut r.author, row.author.as_ref(), &name, "author")?;
    merge_scalar(&mut r.status, row.status.as_ref(), &name, "status")?;
    merge_scalar(&mut r.priority, row.priority.as_ref(), &name, "priority")?;
    merge_scalar(&mut r.works, row.works.as_ref(), &name, "works")?;
    merge_scalar(&mut r.method, row.method.as_ref(), &name, "method")?;
    merge_scalar(&mut r.canon, row.canon.as_ref(), &name, "canon")?;
    merge_scalar(&mut r.body, row.body.as_ref(), &name, "body")?;
    merge_scalar(
        &mut r.channel_id,
        row.channel_id.as_ref(),
        &name,
        "channel_id",
    )?;
    union(&mut r.tags, &row.tags);
    union(&mut r.feeds, &row.feeds);
    union(&mut r.paywall_markers, &row.paywall_markers);
    union(&mut r.from, &row.from);
    union(&mut r.list_id, &row.list_id);
    union(&mut r.hosts, &row.hosts);
    union(&mut r.prefix, &row.prefix);
    if let Some(k) = row.kind {
        match (&r.kind, acc.kind_from) {
            (None, _) | (Some(_), None) => {
                r.kind = Some(k);
                acc.kind_from = Some(kind_from);
            }
            (Some(a), Some(_)) if *a == k => {
                if kind_from != KindFrom::NewsletterDefault {
                    acc.kind_from = Some(kind_from);
                }
            }
            (Some(_), Some(KindFrom::NewsletterDefault)) => {
                r.kind = Some(k);
                acc.kind_from = Some(kind_from);
            }
            (Some(_), Some(_)) if kind_from == KindFrom::NewsletterDefault => {}
            (Some(a), Some(KindFrom::Newsletter)) if kind_from == KindFrom::Serial => {
                notes.push(format!(
                    "{name:?} keeps newsletter kind {a:?} over serial kind {k:?}"
                ));
            }
            (Some(a), Some(KindFrom::Serial)) if kind_from == KindFrom::Newsletter => {
                notes.push(format!(
                    "{name:?} keeps newsletter kind {k:?} over serial kind {a:?}"
                ));
                r.kind = Some(k);
                acc.kind_from = Some(kind_from);
            }
            (Some(a), Some(_)) => {
                return Err(format!(
                    "publication {name:?}: rows give conflicting `kind` ({a:?} vs {k:?})"
                ));
            }
        }
    }
    Ok(())
}

fn some_nonblank(s: Option<&str>) -> Option<String> {
    s.and_then(nonblank).map(str::to_owned)
}

/// Folds `c.publication` and the legacy rows into `c.folded`, enforcing the record invariants.
pub(crate) fn fold(c: &mut SourcesConfig) -> Result<(), String> {
    let mut accs: Vec<Acc> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let place = |accs: &mut Vec<Acc>,
                 key: String,
                 row: Publication,
                 kind_from: KindFrom,
                 origin: RowRef,
                 notes: &mut Vec<String>|
     -> Result<(), String> {
        if let Some(a) = accs.iter_mut().find(|a| a.key == key) {
            if origin.table == "publication" && a.origins.iter().any(|o| o.table == "publication") {
                return Err(format!("publication {:?}: duplicate name", row.name));
            }
            a.origins.push(origin);
            merge(a, row, kind_from, notes)
        } else {
            let kind_from = row.kind.as_ref().map(|_| kind_from);
            accs.push(Acc {
                key,
                rec: row,
                kind_from,
                origins: vec![origin],
            });
            Ok(())
        }
    };

    for (i, p) in c.publication.iter().enumerate() {
        if p.name.trim().is_empty() {
            return Err(format!("publication #{}: needs a non-empty `name`", i + 1));
        }
        if !p.has_matcher() {
            return Err(format!(
                "publication {:?}: needs a feeds, from, list_id, hosts, prefix, channel_id or author matcher",
                p.name
            ));
        }
        let key = p.publication().to_owned();
        let origin = RowRef {
            table: "publication",
            index: i,
        };
        place(
            &mut accs,
            key,
            p.clone(),
            KindFrom::Other,
            origin,
            &mut notes,
        )?;
    }
    for (i, b) in c.blog.iter().enumerate() {
        let row = Publication {
            name: b.name.clone(),
            short: some_nonblank(b.short.as_deref()),
            tags: b.tags.clone(),
            priority: Some(b.priority).filter(|p| *p != Priority::Normal),
            works: b.works.then_some(true),
            feeds: vec![b.link.clone()],
            method: Some(b.method.clone()).filter(|m| m != "rss"),
            canon: Some(b.canon).filter(|v| *v != BlogCanon::Auto),
            body: Some(b.body).filter(|v| *v != BodyMode::Auto),
            paywall_markers: b.paywall_markers.clone(),
            ..Publication::default()
        };
        let key = b.publication().to_owned();
        let origin = RowRef {
            table: "blog",
            index: i,
        };
        place(&mut accs, key, row, KindFrom::Other, origin, &mut notes)?;
    }
    for (i, n) in c.newsletter.iter().enumerate() {
        // A `blog = "<name>"` link folds the newsletter into that blog's record.
        let key = n
            .blog
            .as_deref()
            .and_then(nonblank)
            .and_then(|b| c.blog.iter().find(|r| r.name == b))
            .map_or_else(
                || n.publication().to_owned(),
                |b| b.publication().to_owned(),
            );
        let default_kind = n.kind == "blog";
        let row = Publication {
            name: n.name.clone(),
            short: some_nonblank(n.short.as_deref()),
            kind: some_nonblank(Some(&n.kind)),
            tags: n.tags.clone(),
            works: n.works.then_some(true),
            paywall_markers: n.paywall_markers.clone(),
            from: n.from.clone(),
            list_id: n.list_id.iter().cloned().collect(),
            hosts: n.hosts.clone(),
            ..Publication::default()
        };
        let from = if default_kind {
            KindFrom::NewsletterDefault
        } else {
            KindFrom::Newsletter
        };
        let origin = RowRef {
            table: "newsletter",
            index: i,
        };
        place(&mut accs, key, row, from, origin, &mut notes)?;
    }
    for (i, s) in c.serial.iter().enumerate() {
        // Several Works may share one publication title (`short`): each stays its own record,
        // keyed by its (unique) serial name.
        let shared = c
            .serial
            .iter()
            .filter(|o| o.publication() == s.publication())
            .count()
            > 1;
        let key = if shared {
            s.name.clone()
        } else {
            s.publication().to_owned()
        };
        let row = Publication {
            name: s.name.clone(),
            short: some_nonblank(s.short.as_deref()),
            kind: some_nonblank(Some(&s.kind)),
            author: some_nonblank(s.author.as_deref()),
            status: Some(s.status).filter(|v| *v != SerialStatus::Active),
            works: s.works.then_some(true),
            prefix: s.prefix.clone(),
            ..Publication::default()
        };
        let origin = RowRef {
            table: "serial",
            index: i,
        };
        place(&mut accs, key, row, KindFrom::Serial, origin, &mut notes)?;
    }
    for (i, y) in c.youtube.iter().enumerate() {
        let row = Publication {
            name: y.channel.clone(),
            short: some_nonblank(y.short.as_deref()),
            tags: y.tags.clone(),
            priority: Some(y.priority).filter(|p| *p != Priority::Normal),
            works: y.works.then_some(true),
            channel_id: some_nonblank(Some(&y.id)),
            ..Publication::default()
        };
        let key = y.publication().to_owned();
        let origin = RowRef {
            table: "youtube",
            index: i,
        };
        place(&mut accs, key, row, KindFrom::Other, origin, &mut notes)?;
    }

    check_invariants(&accs)?;
    let merged: Vec<String> = accs
        .iter()
        .filter(|a| a.origins.len() > 1)
        .map(|a| {
            let tables: Vec<&str> = a.origins.iter().map(|o| o.table).collect();
            format!("{:?} ({})", a.key, tables.join("+"))
        })
        .collect();
    c.notice = (!merged.is_empty()).then(|| {
        let mut s = format!(
            "sources.toml: {} publication(s) folded from several rows: {}",
            merged.len(),
            merged.join(", ")
        );
        if !notes.is_empty() {
            s.push_str("; ");
            s.push_str(&notes.join("; "));
        }
        s.push_str(" (run `cv sources migrate`)");
        s
    });
    c.origins = accs.iter().map(|a| a.origins.clone()).collect();
    c.folded = accs.into_iter().map(|a| a.rec).collect();
    Ok(())
}

fn check_invariants(accs: &[Acc]) -> Result<(), String> {
    for (i, a) in accs.iter().enumerate() {
        let n = a.rec.name.trim();
        if n.is_empty() {
            return Err(format!("publication {:?}: needs a non-empty `name`", a.key));
        }
        // Legacy rows of different tables may share a display name under different
        // publications (a channel and a blog both named "X"); that stays loadable, and
        // `cv sources migrate` refuses to write the collision.
        let native = |a: &Acc| a.origins.iter().any(|o| o.table == "publication");
        if accs[..i]
            .iter()
            .any(|o| o.rec.name.trim() == n && (native(o) || native(a)))
        {
            return Err(format!("publication {n:?}: duplicate name"));
        }
        // A legacy serial row with neither author nor prefix was valid (it matches nothing);
        // it keeps loading, and `cv sources migrate` refuses to write it.
        let legacy_serial_only = a.origins.iter().all(|o| o.table == "serial");
        if !a.rec.has_matcher() && !legacy_serial_only {
            return Err(format!(
                "publication {n:?}: needs a feeds, from, list_id, hosts, prefix, channel_id or author matcher"
            ));
        }
    }
    type Proj = fn(&Publication) -> Vec<String>;
    let checks: [(&str, Proj); 6] = [
        ("from", |p| {
            p.from
                .iter()
                .filter_map(|s| nonblank(s))
                .map(str::to_lowercase)
                .collect()
        }),
        ("list_id", |p| {
            p.list_id
                .iter()
                .map(|l| normalize_list_id(l))
                .filter(|l| !l.is_empty())
                .collect()
        }),
        ("feeds", |p| {
            p.feeds
                .iter()
                .filter_map(|s| nonblank(s))
                .map(str::to_owned)
                .collect()
        }),
        ("hosts", |p| {
            p.hosts.iter().filter_map(|h| bare_host(h)).collect()
        }),
        ("prefix", |p| p.prefixes().map(str::to_owned).collect()),
        ("channel_id", |p| {
            p.channel_id
                .as_deref()
                .and_then(nonblank)
                .map(str::to_owned)
                .into_iter()
                .collect()
        }),
    ];
    for (field, proj) in checks {
        let mut seen: Vec<(String, usize)> = Vec::new();
        for (ai, a) in accs.iter().enumerate() {
            let mut own = proj(&a.rec);
            own.dedup();
            for v in own {
                match seen.iter().find(|(s, _)| *s == v) {
                    Some((_, other)) if *other != ai => {
                        let other = &accs[*other].rec.name;
                        return Err(format!(
                            "sources: `{field}` {v:?} belongs to both publication {other:?} and {:?}",
                            a.rec.name
                        ));
                    }
                    Some(_) => {}
                    None => seen.push((v, ai)),
                }
            }
        }
    }
    Ok(())
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
}
