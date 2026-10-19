//! ofm `Note` → index rows. Parsing is ofm's (D12); this module only flattens its items and
//! maps byte offsets to 1-based lines.

use ofm::frontmatter::{JsValue, json_stringify};
use ofm::{LinkDest, Note, TagSource};

use crate::query::LinkKind;

/// Folders never indexed (S2: `.obsidian/`, `.git/`, `.trash/`).
const SKIPPED_DIRS: [&str; 3] = [".obsidian", ".git", ".trash"];

/// Folder whose files are link targets only, never parsed (S2 "Index design": excluding
/// `_resources/` entirely collapsed embed/md-image resolution; Obsidian's `userIgnoreFilters`,
/// read by Crucible at `https://github.com/dulrich/obsidian-crucible/blob/master/src/fileOpenIndex.ts:244`, hides files from
/// search/graph but never removes them as link targets).
const TARGET_ONLY_DIR: &str = "_resources";

/// `path` lies under a skipped folder (any depth for the first segment only, as in S2).
pub fn is_skipped_path(path: &str) -> bool {
    path.split('/')
        .next()
        .is_some_and(|first| SKIPPED_DIRS.contains(&first))
}

/// `path` is recorded as a link target but never parsed.
pub fn is_target_only_path(path: &str) -> bool {
    path.split('/').next() == Some(TARGET_ONLY_DIR)
}

/// `path` is a note the index parses (`.md`, not target-only, not skipped).
pub fn is_parsed_path(path: &str) -> bool {
    !is_skipped_path(path)
        && !is_target_only_path(path)
        && path.rsplit('/').next().is_some_and(|n| {
            n.rsplit_once('.')
                .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("md"))
        })
}

/// The tag names of a note exactly as the index records them (ofm's parse, D12: frontmatter
/// `tags` plus inline `#tags`, never inside code), in parse order. Used by the step guard's
/// `tag`/`not_tag` so guards and views agree about a note's tags (SR11).
#[must_use]
pub fn note_tags(source: &str) -> Vec<String> {
    ofm::parse(source)
        .tags
        .into_iter()
        .map(|t| t.name)
        .collect()
}

pub(crate) struct LinkIn {
    pub kind: LinkKind,
    pub raw: String,
    pub target: String,
    pub fragment: Option<String>,
    pub alias: Option<String>,
    pub external: bool,
    pub line: Option<i64>,
    pub span: Option<(i64, i64)>,
}

pub(crate) struct TaskIn {
    pub line: i64,
    pub status: char,
    pub text: String,
    pub heading: Option<(String, u8)>,
}

