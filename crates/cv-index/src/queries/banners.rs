//! Recurring-banner query (plan `intake-auto-enrich` WP-5): the image embeds of one intake note
//! that repeat across its source group and sit at the top or bottom of the body, so the enrich
//! step can fold them into the `[!removed]-` callout. Definition from the spike census
//! (`docs/spikes/auto-intake-llm/banner-census.pl`).
//!
//! * **Intake note** (banner scope): any note with a non-empty `source` property (clipped
//!   articles carry banners too; WP-3's narrower `post-id`/`message-id`/YouTube rule is for
//!   *which notes get enriched*, not which notes pool banner evidence).
//! * **Source group**: notes sharing the same non-empty `publication` property; a note without
//!   `publication` groups by the lowercase host (leading `www.` dropped) of its `source` URL,
//!   pooling only with other notes that also lack `publication`.
//! * **Recurrence key**: a remote URL with `?…`/`#…` stripped; a local file by its managed
//!   `…_MD5.<ext>` file name (content-addressed, so equal names are equal bytes even when
//!   localize filed them under per-note folders), or by its vault path when the name is not
//!   managed. The index has no content hash and none is computed here.
//! * **Banner**: an image embed (`![[x.png]]`, `![](x.png)`, or any remote `![](https://…)`)
//!   whose key occurs in at least [`MIN_NOTES`] distinct notes of the group (the note included)
//!   and which sits in the first or last [`EDGE_PERCENT`]% of the note's body lines.

use rusqlite::params;

use crate::query::Query;
use crate::{Error, Result};

/// Distinct notes (including the one asked about) a key must appear in.
pub const MIN_NOTES: i64 = 3;
/// The share of body lines, from each end, that counts as banner position.
pub const EDGE_PERCENT: usize = 15;

const IMAGE_EXTS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "webp", "avif", "svg"];

/// A note's source group.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Group {
    Publication(String),
    Host(String),
}

/// The lowercase host of an http(s) URL, `www.` dropped.
fn host_of(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let auth = rest.split(['/', '?', '#']).next()?;
    let auth = auth.rsplit('@').next()?;
    let host = auth.split(':').next()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_owned();
    (!host.is_empty()).then_some(host)
}

