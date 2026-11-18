//! The `source = "sources"` rows and the `file.serial` match set of the view host.

use std::collections::BTreeMap;

use cv_actions::pipelines::{Publication, SerialStatus, SourcesConfig};

/// One In-Works record of `sources.toml` as the Sources view lists it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Listed<'a> {
    /// The record.
    pub(super) rec: &'a Publication,
    /// Listed as a `serial` row: a work record (a prefix or an author) or one that folded in a
    /// `[[serial]]` row.
    pub(super) serial: bool,
}

/// The records of `cfg` whose resolved In-Works value (`in_works`, one per record,
/// [`cv_actions::ConfigSet::in_works`]: the record's `works`, else its kind's, else off) is true,
/// in config order.
pub(super) fn listed<'a>(cfg: &'a SourcesConfig, in_works: &[bool]) -> Vec<Listed<'a>> {
    cfg.publications()
        .iter()
        .enumerate()
        .filter(|(i, _)| in_works.get(*i).copied().unwrap_or(false))
        .map(|(i, rec)| Listed {
            rec,
            serial: rec.is_work() || cfg.was_serial(i),
        })
        .collect()
}

/// One day-folder note as the source-row counts read it.
#[derive(Default)]
struct Entry {
    authors: Vec<String>,
    source: String,
    publication: String,
    read: bool,
    created: Option<views::value::Value>,
    ctime: i64,
}

/// The `source = "sources"` records and the `file.serial` match set (plan round-11-feedback-todos
/// Decision 5; plan sources-publication-model WP-5), over the `daily/day` notes of `q`. Only the
/// [`listed`] records take part; a record is never listed twice:
/// - one row per In-Works record (config order), binding `name`, `publication`, `author` (bare;
///   null when unset), `prefix` (list), `kind` (a work record: its `kind`, else empty; others:
///   `kind`, else the row shape), `status` (a work record: its status; others `active`), `row`
///   (`serial` for a work record — a prefix or an author — or one that came from a `[[serial]]`
///   row, else blog/newsletter/youtube by shape), `entries`, `unread` (`read != true`) and
///   `last_update` (latest `created`, else file ctime; null without entries). Work records count
///   the notes `Publication::matches_note` accepts (the auto-file matcher, shared — never
///   restated as a view expression); other records count notes whose `publication` equals theirs;
/// - note path → name of the first In-Works work record (config order) matching it; a note
///   matching only a record with In Works off has none.
pub(super) fn source_data(
    q: &cv_index::Query<'_>,
    listed: &[Listed<'_>],
    day_folder: &str,
) -> Result<(Vec<views::engine::Record>, BTreeMap<String, String>), String> {
    use views::value::Value;
    let ix = |e: cv_index::Error| format!("index: {e}");
    let mut notes: BTreeMap<String, Entry> = q
        .md_files_under(day_folder)
        .map_err(ix)?
        .into_iter()
        .map(|f| {
            (
                f.path,
                Entry {
                    ctime: f.ctime,
                    ..Entry::default()
                },
            )
        })
        .collect();
    for key in ["author", "source", "publication", "read", "created"] {
        for r in q.md_props_with_key(key).map_err(ix)? {
            let Some(e) = notes.get_mut(&r.path) else {
                continue;
            };
            match key {
                "author" => e.authors.extend(cv_index::prop_texts(&r.value)),
                "source" => e.source = cv_index::prop_text(&r.value),
                "publication" => e.publication = cv_index::prop_text(&r.value).trim().to_owned(),
                "read" => e.read = r.value == serde_json::Value::Bool(true),
                _ => {
                    let v = Value::from_json(&r.value);
                    e.created = matches!(v, Value::Date(..)).then_some(v);
                }
            }
        }
    }
    // The work records set `file.serial`: the first match (config order) wins; each row counts its
    // own matches below.
    let works: Vec<&Publication> = listed.iter().filter(|l| l.serial).map(|l| l.rec).collect();
    let mut serial_of = BTreeMap::new();
    for (path, e) in &notes {
        let a: Vec<&str> = e.authors.iter().map(String::as_str).collect();
        if let Some(s) = works.iter().find(|s| s.matches_note(&a, &e.source)) {
            serial_of.insert(path.clone(), s.name.clone());
        }
    }
    let counts = |accept: &dyn Fn(&Entry) -> bool| {
        let (mut n, mut unread, mut last) = (0.0, 0.0, None::<Value>);
        for e in notes.values().filter(|e| accept(e)) {
            n += 1.0;
            if !e.read {
                unread += 1.0;
            }
            let d = e
                .created
                .clone()
                .unwrap_or_else(|| views::eval::ms_date(e.ctime));
            if last.as_ref().is_none_or(|l| {
                views::value::partial_cmp(&d, l).is_some_and(std::cmp::Ordering::is_gt)
            }) {
                last = Some(d);
            }
        }
        (n, unread, last.unwrap_or(Value::Null))
    };
    let text = |s: &str| Value::Str(s.to_owned());
    let mut out = Vec::new();
    let mut push = |row: &str,
                    name: &str,
                    publication: &str,
                    author: Option<&str>,
                    prefix: &[String],
                    kind: &str,
                    status: &str,
                    accept: &dyn Fn(&Entry) -> bool| {
        let (n, unread, last) = counts(accept);
        let mut m = BTreeMap::new();
        m.insert("row".to_owned(), text(row));
        m.insert("name".to_owned(), text(name));
        m.insert("publication".to_owned(), text(publication));
        m.insert(
            "author".to_owned(),
            author
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .map_or(Value::Null, text),
        );
        m.insert(
            "prefix".to_owned(),
            Value::list(prefix.iter().map(|p| text(p)).collect()),
        );
        m.insert("kind".to_owned(), text(kind));
        m.insert("status".to_owned(), text(status));
        m.insert("entries".to_owned(), Value::Num(n));
        m.insert("unread".to_owned(), Value::Num(unread));
        m.insert("last_update".to_owned(), last);
        out.push(std::sync::Arc::new(m));
    };
    let by_pub = |p: &str| {
        let p = p.trim().to_owned();
        move |e: &Entry| e.publication == p
    };
    for s in &works {
        let status = match s.status() {
            SerialStatus::Active => "active",
            SerialStatus::Complete => "complete",
            SerialStatus::Ignored => "ignored",
        };
        push(
            "serial",
            &s.name,
            s.publication(),
            s.author.as_deref(),
            &s.prefix,
            s.kind.as_deref().unwrap_or(""),
            status,
            &|e: &Entry| {
                let a: Vec<&str> = e.authors.iter().map(String::as_str).collect();
                s.matches_note(&a, &e.source)
            },
        );
    }
    // One row per listed record, grouped by shape as the old tables were (blog, newsletter,
    // youtube).
    fn row_of(p: &Publication) -> (&'static str, &str) {
        if p.channel_id.is_some() {
            ("youtube", "youtube")
        } else if p.feeds.is_empty() {
            ("newsletter", p.kind_or_blog())
        } else {
            ("blog", p.kind.as_deref().unwrap_or("blog"))
        }
    }
    for shape in ["blog", "newsletter", "youtube"] {
        for p in listed
            .iter()
            .filter(|l| !l.serial && row_of(l.rec).0 == shape)
            .map(|l| l.rec)
        {
            let f = by_pub(p.publication());
            let (_, kind) = row_of(p);
            push(
                shape,
                &p.name,
                p.publication(),
                None,
                &[],
                kind,
                "active",
                &f,
            );
        }
    }
    Ok((out, serial_of))
}
