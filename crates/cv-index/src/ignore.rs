//! Index exclusion rules (plan editor-ui-round-3 WP-1): the vault's own `.gitignore` files plus
//! every dot-prefixed path component (which subsumes `.obsidian/`, `.git/`, `.trash/`).
//!
//! Supported gitignore subset: blank lines, `#` comments, `\#`/`\!` escapes, `!` negation, a
//! trailing `/` (directory-only), a leading or inner `/` (anchored to the `.gitignore`'s
//! directory), `*`, `?`, `**`, `[...]` classes (`!`/`^` negation, ranges), trailing-space
//! trimming. Nested files apply below their own directory; the last match wins (deeper files
//! after shallower ones); a path is excluded when any ancestor directory is excluded (git cannot
//! re-include a file whose parent directory is excluded). Only `.gitignore` files inside the
//! vault tree count — no `.git/info/exclude`, no global excludes, nothing above the vault root.

use std::path::Path;

use walkdir::WalkDir;

/// The file name whose patterns exclude paths from the index.
pub const GITIGNORE: &str = ".gitignore";

#[derive(Debug, Clone)]
struct Pattern {
    glob: Vec<u8>,
    negate: bool,
    dir_only: bool,
    anchored: bool,
}

impl Pattern {
    fn parse(line: &str) -> Option<Self> {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let line = trim_trailing_spaces(line);
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (negate, rest) = match line.strip_prefix('!') {
            Some(r) => (true, r),
            None => (false, line),
        };
        let rest = rest
            .strip_prefix("\\#")
            .map(|r| format!("#{r}"))
            .or_else(|| rest.strip_prefix("\\!").map(|r| format!("!{r}")))
            .unwrap_or_else(|| rest.to_string());
        let (dir_only, rest) = match rest.strip_suffix('/') {
            Some(r) => (true, r.to_string()),
            None => (false, rest),
        };
        let anchored = rest.contains('/');
        let glob = rest.strip_prefix('/').unwrap_or(&rest).as_bytes().to_vec();
        if glob.is_empty() {
            return None;
        }
        Some(Self {
            glob,
            negate,
            dir_only,
            anchored,
        })
    }

    /// `sub` is the path relative to the pattern's `.gitignore` directory.
    fn matches(&self, sub: &str, is_dir: bool) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }
        let target = if self.anchored {
            sub
        } else {
            sub.rsplit('/').next().unwrap_or(sub)
        };
        glob(&self.glob, target.as_bytes())
    }
}

/// Git trims trailing spaces unless the last one is backslash-escaped.
fn trim_trailing_spaces(line: &str) -> &str {
    let mut end = line.len();
    while end > 0 && line.as_bytes()[end - 1] == b' ' {
        if end >= 2 && line.as_bytes()[end - 2] == b'\\' {
            break;
        }
        end -= 1;
    }
    &line[..end]
}

/// Gitignore-style glob match of `p` against the whole of `s` (`*`/`?` never cross `/`).
fn glob(p: &[u8], s: &[u8]) -> bool {
    let Some(&c) = p.first() else {
        return s.is_empty();
    };
    match c {
        b'*' if p.get(1) == Some(&b'*') => {
            let rest = &p[2..];
            if let Some(after) = rest.strip_prefix(b"/")
                && glob(after, s)
            {
                return true;
            }
            (0..=s.len()).any(|i| glob(rest, &s[i..]))
        }
        b'*' => {
            for i in 0..=s.len() {
                if glob(&p[1..], &s[i..]) {
                    return true;
                }
                if s.get(i) == Some(&b'/') {
                    return false;
                }
            }
            false
        }
        b'?' => s.first().is_some_and(|&b| b != b'/') && glob(&p[1..], &s[1..]),
        b'[' => match class(&p[1..], s.first().copied()) {
            Some((true, used)) => glob(&p[1 + used..], &s[1..]),
            Some((false, _)) => false,
            None => s.first() == Some(&b'[') && glob(&p[1..], &s[1..]),
        },
        b'\\' if p.len() > 1 => s.first() == Some(&p[1]) && glob(&p[2..], &s[1..]),
        _ => s.first() == Some(&c) && glob(&p[1..], &s[1..]),
    }
}

