//! Source-eval signals over the index (plan `v0-automation-pipelines` WP-10): per-note `read`,
//! tags, the human eval label and `eval-skip`, plus the monthly `# Observations` counts.
//!
//! Ported from Crucible `src/sourceEval/signals.ts` (`parseEvalLabel` `:4-15`,
//! `scanObservationSignals` `:17-79`) and `captureIndex.ts:36-48` (`read`, tags, `label`,
//! `evalSkip`). Dropped as Obsidian-induced: metadata-cache reads become index queries
//! (`props`/`tags`), and `getFirstLinkpathDest` becomes the index's resolved `links` row for the
//! bullet's line. The `# Observations` section itself is not indexed, so its text is read from
//! the vault file (read-only; the index stays derived).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{Map, Value};

use crate::Result;
use crate::query::{LinkKind, Query};

fn collect<T>(
    conn: &rusqlite::Connection,
    sql: &str,
    p: impl rusqlite::Params,
    f: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map(p, f)?;
    Ok(rows.collect::<rusqlite::Result<Vec<T>>>()?)
}

/// A human eval label (`eval-importance`, `eval-urgent`, `eval-rated`, `eval-tags`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvalLabel {
    /// `eval-importance` (0..=5, integral); `None` when unset or invalid.
    pub importance: Option<i64>,
    /// `eval-urgent === true`.
    pub urgent: bool,
    /// Trimmed non-empty `eval-rated`.
    pub rated: Option<String>,
    /// `eval-tags` (list or comma string), `#` stripped, deduplicated in order.
    pub tags: Vec<String>,
}

/// Crucible `parseEvalLabel` (`signals.ts:4-15`): `None` when every field is empty.
#[must_use]
pub fn parse_eval_label(fm: &Map<String, Value>) -> Option<EvalLabel> {
    let importance = match fm.get("eval-importance") {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|n| n.is_finite() && (0.0..=5.0).contains(n) && n.fract() == 0.0)
    .map(|n| n as i64);
    let urgent = fm.get("eval-urgent") == Some(&Value::Bool(true));
    let rated = fm
        .get("eval-rated")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let raw: Vec<&str> = match fm.get("eval-tags") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        Some(Value::String(s)) => s.split(',').collect(),
        _ => Vec::new(),
    };
    let mut tags: Vec<String> = Vec::new();
    for t in raw {
        let t = t.trim().trim_start_matches('#');
        if !t.is_empty() && !tags.iter().any(|x| x == t) {
            tags.push(t.to_owned());
        }
    }
    if importance.is_none() && !urgent && rated.is_none() && tags.is_empty() {
        return None;
    }
    Some(EvalLabel {
        importance,
        urgent,
        rated,
        tags,
    })
}

/// Per-note signals (Crucible `CaptureRecord` minus the source attribution).
#[derive(Debug, Clone, PartialEq)]
pub struct NoteSignals {
    /// Vault path.
    pub path: String,
    /// Folder (`""` = root).
    pub folder: String,
    /// Basename without extension.
    pub name: String,
    /// Frontmatter, key → JSON value.
    pub props: Map<String, Value>,
    /// Unique tags (frontmatter + inline), `#` stripped, in index order.
    pub tags: Vec<String>,
    /// `read === true`.
    pub read: bool,
    /// The parsed eval label.
    pub label: Option<EvalLabel>,
    /// `eval-skip === true` (`captureIndex.ts:47`).
    pub eval_skip: bool,
}

/// Signals of one indexed note (`None` when not indexed).
pub fn note_signals(q: &Query<'_>, path: &str) -> Result<Option<NoteSignals>> {
    let Some(file) = q.file(path)? else {
        return Ok(None);
    };
    let mut props = Map::new();
    for p in q.props(path)? {
        props.insert(p.key, p.value);
    }
    let mut tags: Vec<String> = Vec::new();
    for t in q.tags_of(path)? {
        let t = t.tag.trim().trim_start_matches('#').to_owned();
        if !t.is_empty() && !tags.contains(&t) {
            tags.push(t);
        }
    }
    let label = parse_eval_label(&props);
    Ok(Some(NoteSignals {
        path: file.path,
        folder: file.folder,
        name: file.name,
        read: props.get("read") == Some(&Value::Bool(true)),
        eval_skip: props.get("eval-skip") == Some(&Value::Bool(true)),
        props,
        tags,
        label,
    }))
}

/// Markdown notes carrying any of `keys` as a frontmatter property, by path.
pub fn notes_with_any_prop(q: &Query<'_>, keys: &[&str]) -> Result<Vec<String>> {
    let mut out = BTreeSet::new();
    for key in keys {
        let rows: Vec<String> = collect(
            q.conn(),
            "SELECT DISTINCT f.path FROM props p JOIN files f ON f.id = p.file_id WHERE p.key = ?1 AND f.is_md = 1",
            [key],
            |r| r.get(0),
        )?;
        out.extend(rows);
    }
    Ok(out.into_iter().collect())
}

/// One note's observation signal: distinct monthly notes citing it, and nested quote bullets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObservationSignal {
    /// Monthly notes whose `# Observations` section links the note.
    pub months: u32,
    /// Nested bullets under those links.
    pub quotes: u32,
}