/// The URL without its query and fragment.
fn strip_url(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

fn is_image_ext(ext: &str) -> bool {
    IMAGE_EXTS.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

/// Whether the 0-based body line `rel` falls in the first or last edge band of `n` lines.
fn in_edge(rel: usize, n: usize) -> bool {
    let k = (n * EDGE_PERCENT).div_ceil(100).max(1);
    rel < k || rel + k >= n
}

/// How a candidate embed is keyed for the grouped count.
enum Key {
    Remote(String),
    Managed { name: String, ext: String },
    Path(String),
}

struct Candidate {
    span: (usize, usize),
    key: Key,
}

fn group_of(q: &Query<'_>, note: &str) -> Result<Option<Group>> {
    let mut publication = None;
    let mut source = None;
    for p in q.props(note)? {
        let text = crate::query::prop_text(&p.value);
        if text.trim().is_empty() {
            continue;
        }
        match p.key.as_str() {
            "publication" => publication = Some(text.trim().to_owned()),
            "source" => source = Some(text),
            _ => {}
        }
    }
    Ok(match (publication, source) {
        (Some(p), _) => Some(Group::Publication(p)),
        (None, Some(s)) => host_of(&s).map(Group::Host),
        (None, None) => None,
    })
}

/// File ids of the group's notes (a non-empty `source` each, see the module docs).
fn members(q: &Query<'_>, group: &Group) -> Result<Vec<i64>> {
    let conn = q.conn();
    let mut ids = Vec::new();
    match group {
        Group::Publication(p) => {
            let json = serde_json::to_string(p).map_err(Error::Json)?;
            let mut st = conn.prepare_cached(
                "SELECT p.file_id FROM props p WHERE p.key = 'publication' AND p.value = ?1 \
                 AND EXISTS (SELECT 1 FROM props s WHERE s.file_id = p.file_id AND s.key = 'source' \
                             AND s.value NOT IN ('\"\"', 'null'))",
            )?;
            for r in st.query_map([json], |r| r.get::<_, i64>(0))? {
                ids.push(r?);
            }
        }
        Group::Host(h) => {
            let mut st = conn.prepare_cached(
                "SELECT s.file_id, s.value FROM props s WHERE s.key = 'source' \
                 AND instr(lower(s.value), ?1) > 0 \
                 AND NOT EXISTS (SELECT 1 FROM props p WHERE p.file_id = s.file_id \
                                 AND p.key = 'publication' AND p.value NOT IN ('\"\"', 'null'))",
            )?;
            for r in st.query_map([h], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
                let (id, v) = r?;
                let text = serde_json::from_str::<serde_json::Value>(&v)
                    .map(|j| crate::query::prop_text(&j))
                    .unwrap_or_default();
                if host_of(&text).as_deref() == Some(h.as_str()) {
                    ids.push(id);
                }
            }
        }
    }
    Ok(ids)
}

/// The note's image-embed candidates: one SQL pass over its own link rows.
fn candidates(q: &Query<'_>, note: &str) -> Result<Vec<Candidate>> {
    let mut st = q.conn().prepare_cached(
        "SELECT l.kind, l.target, l.external, l.line, l.span_start, l.span_end, \
                rf.path, rf.name, rf.ext \
         FROM links l JOIN files f ON f.id = l.src_id LEFT JOIN files rf ON rf.id = l.resolved_id \
         WHERE f.path = ?1 AND l.kind IN ('embed', 'md-image') AND l.line IS NOT NULL \
         AND l.span_start IS NOT NULL AND l.span_end IS NOT NULL ORDER BY l.line, l.span_start",
    )?;
    let rows = st.query_map([note], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, bool>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, Option<String>>(8)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (kind, target, external, line, s, e, rpath, rname, rext) = row?;
        let (Ok(_), Ok(s), Ok(e)) = (
            usize::try_from(line),
            usize::try_from(s),
            usize::try_from(e),
        ) else {
            continue;
        };
        let key = if external {
            if kind != "md-image" || !target.starts_with("http") {
                continue;
            }
            Key::Remote(strip_url(&target).to_owned())
        } else {
            let (Some(path), Some(name), Some(ext)) = (rpath, rname, rext) else {
                continue; // unresolved
            };
            if !is_image_ext(&ext) {
                continue;
            }
            if cv_text::attachments::is_md5_name(&format!("{name}.{ext}")) {
                Key::Managed { name, ext }
            } else {
                Key::Path(path)
            }
        };
        out.push(Candidate { span: (s, e), key });
    }
    Ok(out)
}

/// Distinct notes among `ids` (a JSON array) holding an embed with `key`.
fn key_count(q: &Query<'_>, ids: &str, key: &Key) -> Result<i64> {
    let conn = q.conn();
    let n = match key {
        Key::Remote(url) => conn.query_row(
            "SELECT COUNT(DISTINCT l.src_id) FROM links l \
             WHERE l.src_id IN (SELECT value FROM json_each(?1)) AND l.kind = 'md-image' \
             AND l.external = 1 AND (l.target = ?2 \
                 OR substr(l.target, 1, length(?2) + 1) IN (?2 || '?', ?2 || '#'))",
            params![ids, url],
            |r| r.get(0),
        )?,
        Key::Managed { name, ext } => conn.query_row(
            "SELECT COUNT(DISTINCT l.src_id) FROM links l JOIN files rf ON rf.id = l.resolved_id \
             WHERE l.src_id IN (SELECT value FROM json_each(?1)) \
             AND l.kind IN ('embed', 'md-image') AND rf.name = ?2 AND rf.ext = ?3",
            params![ids, name, ext],
            |r| r.get(0),
        )?,
        Key::Path(path) => conn.query_row(
            "SELECT COUNT(DISTINCT l.src_id) FROM links l JOIN files rf ON rf.id = l.resolved_id \
             WHERE l.src_id IN (SELECT value FROM json_each(?1)) \
             AND l.kind IN ('embed', 'md-image') AND rf.path = ?2",
            params![ids, path],
            |r| r.get(0),
        )?,
    };
    Ok(n)
}

/// The exact embed texts in `note` (whose frontmatter-free `body` the caller holds) that are
/// recurring banners, in document order, deduplicated. The indexed spans are whole-file offsets;
/// the frontmatter length is the indexed file size minus `body`'s length. Texts are sliced from
/// `body` by the indexed byte span (wiki `raw` is faithful but md-image
/// `raw` is only the destination); a span that no longer reads as an embed (stale index) is
/// skipped. Cost: one SQL pass for the note's embeds, one grouped count per candidate key.
pub fn banner_refs(q: &Query<'_>, note: &str, body: &str) -> Result<Vec<String>> {
    let Some(group) = group_of(q, note)? else {
        return Ok(Vec::new());
    };
    let cands = candidates(q, note)?;
    if cands.is_empty() {
        return Ok(Vec::new());
    }
    let Some(fm_len) = q
        .file(note)?
        .and_then(|f| usize::try_from(f.size).ok())
        .and_then(|size| size.checked_sub(body.len()))
    else {
        return Ok(Vec::new());
    };
    let n = body.trim_end_matches('\n').matches('\n').count() + 1;
    let ids = members(q, &group)?;
    let ids = serde_json::to_string(&ids).map_err(Error::Json)?;
    let mut out: Vec<String> = Vec::new();
    for c in cands {
        let (Some(s), Some(e)) = (c.span.0.checked_sub(fm_len), c.span.1.checked_sub(fm_len))
        else {
            continue;
        };
        let Some(before) = body.get(..s) else {
            continue;
        };
        if !in_edge(before.matches('\n').count(), n) {
            continue;
        }
        let Some(raw) = body.get(s..e).filter(|r| r.starts_with("![")) else {
            continue;
        };
        if out.iter().any(|o| o == raw) || key_count(q, &ids, &c.key)? < MIN_NOTES {
            continue;
        }
        out.push(raw.to_owned());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Index;

    #[test]
    fn helpers() {
        assert_eq!(
            host_of("https://WWW.Example.com:8080/a?b").as_deref(),
            Some("example.com")
        );
        assert_eq!(host_of("ftp://x"), None);
        assert_eq!(strip_url("https://a/b.png?w=1#x"), "https://a/b.png");
        // 20 lines -> 3 per edge.
        assert!(in_edge(0, 20) && in_edge(2, 20) && !in_edge(3, 20));
        assert!(in_edge(19, 20) && in_edge(17, 20) && !in_edge(16, 20));
    }

    struct Fx(tempfile::TempDir);
    impl Fx {
        fn new() -> Self {
            let d = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(d.path().join("vault")).unwrap();
            Self(d)
        }
        fn write(&self, rel: &str, content: &str) {
            let p = self.0.path().join("vault").join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        fn index(&self) -> Index {
            Index::create(
                &self.0.path().join("vault"),
                &self.0.path().join("index.db"),
            )
            .unwrap()
        }
        fn text(&self, rel: &str) -> String {
            std::fs::read_to_string(self.0.path().join("vault").join(rel)).unwrap()
        }
    }

    /// A 20-line body; `top` / `mid` go on the first / middle line.
    fn note(fm: &str, top: &str, mid: &str) -> String {
        let mut s = format!("---\n{fm}\n---\n{top}\n");
        for i in 0..18 {
            s.push_str(if i == 9 { mid } else { "text" });
            s.push('\n');
        }
        s.push_str("end\n");
        s
    }

    fn refs(t: &Fx, idx: &Index, rel: &str) -> Vec<String> {
        let text = t.text(rel);
        let body = &text[text.find("\n---\n").unwrap() + 5..];
        banner_refs(&idx.query(), rel, body).unwrap()
    }

    const PUB: &str = "source: https://x.test/p\npublication: Pub";

    #[test]
    fn remote_banner_recurs_across_query_strings() {
        let t = Fx::new();
        for (i, q) in ["", "?w=1", "#f"].iter().enumerate() {
            t.write(
                &format!("n{i}.md"),
                &note(PUB, &format!("![](https://cdn.test/b.png{q})"), ""),
            );
        }
        let idx = t.index();
        assert_eq!(refs(&t, &idx, "n1.md"), ["![](https://cdn.test/b.png?w=1)"]);
    }

    #[test]
    fn below_threshold_middle_and_other_groups_do_not_count() {
        let t = Fx::new();
        let b = "![](https://cdn.test/b.png)";
        t.write("a.md", &note(PUB, b, ""));
        t.write("b.md", &note(PUB, b, ""));
        // A third note in a different publication does not pool.
        t.write(
            "c.md",
            &note("source: https://x.test/p\npublication: Other", b, ""),
        );
        let idx = t.index();
        assert!(refs(&t, &idx, "a.md").is_empty());
        // Third member, but the embed sits mid-body in the note asked about.
        t.write("m.md", &note(PUB, "", b));
        t.write("d.md", &note(PUB, b, ""));
        let idx = t.index();
        assert!(refs(&t, &idx, "m.md").is_empty());
        assert_eq!(refs(&t, &idx, "d.md"), [b]);
    }

    #[test]
    fn host_group_and_local_managed_names() {
        let t = Fx::new();
        for i in 0..3 {
            t.write(&format!("att/{i}/abc_MD5.png"), "png");
            t.write(
                &format!("n{i}.md"),
                &note(
                    &format!("source: https://www.blog.test/{i}"),
                    &format!("![[att/{i}/abc_MD5.png]]"),
                    "",
                ),
            );
        }
        t.write("att/plain.png", "png");
        t.write(
            "p.md",
            &note("source: https://blog.test/p", "![[att/plain.png]]", ""),
        );
        let idx = t.index();
        assert_eq!(refs(&t, &idx, "n2.md"), ["![[att/2/abc_MD5.png]]"]);
        assert!(refs(&t, &idx, "p.md").is_empty()); // unmanaged path occurs once
    }

    #[test]
    fn local_unmanaged_recurs_by_path() {
        let t = Fx::new();
        t.write("shared/logo.png", "png");
        for i in 0..3 {
            t.write(
                &format!("n{i}.md"),
                &note("source: https://s.test/x", "![[logo.png]]", ""),
            );
        }
        let idx = t.index();
        assert_eq!(refs(&t, &idx, "n0.md"), ["![[logo.png]]"]);
    }
}
