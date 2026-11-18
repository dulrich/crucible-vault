//! The legacy `[[blog]]`/`[[newsletter]]`/`[[serial]]`/`[[youtube]]` tables: their row types and
//! the fold that merges them with `[[publication]]` rows into one record per publication (keyed
//! by `publication_of(short, name)`), so an un-migrated file keeps loading and `cv sources
//! migrate` can rewrite it. The tables stay readable through the 1.x line and are deleted in 2.0
//! (`docs/decisions.md` D35).

use std::fmt;

use serde::Deserialize;

use super::{
    FIELD_AUTHOR, FIELD_CHANNEL_ID, FIELD_FEEDS, FIELD_FROM, FIELD_HOSTS, FIELD_LIST_ID,
    FIELD_PREFIX, LEGACY_TABLE_NAMES, Publication, bare_host, nonblank, normalize_list_id,
};
#[cfg(test)]
use crate::pipelines::author_key;
use crate::pipelines::{
    BlogCanon, BodyMode, Priority, SerialStatus, SourcesConfig, publication_of,
};

/// A `sources.toml` table a folded record's row can come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Table {
    /// `[[publication]]`.
    Publication,
    /// `[[blog]]`.
    Blog,
    /// `[[newsletter]]`.
    Newsletter,
    /// `[[serial]]`.
    Serial,
    /// `[[youtube]]`.
    Youtube,
}

impl Table {
    /// The table's name in `sources.toml`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Publication => "publication",
            Self::Youtube => LEGACY_TABLE_NAMES[0],
            Self::Blog => LEGACY_TABLE_NAMES[1],
            Self::Newsletter => LEGACY_TABLE_NAMES[2],
            Self::Serial => LEGACY_TABLE_NAMES[3],
        }
    }
}

impl fmt::Display for Table {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Where a folded record's row came from: its table and its position in that table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowRef {
    /// The table.
    pub table: Table,
    /// The row index within it.
    pub index: usize,
}

impl SourcesConfig {
    /// The rows each record of [`Self::publications`] came from.
    #[must_use]
    pub fn origins(&self) -> &[Vec<RowRef>] {
        &self.origins
    }

    /// Whether record `i` of [`Self::publications`] folded in a `[[serial]]` row.
    #[must_use]
    pub fn was_serial(&self, i: usize) -> bool {
        self.came_from(i, Table::Serial)
    }

    /// Whether record `i` of [`Self::publications`] folded in a `[[newsletter]]` row.
    #[must_use]
    pub fn was_newsletter(&self, i: usize) -> bool {
        self.came_from(i, Table::Newsletter)
    }

    fn came_from(&self, i: usize, t: Table) -> bool {
        self.origins
            .get(i)
            .is_some_and(|o| o.iter().any(|r| r.table == t))
    }

    /// Whether the file still has legacy rows (`cv sources migrate` has work to do).
    #[must_use]
    pub fn has_legacy_rows(&self) -> bool {
        !(self.blog.is_empty()
            && self.newsletter.is_empty()
            && self.serial.is_empty()
            && self.youtube.is_empty())
    }

    /// Mail tracker keys a fold renamed: `(legacy [[newsletter]] name, record name)` for every
    /// newsletter row folded into a record of another name (plan sources-publication-model WP-2).
    /// Empty once the file holds only `[[publication]]` rows.
    #[must_use]
    pub fn mail_key_renames(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (rec, origins) in self.folded.iter().zip(&self.origins) {
            for o in origins.iter().filter(|o| o.table == Table::Newsletter) {
                if let Some(n) = self.newsletter.get(o.index)
                    && n.name != rec.name
                    && !out.iter().any(|(a, _): &(String, String)| *a == n.name)
                {
                    out.push((n.name.clone(), rec.name.clone()));
                }
            }
        }
        out
    }
}