/// Matches a `[...]` class body (`p` starts after `[`) against `ch`. Returns (matched, bytes of
/// `p` consumed including `]`), or `None` when the class is unterminated (then `[` is literal).
fn class(p: &[u8], ch: Option<u8>) -> Option<(bool, usize)> {
    let mut i = 0;
    let negate = matches!(p.first(), Some(b'!' | b'^'));
    if negate {
        i += 1;
    }
    let mut hit = false;
    let mut first = true;
    loop {
        let &b = p.get(i)?;
        if b == b']' && !first {
            break;
        }
        first = false;
        let lo = if b == b'\\' {
            i += 1;
            *p.get(i)?
        } else {
            b
        };
        if p.get(i + 1) == Some(&b'-') && p.get(i + 2).is_some_and(|&h| h != b']') {
            let hi = p[i + 2];
            if ch.is_some_and(|c| lo <= c && c <= hi) {
                hit = true;
            }
            i += 3;
        } else {
            if ch == Some(lo) {
                hit = true;
            }
            i += 1;
        }
    }
    let ok = ch.is_some_and(|c| c != b'/') && (hit != negate);
    Some((ok, i + 1))
}

/// The vault's exclusion rules: patterns grouped by the directory of their `.gitignore`.
#[derive(Debug, Clone, Default)]
pub struct IgnoreRules {
    /// (directory, "" for the vault root; patterns in file order), shallow before deep.
    sets: Vec<(String, Vec<Pattern>)>,
}

impl IgnoreRules {
    /// Adds the `.gitignore` of vault directory `dir` ("" = root) with `contents`.
    pub fn add(&mut self, dir: &str, contents: &str) {
        let pats: Vec<Pattern> = contents.lines().filter_map(Pattern::parse).collect();
        if pats.is_empty() {
            return;
        }
        self.sets.push((dir.to_string(), pats));
        self.sets.sort_by_key(|(d, _)| {
            if d.is_empty() {
                0
            } else {
                d.split('/').count()
            }
        });
    }

    /// Loads every `.gitignore` in the vault tree under `root`, never descending into excluded
    /// directories. Unreadable files are skipped with a warning.
    pub fn load(root: &Path) -> Self {
        let mut rules = Self::default();
        let mut it = WalkDir::new(root).follow_links(false).into_iter();
        while let Some(entry) = it.next() {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_dir() {
                continue;
            }
            let Some(rel) = rel_of(root, entry.path()) else {
                it.skip_current_dir();
                continue;
            };
            if !rel.is_empty() && rules.is_excluded(&rel, true) {
                it.skip_current_dir();
                continue;
            }
            let file = entry.path().join(GITIGNORE);
            match std::fs::read_to_string(&file) {
                Ok(text) => rules.add(&rel, &text),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(error = %e, "reading a vault .gitignore"),
            }
        }
        rules
    }

    /// Whether vault path `path` (a directory when `is_dir`) is excluded from the index: a
    /// dot-prefixed component, or an excluded ancestor directory, or a last-matching pattern.
    pub fn is_excluded(&self, path: &str, is_dir: bool) -> bool {
        if path.split('/').any(|s| s.starts_with('.')) {
            return true;
        }
        for (i, b) in path.bytes().enumerate() {
            if b == b'/' && self.matched(&path[..i], true) {
                return true;
            }
        }
        self.matched(path, is_dir)
    }

    /// Last-match-wins over the patterns of every `.gitignore` above `path`.
    fn matched(&self, path: &str, is_dir: bool) -> bool {
        let mut excluded = false;
        for (dir, pats) in &self.sets {
            let sub = if dir.is_empty() {
                path
            } else {
                match path
                    .strip_prefix(dir.as_str())
                    .and_then(|r| r.strip_prefix('/'))
                {
                    Some(r) => r,
                    None => continue,
                }
            };
            for p in pats {
                if p.matches(sub, is_dir) {
                    excluded = !p.negate;
                }
            }
        }
        excluded
    }
}