#[derive(Default)]
pub(crate) struct Extracted {
    pub has_fm: bool,
    /// Key → `JSON.stringify` of the `JsValue` (type preserved: `true` ≠ `"true"`).
    pub props: Vec<(String, String)>,
    pub links: Vec<LinkIn>,
    /// (tag, source `fm|inline`, line).
    pub tags: Vec<(String, &'static str, Option<i64>)>,
    /// (kind `callout|fndef|fnref|blockid`, label, line).
    pub blocks: Vec<(&'static str, String, i64)>,
    pub tasks: Vec<TaskIn>,
    pub body: String,
}

/// Byte offset → 1-based line.
struct Lines(Vec<usize>);

impl Lines {
    fn new(src: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(src.match_indices('\n').map(|(i, _)| i + 1));
        Self(starts)
    }
    fn line(&self, offset: usize) -> i64 {
        let idx = match self.0.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        i64::try_from(idx).unwrap_or(i64::MAX).saturating_add(1)
    }
}

fn off(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn wiki_raw(target: &str, fragment: Option<&str>, alias: Option<&str>, embed: bool) -> String {
    let mut s = String::from(if embed { "![[" } else { "[[" });
    s.push_str(target);
    if let Some(f) = fragment {
        s.push('#');
        s.push_str(f);
    }
    if let Some(a) = alias {
        s.push('|');
        s.push_str(a);
    }
    s.push_str("]]");
    s
}

pub(crate) fn extract(source: &str) -> Extracted {
    let note: Note = ofm::parse(source);
    let lines = Lines::new(source);
    let mut out = Extracted {
        has_fm: note.frontmatter.is_some(),
        body: source.get(note.body_offset..).unwrap_or("").to_string(),
        ..Extracted::default()
    };
    if let Some(fm) = &note.frontmatter
        && let Ok(JsValue::Obj(obj)) = &fm.value
    {
        for (k, v) in obj.entries() {
            out.props.push((k.to_string(), json_stringify(v)));
        }
    }
    for w in &note.wikilinks {
        out.links.push(LinkIn {
            kind: if w.embed {
                LinkKind::Embed
            } else {
                LinkKind::Wikilink
            },
            raw: source.get(w.span.clone()).unwrap_or("").to_string(),
            target: w.target.clone(),
            fragment: w.fragment.clone(),
            alias: w.alias.clone(),
            external: false,
            line: Some(lines.line(w.span.start)),
            span: Some((off(w.span.start), off(w.span.end))),
        });
    }
    for m in &note.md_links {
        let (target, fragment, external) = match &m.dest {
            LinkDest::External(u) => (u.clone(), None, true),
            LinkDest::Local { path, fragment } => (path.clone(), fragment.clone(), false),
        };
        out.links.push(LinkIn {
            kind: if m.image {
                LinkKind::MdImage
            } else {
                LinkKind::MdLink
            },
            raw: m.dest_raw.clone(),
            target,
            fragment,
            alias: None,
            external,
            line: Some(lines.line(m.span.start)),
            span: Some((off(m.span.start), off(m.span.end))),
        });
    }
    // Frontmatter `[[…]]` values are links too — Crucible unions `frontmatterLinks` because
    // Obsidian's `resolvedLinks` omits them (`https://github.com/dulrich/obsidian-crucible/blob/master/src/search/linkGraph.ts:63-68`);
    // here they are first-class rows. fm-links have no per-value span (ofm): line/span NULL.
    for f in &note.fm_links {
        out.links.push(LinkIn {
            kind: LinkKind::FmLink,
            raw: wiki_raw(
                &f.target,
                f.fragment.as_deref(),
                f.alias.as_deref(),
                f.embed,
            ),
            target: f.target.clone(),
            fragment: f.fragment.clone(),
            alias: f.alias.clone(),
            external: false,
            line: None,
            span: None,
        });
    }
    for t in &note.tags {
        match &t.source {
            TagSource::Frontmatter => out.tags.push((t.name.clone(), "fm", None)),
            TagSource::Inline { span } => {
                out.tags
                    .push((t.name.clone(), "inline", Some(lines.line(span.start))));
            }
        }
    }
    for c in &note.callouts {
        out.blocks
            .push(("callout", c.kind.clone(), lines.line(c.span.start)));
    }
    for f in &note.footnote_defs {
        out.blocks
            .push(("fndef", f.label.clone(), lines.line(f.span.start)));
    }
    for f in &note.footnote_refs {
        out.blocks
            .push(("fnref", f.label.clone(), lines.line(f.span.start)));
    }
    for b in &note.block_ids {
        out.blocks
            .push(("blockid", b.id.clone(), lines.line(b.span.start)));
    }
    for t in &note.tasks {
        let heading = note
            .headings
            .iter()
            .rfind(|h| h.span.start < t.span.start)
            .map(|h| (h.text.clone(), h.level));
        out.tasks.push(TaskIn {
            line: lines.line(t.span.start),
            status: t.status,
            text: t.text.clone(),
            heading,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert!(is_skipped_path(".obsidian/app.json"));
        assert!(!is_skipped_path("notes/.obsidian.md"));
        assert!(is_target_only_path("_resources/a/b.png"));
        assert!(is_parsed_path("a/b.md"));
        assert!(is_parsed_path("a/B.MD"));
        assert!(!is_parsed_path("_resources/x.md"));
        assert!(!is_parsed_path("a/b.png"));
    }

    #[test]
    fn lines_are_one_based() {
        let l = Lines::new("a\nbb\n\nc");
        assert_eq!(l.line(0), 1);
        assert_eq!(l.line(2), 2);
        assert_eq!(l.line(5), 3);
        assert_eq!(l.line(6), 4);
    }

    #[test]
    fn extracts_items() {
        let src = "---\ntags: [fm1]\nup: \"[[Parent#h|p]]\"\ndone: true\n---\n# Top\n\nSee [[Foo#Bar|baz]] and ![[img.png]] and [x](sub/y.md#z) and [w](https://e.x).\n\n## Sub\n- [ ] one #inline\n- [>] two\n\n> [!note] t\n\nText[^1] ^blk\n\n[^1]: fn\n";
        let e = extract(src);
        assert!(e.has_fm);
        assert!(e.props.contains(&("done".to_string(), "true".to_string())));
        assert!(
            e.props
                .contains(&("tags".to_string(), "[\"fm1\"]".to_string()))
        );
        let kinds: Vec<LinkKind> = e.links.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                LinkKind::Wikilink,
                LinkKind::Embed,
                LinkKind::MdLink,
                LinkKind::MdLink,
                LinkKind::FmLink
            ]
        );
        let foo = &e.links[0];
        assert_eq!(foo.target, "Foo");
        assert_eq!(foo.fragment.as_deref(), Some("Bar"));
        assert_eq!(foo.line, Some(8));
        assert_eq!(foo.raw, "[[Foo#Bar|baz]]");
        let (s, en) = foo.span.unwrap_or_default();
        assert_eq!(&src[s as usize..en as usize], "[[Foo#Bar|baz]]");
        assert_eq!(e.links[2].target, "sub/y.md");
        assert_eq!(e.links[2].fragment.as_deref(), Some("z"));
        assert!(e.links[3].external);
        assert_eq!(e.links[4].raw, "[[Parent#h|p]]");
        assert_eq!(e.links[4].line, None);
        assert!(e.tags.contains(&("fm1".to_string(), "fm", None)));
        assert!(e.tags.contains(&("inline".to_string(), "inline", Some(11))));
        assert_eq!(e.tasks.len(), 2);
        assert_eq!(e.tasks[0].line, 11);
        assert_eq!(e.tasks[0].status, ' ');
        assert_eq!(e.tasks[1].status, '>');
        assert_eq!(e.tasks[1].heading, Some(("Sub".to_string(), 2)));
        assert!(e.blocks.iter().any(|b| b.0 == "callout" && b.1 == "note"));
        assert!(e.blocks.iter().any(|b| b.0 == "blockid" && b.1 == "blk"));
        assert!(e.blocks.iter().any(|b| b.0 == "fndef" && b.1 == "1"));
        assert!(e.body.starts_with("# Top"));
    }
}
