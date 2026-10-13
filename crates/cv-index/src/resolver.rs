//! In-memory link resolver, kept in sync with `files` by the writer.
//!
//! Order (S2, `runs/spike/core-index/FINDINGS.md` "Index design", matching Obsidian's
//! `getFirstLinkpathDest` as Crucible relies on it in `https://github.com/dulrich/obsidian-crucible/blob/master/src/search/linkGraph.ts:63-90`):
//! 1. exact vault path (the target as written, then with `.md`), case-sensitive first, then
//!    case-insensitive;
//! 2. relative to the source note's folder (same two spellings and case rule);
//! 3. unique basename (case-insensitive; filtered by path suffix when the target has folders);
//! 4. shortest path among the basename matches (ties broken by byte order, for determinism).
//!
//! An empty target (fragment-only `[[#h]]`) resolves to the source note itself. Obsidian-induced
//! part dropped: Crucible reads Obsidian's `metadataCache` (stale on startup, see its quirk
//! index); here the daemon owns resolution and keeps it current per write.

use std::collections::HashMap;

/// Path → file id maps for resolution.
#[derive(Debug, Default, Clone)]
pub struct Resolver {
    exact: HashMap<String, i64>,
    lower: HashMap<String, Vec<(String, i64)>>,
    by_base: HashMap<String, Vec<(String, i64)>>,
}

/// The case-folded basename key a file is found by (`.md` stripped for notes).
pub(crate) fn file_key(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    match base.strip_suffix(".md") {
        Some(stem) => stem.to_string(),
        None => base,
    }
}

/// The basename key a link target looks up (same folding as [`file_key`]).
pub(crate) fn target_key(target: &str) -> String {
    file_key(target.trim().trim_end_matches('/'))
}