/// `path` is a `.gitignore` file (at any depth).
pub fn is_gitignore_path(path: &str) -> bool {
    path.rsplit('/').next() == Some(GITIGNORE)
}

fn rel_of(root: &Path, p: &Path) -> Option<String> {
    let rel = p.strip_prefix(root).ok()?;
    let s = rel.to_str()?;
    Some(s.replace(std::path::MAIN_SEPARATOR, "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(files: &[(&str, &str)]) -> IgnoreRules {
        let mut r = IgnoreRules::default();
        for (d, c) in files {
            r.add(d, c);
        }
        r
    }

    #[test]
    fn matcher_table() {
        let r = rules(&[(
            "",
            "# comment\n\n*.log\n!keep.log\n/root-only.md\nbuild/\ndocs/*.tmp\n**/cache\nfoo/**\na/**/z.md\n[ab]x?.md\n\\#hash.md\n\\!bang.md\ntrail.md   \n",
        )]);
        let cases: &[(&str, bool, bool)] = &[
            ("x.log", false, true),
            ("deep/x.log", false, true),
            ("keep.log", false, false),
            ("deep/keep.log", false, false),
            ("root-only.md", false, true),
            ("sub/root-only.md", false, false),
            ("build", true, true),
            ("build", false, false),
            ("sub/build", true, true),
            ("sub/build/x.md", false, true),
            ("docs/a.tmp", false, true),
            ("docs/sub/a.tmp", false, false),
            ("sub/docs/a.tmp", false, false),
            ("cache", true, true),
            ("x/y/cache", false, true),
            ("foo/a/b.md", false, true),
            ("foo", true, false),
            ("a/z.md", false, true),
            ("a/b/c/z.md", false, true),
            ("ax1.md", false, true),
            ("cx1.md", false, false),
            ("#hash.md", false, true),
            ("!bang.md", false, true),
            ("trail.md", false, true),
            ("comment", false, false),
            ("notes/n.md", false, false),
        ];
        for &(p, d, want) in cases {
            assert_eq!(r.is_excluded(p, d), want, "{p} dir={d}");
        }
    }

    #[test]
    fn nested_override_and_parent_rule() {
        let r = rules(&[
            ("", "*.md\n!sub/\n!sub/**\nout/\n"),
            ("sub", "!*.md\nsecret.md\n"),
            ("out", "!*\n"),
        ]);
        assert!(r.is_excluded("top.md", false));
        assert!(!r.is_excluded("sub/a.md", false));
        assert!(r.is_excluded("sub/secret.md", false));
        // A parent directory excluded: a nested negation cannot re-include.
        assert!(r.is_excluded("out/x.png", false));
        assert!(r.is_excluded("out", true));
    }

    #[test]
    fn negation_of_parent_dir_pattern_fails() {
        let r = rules(&[("", "dir/\n!dir/keep.md\n")]);
        assert!(r.is_excluded("dir/keep.md", false));
    }

    #[test]
    fn dot_components_always_excluded() {
        let r = IgnoreRules::default();
        for p in [
            ".obsidian/app.json",
            ".git/HEAD",
            ".trash/x.md",
            "a/.hidden/b.md",
            ".gitignore",
            "a/.env",
        ] {
            assert!(r.is_excluded(p, false), "{p}");
        }
        assert!(!r.is_excluded("a/b.md", false));
        assert!(is_gitignore_path("a/.gitignore"));
        assert!(!is_gitignore_path("a/x.gitignore"));
    }

    #[test]
    fn classes_and_escapes() {
        assert!(glob(b"[!a]b", b"cb"));
        assert!(!glob(b"[!a]b", b"ab"));
        assert!(glob(b"[a-c]", b"b"));
        assert!(!glob(b"[a-c]", b"d"));
        assert!(glob(b"[x", b"[x"));
        assert!(!glob(b"a?b", b"a/b"));
        assert!(!glob(b"a*b", b"a/b"));
        assert!(glob(b"a**b", b"a/x/b"));
    }
}