/// The legacy-row checks `SourcesConfig::from_table` runs before the fold.
pub(crate) fn validate(c: &SourcesConfig) -> Result<(), String> {
    for (i, s) in c.serial.iter().enumerate() {
        if s.name.trim().is_empty() {
            return Err(format!("serial #{}: needs a non-empty `name`", i + 1));
        }
        if c.serial[..i].iter().any(|o| o.name.trim() == s.name.trim()) {
            return Err(format!("serial {:?}: duplicate name", s.name));
        }
    }
    for n in &c.newsletter {
        if n.from.iter().all(|f| f.trim().is_empty())
            && n.list_id.as_deref().is_none_or(|l| l.trim().is_empty())
            && n.hosts.iter().all(|h| h.trim().is_empty())
        {
            return Err(format!(
                "newsletter {:?}: needs a `from`, `list_id` or `hosts` matcher",
                n.name
            ));
        }
        if let Some(b) = n.blog.as_deref().filter(|b| !b.trim().is_empty())
            && !c.blog.iter().any(|r| r.name == b)
        {
            return Err(format!(
                "newsletter {:?}: blog {b:?} names no [[blog]] row",
                n.name
            ));
        }
    }
    Ok(())
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
    merge_scalar(&mut r.author, row.author.as_ref(), &name, FIELD_AUTHOR)?;
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
        FIELD_CHANNEL_ID,
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
            if origin.table == Table::Publication
                && a.origins.iter().any(|o| o.table == Table::Publication)
            {
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
            table: Table::Publication,
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
            table: Table::Blog,
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
            table: Table::Newsletter,
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
            table: Table::Serial,
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
            table: Table::Youtube,
            index: i,
        };
        place(&mut accs, key, row, KindFrom::Other, origin, &mut notes)?;
    }

    check_invariants(&accs)?;
    let merged: Vec<String> = accs
        .iter()
        .filter(|a| a.origins.len() > 1)
        .map(|a| {
            let tables: Vec<&str> = a.origins.iter().map(|o| o.table.name()).collect();
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
        let native = |a: &Acc| a.origins.iter().any(|o| o.table == Table::Publication);
        if accs[..i]
            .iter()
            .any(|o| o.rec.name.trim() == n && (native(o) || native(a)))
        {
            return Err(format!("publication {n:?}: duplicate name"));
        }
        // A legacy serial row with neither author nor prefix was valid (it matches nothing);
        // it keeps loading, and `cv sources migrate` refuses to write it.
        let legacy_serial_only = a.origins.iter().all(|o| o.table == Table::Serial);
        if !a.rec.has_matcher() && !legacy_serial_only {
            return Err(format!(
                "publication {n:?}: needs a feeds, from, list_id, hosts, prefix, channel_id or author matcher"
            ));
        }
    }
    type Proj = fn(&Publication) -> Vec<String>;
    let checks: [(&str, Proj); 6] = [
        (FIELD_FROM, |p| {
            p.from
                .iter()
                .filter_map(|s| nonblank(s))
                .map(str::to_lowercase)
                .collect()
        }),
        (FIELD_LIST_ID, |p| {
            p.list_id
                .iter()
                .map(|l| normalize_list_id(l))
                .filter(|l| !l.is_empty())
                .collect()
        }),
        (FIELD_FEEDS, |p| {
            p.feeds
                .iter()
                .filter_map(|s| nonblank(s))
                .map(str::to_owned)
                .collect()
        }),
        (FIELD_HOSTS, |p| {
            p.hosts.iter().filter_map(|h| bare_host(h)).collect()
        }),
        (FIELD_PREFIX, |p| p.prefixes().map(str::to_owned).collect()),
        (FIELD_CHANNEL_ID, |p| {
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

/// One tracked YouTube channel.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct YoutubeSource {
    /// Display name.
    pub channel: String,
    /// Publication name shown in the Author column (default: `channel`; plan intake-round-2 WP-3).
    #[serde(default)]
    pub short: Option<String>,
    /// Channel id (`UC…`).
    pub id: String,
    /// Tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Priority.
    #[serde(default)]
    pub priority: Priority,
    /// Listed in the Works view (plan round-11-feedback-todos Decision 5).
    #[serde(default)]
    pub works: bool,
}

/// One tracked blog feed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BlogSource {
    /// Display name.
    pub name: String,
    /// Publication name shown in the Author column (default: `name`; plan intake-round-2 WP-3).
    #[serde(default)]
    pub short: Option<String>,
    /// RSS/Atom feed URL.
    pub link: String,
    /// Fetch method (`rss`; other values are skipped by the tracker, as in Crucible).
    #[serde(default = "rss")]
    pub method: String,
    /// Tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Priority.
    #[serde(default)]
    pub priority: Priority,
    /// Post-id canonicalization.
    #[serde(default)]
    pub canon: BlogCanon,
    /// Body mode.
    #[serde(default)]
    pub body: BodyMode,
    /// Body phrases (case-insensitive) marking a post incomplete (plan auto-intake-sprint WP-2).
    #[serde(default)]
    pub paywall_markers: Vec<String>,
    /// Listed in the Works view (plan round-11-feedback-todos Decision 5).
    #[serde(default)]
    pub works: bool,
}

fn rss() -> String {
    "rss".into()
}

/// One `[[newsletter]]` mail sender (plan auto-intake-sprint WP-4): matched by `from` (an exact
/// address, or `@domain` suffix) or `list_id`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NewsletterSource {
    /// Display name (also the `tracker_items.source_key` of its mails).
    pub name: String,
    /// Publication name shown in the Author column (default: `name`; plan intake-round-2 WP-3).
    #[serde(default)]
    pub short: Option<String>,
    /// Sender matchers, case-insensitive: `a@b.example` or `@b.example`.
    #[serde(default)]
    pub from: Vec<String>,
    /// `List-Id` matcher.
    #[serde(default)]
    pub list_id: Option<String>,
    /// The name of the `[[blog]]` row this newsletter mirrors (access evidence and dedup source).
    #[serde(default)]
    pub blog: Option<String>,
    /// The `kinds.toml` kind its mails are filed as (default `blog`).
    #[serde(default = "blog_kind")]
    pub kind: String,
    /// Extra tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Body phrases (case-insensitive) marking an email incomplete (united with the linked blog row's).
    #[serde(default)]
    pub paywall_markers: Vec<String>,
    /// Clip hosts (kinds.toml `hosts` forms) whose clips file as this row's `kind` with its
    /// publication (plan round-11-feedback-todos Decision 7); a hosts-only row needs no mail
    /// matcher.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Listed in the Works view (plan round-11-feedback-todos Decision 5).
    #[serde(default)]
    pub works: bool,
}

fn blog_kind() -> String {
    "blog".into()
}

/// One `[[serial]]` work (plan round-11-feedback-todos Decision 4): a clip matches it by
/// `prefix`, else by `author` ([`SerialSource::matches`]) and files as its `kind` with its
/// publication.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SerialSource {
    /// Display name (unique among serial rows).
    pub name: String,
    /// Publication name (default: `name`).
    #[serde(default)]
    pub short: Option<String>,
    /// The bare author name a clip's `author` list must contain.
    #[serde(default)]
    pub author: Option<String>,
    /// Source-URL prefixes; some non-empty one must start the clip's `source`.
    #[serde(default)]
    pub prefix: Vec<String>,
    /// The `kinds.toml` kind its clips file as (no default, plan public-release WP-4 C#30;
    /// absent or blank = never filed: `auto_file` skips the row with a warning; the Works view
    /// still lists it).
    #[serde(default)]
    pub kind: String,
    /// `active`, `complete` or `ignored`.
    #[serde(default)]
    pub status: SerialStatus,
    /// Listed in the Works view (every serial row is; accepted for uniformity, Decision 5).
    #[serde(default)]
    pub works: bool,
}