/// Collapses `.`/`..`/empty segments (`..` above the root is dropped).
pub(crate) fn normalize(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

fn pick(entries: &[(String, i64)]) -> Option<i64> {
    entries
        .iter()
        .min_by(|a, b| a.0.len().cmp(&b.0.len()).then_with(|| a.0.cmp(&b.0)))
        .map(|(_, id)| *id)
}

fn remove_entry(map: &mut HashMap<String, Vec<(String, i64)>>, key: &str, id: i64) {
    if let Some(v) = map.get_mut(key) {
        v.retain(|(_, i)| *i != id);
        if v.is_empty() {
            map.remove(key);
        }
    }
}

impl Resolver {
    /// Registers `path` as file `id`.
    pub fn add(&mut self, id: i64, path: &str) {
        self.exact.insert(path.to_string(), id);
        self.lower
            .entry(path.to_lowercase())
            .or_default()
            .push((path.to_string(), id));
        self.by_base
            .entry(file_key(path))
            .or_default()
            .push((path.to_string(), id));
    }

    /// Forgets `path` (file `id`).
    pub fn remove(&mut self, id: i64, path: &str) {
        if self.exact.get(path) == Some(&id) {
            self.exact.remove(path);
        }
        remove_entry(&mut self.lower, &path.to_lowercase(), id);
        remove_entry(&mut self.by_base, &file_key(path), id);
    }

    /// Number of registered files.
    pub fn len(&self) -> usize {
        self.exact.len()
    }

    /// No files registered.
    pub fn is_empty(&self) -> bool {
        self.exact.is_empty()
    }

    fn by_path(&self, cand: &str) -> Option<i64> {
        if let Some(id) = self.exact.get(cand) {
            return Some(*id);
        }
        self.lower.get(&cand.to_lowercase()).and_then(|v| pick(v))
    }

    /// Resolves `target` written in the note at `src_path` (file `src_id`).
    pub fn resolve(&self, src_path: &str, src_id: i64, target: &str) -> Option<i64> {
        let t = normalize(target.trim());
        if t.is_empty() {
            return if target.trim().is_empty() {
                Some(src_id)
            } else {
                None
            };
        }
        let raw = target.trim();
        // `./x` / `../x` are explicitly note-relative: skip the vault-root lookup.
        let explicit_rel = raw.starts_with("./") || raw.starts_with("../");
        if !explicit_rel {
            for cand in [t.clone(), format!("{t}.md")] {
                if let Some(id) = self.by_path(&cand) {
                    return Some(id);
                }
            }
        }
        let dir = src_path.rsplit_once('/').map_or("", |(d, _)| d);
        if !dir.is_empty() || explicit_rel {
            let rel = normalize(&format!("{dir}/{raw}"));
            if !rel.is_empty() {
                for cand in [rel.clone(), format!("{rel}.md")] {
                    if let Some(id) = self.by_path(&cand) {
                        return Some(id);
                    }
                }
            }
        }
        let hits = self.by_base.get(&target_key(&t))?;
        if t.contains('/') {
            let lt = t.to_lowercase();
            let suffixes = [format!("/{lt}"), format!("/{lt}.md")];
            let filtered: Vec<(String, i64)> = hits
                .iter()
                .filter(|(p, _)| {
                    let lp = p.to_lowercase();
                    suffixes.iter().any(|s| lp.ends_with(s.as_str()))
                })
                .cloned()
                .collect();
            pick(&filtered)
        } else {
            pick(hits)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r() -> Resolver {
        let mut r = Resolver::default();
        for (id, p) in [
            (1, "a/Note.md"),
            (2, "b/c/Note.md"),
            (3, "Note.md"),
            (4, "a/Other.md"),
            (5, "_resources/img.png"),
            (6, "x/y/Deep.md"),
            (7, "q/Deep.md"),
            (8, "a/sub/Rel.md"),
            (9, "Rel.md"),
        ] {
            r.add(id, p);
        }
        r
    }

    #[test]
    fn exact_path_first() {
        let r = r();
        assert_eq!(r.resolve("z.md", 0, "a/Note"), Some(1));
        assert_eq!(r.resolve("z.md", 0, "b/c/Note.md"), Some(2));
        assert_eq!(r.resolve("z.md", 0, "A/NOTE"), Some(1));
    }

    #[test]
    fn exact_case_wins_over_folded() {
        let mut r = Resolver::default();
        r.add(1, "Foo.md");
        r.add(2, "foo.md");
        assert_eq!(r.resolve("z.md", 0, "foo"), Some(2));
        assert_eq!(r.resolve("z.md", 0, "Foo"), Some(1));
    }

    #[test]
    fn relative_to_source() {
        let r = r();
        assert_eq!(r.resolve("a/sub/x.md", 0, "./Rel"), Some(8));
        assert_eq!(r.resolve("a/sub/x.md", 0, "../Other"), Some(4));
        // Exact vault path beats relative.
        assert_eq!(r.resolve("a/x.md", 0, "Rel"), Some(9));
    }

    #[test]
    fn unique_basename_then_shortest() {
        let r = r();
        assert_eq!(r.resolve("z.md", 0, "Other"), Some(4));
        assert_eq!(r.resolve("z.md", 0, "img.png"), Some(5));
        assert_eq!(r.resolve("z.md", 0, "deep"), Some(7));
        assert_eq!(r.resolve("z.md", 0, "y/Deep"), Some(6));
        assert_eq!(r.resolve("z.md", 0, "nope/Deep"), None);
        assert_eq!(r.resolve("z.md", 0, "Missing"), None);
    }

    #[test]
    fn empty_target_is_self() {
        assert_eq!(r().resolve("z.md", 42, ""), Some(42));
    }

    #[test]
    fn remove_keeps_maps_in_sync() {
        let mut r = r();
        r.remove(7, "q/Deep.md");
        assert_eq!(r.resolve("z.md", 0, "Deep"), Some(6));
        r.remove(6, "x/y/Deep.md");
        assert_eq!(r.resolve("z.md", 0, "Deep"), None);
        assert_eq!(r.len(), 7);
    }
}