/// Crucible `scanObservationSignals` (`signals.ts:17-79`) over every markdown note under
/// `monthly_folder` (recursive): in the level-1 `# Observations` section, a top-level bullet whose
/// first wikilink resolves to a note sets the current note (one month per monthly note); an
/// indented bullet counts one quote for it; a top-level bullet without a resolvable link clears it.
pub fn observation_signals(
    q: &Query<'_>,
    vault_root: &Path,
    monthly_folder: &str,
) -> Result<BTreeMap<String, ObservationSignal>> {
    let folder = monthly_folder.trim_matches('/');
    let files: Vec<(String, String)> = collect(
        q.conn(),
        "SELECT path, name FROM files WHERE is_md = 1 AND (?1 = '' OR folder = ?1 OR substr(folder, 1, length(?1) + 1) = ?1 || '/') ORDER BY path",
        [folder],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut by_note: BTreeMap<String, (BTreeSet<String>, u32)> = BTreeMap::new();
    for (path, name) in files {
        let Ok(text) = std::fs::read_to_string(vault_root.join(&path)) else {
            continue;
        };
        // Line (1-based) → the first resolved wikilink on it.
        let mut links: BTreeMap<i64, String> = BTreeMap::new();
        for l in q.outgoing(&path)? {
            if l.kind != LinkKind::Wikilink {
                continue;
            }
            if let (Some(line), Some(dest)) = (l.line, l.resolved_path) {
                links.entry(line).or_insert(dest);
            }
        }
        let mut in_section = false;
        let mut current: Option<String> = None;
        for (i, line) in text.lines().enumerate() {
            if let Some(h) = line.strip_prefix("# ") {
                in_section = h.trim() == "Observations";
                current = None;
                continue;
            }
            if !in_section {
                continue;
            }
            if is_nested_bullet(line) {
                if let Some(c) = &current
                    && let Some(e) = by_note.get_mut(c)
                {
                    e.1 += 1;
                }
                continue;
            }
            if !is_top_bullet(line) {
                continue;
            }
            let lineno = i64::try_from(i + 1).unwrap_or(i64::MAX);
            current = if line.contains("[[") {
                links.get(&lineno).cloned()
            } else {
                None
            };
            if let Some(c) = &current {
                by_note.entry(c.clone()).or_default().0.insert(name.clone());
            }
        }
    }
    Ok(by_note
        .into_iter()
        .map(|(k, (months, quotes))| {
            (
                k,
                ObservationSignal {
                    months: u32::try_from(months.len()).unwrap_or(u32::MAX),
                    quotes,
                },
            )
        })
        .collect())
}

/// `^(?: {2,}|\t+)[-*+]\s+`.
fn is_nested_bullet(line: &str) -> bool {
    let rest = if line.starts_with('\t') {
        line.trim_start_matches('\t')
    } else if line.starts_with("  ") {
        line.trim_start_matches(' ')
    } else {
        return false;
    };
    is_top_bullet(rest)
}

/// `^[-*+]\s+.+`.
fn is_top_bullet(line: &str) -> bool {
    let mut c = line.chars();
    matches!(c.next(), Some('-' | '*' | '+'))
        && c.next().is_some_and(char::is_whitespace)
        && !c.as_str().trim().is_empty()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::Index;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("cv-index-signals-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(p.join("vault")).unwrap();
            Self(p)
        }
        fn write(&self, rel: &str, content: &str) {
            let p = self.0.join("vault").join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fm(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn eval_label_parsing() {
        assert_eq!(parse_eval_label(&fm(json!({"title": "x"}))), None);
        assert_eq!(
            parse_eval_label(&fm(json!({"eval-urgent": "true", "eval-rated": "  "}))),
            None
        );
        let l = parse_eval_label(&fm(json!({
            "eval-importance": "3", "eval-urgent": true, "eval-rated": " 2026-10-01 ",
            "eval-tags": "#gold, revisit,gold"
        })))
        .unwrap();
        assert_eq!(l.importance, Some(3));
        assert!(l.urgent);
        assert_eq!(l.rated.as_deref(), Some("2026-10-01"));
        assert_eq!(l.tags, ["gold", "revisit"]);
        let l = parse_eval_label(&fm(json!({"eval-importance": 9, "eval-tags": ["a"]}))).unwrap();
        assert_eq!((l.importance, l.tags.len()), (None, 1));
    }

    #[test]
    fn signals_and_observations_from_two_months() {
        let t = Tmp::new();
        t.write(
            "c/a.md",
            "---\nread: true\neval-skip: true\neval-importance: 2\ntags: [gold]\n---\nbody #x\n",
        );
        t.write("c/b.md", "b\n");
        t.write(
            "m/2026-09.md",
            "---\nk: v\n---\n# Observations\n- [[a]] said\n  - quote one\n  - quote two\n- no link\n  - orphan quote\n- [[b]]\n# Other\n- [[a]]\n",
        );
        t.write("m/2026-10.md", "# Observations\n- see [[a]]\n\t- q\n");
        let idx = Index::create(&t.0.join("vault"), &t.0.join("index.db")).unwrap();
        let q = idx.query();
        let s = note_signals(&q, "c/a.md").unwrap().unwrap();
        assert!(s.read && s.eval_skip);
        assert_eq!(s.label.unwrap().importance, Some(2));
        assert!(s.tags.contains(&"gold".to_owned()) && s.tags.contains(&"x".to_owned()));
        assert_eq!(notes_with_any_prop(&q, &["read", "k"]).unwrap().len(), 2);
        let obs = observation_signals(&q, &t.0.join("vault"), "m").unwrap();
        assert_eq!(
            obs["c/a.md"],
            ObservationSignal {
                months: 2,
                quotes: 3
            }
        );
        assert_eq!(
            obs["c/b.md"],
            ObservationSignal {
                months: 1,
                quotes: 0
            }
        );
    }
}