impl SerialSource {
    /// The `publication` value for this work's notes (`short`, else `name`).
    #[must_use]
    pub fn publication(&self) -> &str {
        publication_of(self.short.as_deref(), &self.name)
    }

    /// Whether a note with these `author` entries and `source` belongs to this work (plan
    /// round-12-feedback-todos Decision 7): a row with any non-blank (trimmed) prefix matches by
    /// prefix alone — some prefix must start the trimmed `source`, the author is ignored; a row
    /// with only an author matches when it equals one of the note's authors (both normalized by
    /// [`author_key`]); a row with neither matches nothing. `status` does not affect matching.
    #[cfg(test)]
    #[must_use]
    pub fn matches(&self, authors: &[&str], source: &str) -> bool {
        let want = self
            .author
            .as_deref()
            .map(author_key)
            .filter(|a| !a.is_empty());
        let prefixes: Vec<&str> = self
            .prefix
            .iter()
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect();
        if !prefixes.is_empty() {
            let source = source.trim();
            return prefixes.iter().any(|p| source.starts_with(p));
        }
        want.is_some_and(|w| authors.iter().any(|c| author_key(c) == w))
    }
}

impl YoutubeSource {
    /// The `publication` value for this channel's notes (`short`, else `channel`).
    #[must_use]
    pub fn publication(&self) -> &str {
        publication_of(self.short.as_deref(), &self.channel)
    }
}

impl BlogSource {
    /// The `publication` value for this feed's notes (`short`, else `name`).
    #[must_use]
    pub fn publication(&self) -> &str {
        publication_of(self.short.as_deref(), &self.name)
    }
}

impl NewsletterSource {
    /// The `publication` value for this sender's notes (`short`, else `name`).
    #[must_use]
    pub fn publication(&self) -> &str {
        publication_of(self.short.as_deref(), &self.name)
    }
}
