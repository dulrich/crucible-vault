//! Inbound-link rewrite on rename (Obsidian's `alwaysUpdateLinks: true`, assessment §2 —
//! `docs/assessment-2026-10-13.md:22`). Byte spans come from ofm (D12); nothing is re-parsed
//! with regex.
//!
//! Crucible relies on Obsidian's `fileManager.renameFile` for this. Dropped as Obsidian-induced:
//! the folder-rename link lag ("`fileManager.renameFile` on a folder skips a note renamed in the
//! same tick") and the attachment-folder `vault.rename` workaround
//! (`https://github.com/dulrich/obsidian-crucible/blob/master/src/AGENTS.md:30-31`) — here the daemon renames each file itself and
//! rewrites from its own index, so neither constraint exists.

use std::collections::BTreeSet;

use cv_index::{LinkKind, LinkRow};
use ofm::LinkDest;

fn is_md(p: &str) -> bool {
    p.to_ascii_lowercase().ends_with(".md")
}

fn dir_of(p: &str) -> &str {
    p.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Collapses `.`/`..`/empty segments.
fn normalize(p: &str) -> String {
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

/// `target` relative to folder `dir` (both vault-relative).
fn relative(dir: &str, target: &str) -> String {
    let a: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    let b: Vec<&str> = target.split('/').collect();
    let mut k = 0;
    while k < a.len() && k + 1 < b.len() && a[k] == b[k] {
        k += 1;
    }
    let mut out = "../".repeat(a.len() - k);
    out.push_str(&b[k..].join("/"));
    out
}

/// The new wikilink target for a link to the moved file `to`: a bare basename stays a basename
/// when `unique(basename)` (it resolves uniquely to the moved file), else the vault-relative path;
/// `.md` is dropped unless the original spelled it; other extensions are kept.
pub(crate) fn new_wiki_target(orig: &str, to: &str, unique: &dyn Fn(&str) -> bool) -> String {
    let keep_md = is_md(orig.trim());
    let stem = if is_md(to) && !keep_md {
        &to[..to.len() - 3]
    } else {
        to
    };
    let base = stem.rsplit('/').next().unwrap_or(stem);
    if !orig.contains('/') && unique(base) {
        base.to_string()
    } else {
        stem.to_string()
    }
}

/// The new md-link destination: relative to the (new) source folder if the original was
/// relative, else vault-relative; `#fragment` kept verbatim. Spaces become `%20` when the original
/// was encoded — and also when it was not but sat outside `<…>`, because a bare space would end
/// the CommonMark destination and break the link.
fn new_md_dest(
    dest_raw: &str,
    path: &str,
    src_new: &str,
    from: &str,
    to: &str,
    angle: bool,
) -> String {
    let encoded = dest_raw.contains("%20");
    let frag = dest_raw.find('#').map_or("", |i| &dest_raw[i..]);
    let explicit_rel = path.starts_with("./") || path.starts_with("../");
    let n = normalize(path);
    let vault_form = !explicit_rel
        && (n.eq_ignore_ascii_case(from) || format!("{n}.md").eq_ignore_ascii_case(from));
    let target = if is_md(to) && !is_md(path) {
        &to[..to.len() - 3]
    } else {
        to
    };
    let mut p = if vault_form {
        target.to_string()
    } else {
        let r = relative(dir_of(src_new), target);
        if path.starts_with("./") && !r.starts_with("../") {
            format!("./{r}")
        } else {
            r
        }
    };
    if encoded || (!angle && p.contains(' ')) {
        p = p.replace(' ', "%20");
    }
    p.push_str(frag);
    p
}

/// New destination for a folder-relative md-link of the moved note (`None` = not relative, or
/// unchanged): recomputed from the new folder `dir_of(to)` so it still reaches `resolved`.
fn rebased_md_dest(
    dest_raw: &str,
    path: &str,
    to: &str,
    resolved: &str,
    angle: bool,
) -> Option<String> {
    let explicit_rel = path.starts_with("./") || path.starts_with("../");
    let n = normalize(path);
    let vault_form =
        n.eq_ignore_ascii_case(resolved) || format!("{n}.md").eq_ignore_ascii_case(resolved);
    if !explicit_rel && vault_form {
        return None;
    }
    let target = if is_md(resolved) && !is_md(path) {
        &resolved[..resolved.len() - 3]
    } else {
        resolved
    };
    let mut p = relative(dir_of(to), target);
    if path.starts_with("./") && !p.starts_with("../") {
        p = format!("./{p}");
    }
    if dest_raw.contains("%20") || (!angle && p.contains(' ')) {
        p = p.replace(' ', "%20");
    }
    p.push_str(dest_raw.find('#').map_or("", |i| &dest_raw[i..]));
    Some(p)
}

/// New target for an explicitly relative (`./`, `../`) wikilink of the moved note.
fn rebased_wiki_target(orig: &str, to: &str, resolved: &str) -> Option<String> {
    let t = orig.trim();
    if !(t.starts_with("./") || t.starts_with("../")) {
        return None;
    }
    let target = if is_md(resolved) && !is_md(t) {
        &resolved[..resolved.len() - 3]
    } else {
        resolved
    };
    let r = relative(dir_of(to), target);
    Some(if r.starts_with("../") {
        r
    } else {
        format!("./{r}")
    })
}

/// Rewrites the inbound links `links` (rows of one source note, as indexed) in `text` to point at
/// `to` instead of `from`. `src_new` is the source's path after the rename (differs from its
/// indexed path only for self-links in the moved note). Returns `None` when the text no longer
/// matches the index (a span no longer holds the indexed link): the caller skips the note.
/// `outgoing` (only for the moved note itself) are its folder-relative outgoing links to other
/// files, rebased onto the new folder in the same pass.
pub(crate) fn rewrite_note(
    text: &str,
    links: &[LinkRow],
    outgoing: &[LinkRow],
    src_new: &str,
    from: &str,
    to: &str,
    unique: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let note = ofm::parse(text);
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut fm_targets = BTreeSet::new();
    for row in links {
        let span = row
            .span
            .and_then(|(s, e)| Some((usize::try_from(s).ok()?, usize::try_from(e).ok()?)));
        match row.kind {
            LinkKind::Wikilink | LinkKind::Embed => {
                let (s, e) = span?;
                let embed = row.kind == LinkKind::Embed;
                let w = note
                    .wikilinks
                    .iter()
                    .find(|w| w.span.start == s && w.span.end == e && w.embed == embed)?;
                if w.target.trim().is_empty() {
                    continue;
                }
                let new = new_wiki_target(&w.target, to, unique);
                if new == w.target {
                    continue;
                }
                let pre = if embed { 3 } else { 2 };
                let rest = text.get(s + pre..e)?;
                let pos = rest.find(&w.target)?;
                let start = s + pre + pos;
                edits.push((start, start + w.target.len(), new));
            }
            LinkKind::MdLink | LinkKind::MdImage => {
                let (s, e) = span?;
                let m = note
                    .md_links
                    .iter()
                    .find(|m| m.span.start == s && m.span.end == e)?;
                let LinkDest::Local { path, .. } = &m.dest else {
                    continue;
                };
                let pos = text.get(s..e)?.rfind(&m.dest_raw)?;
                let angle = pos > 0 && text.as_bytes().get(s + pos - 1) == Some(&b'<');
                let new = new_md_dest(&m.dest_raw, path, src_new, from, to, angle);
                if new == m.dest_raw {
                    continue;
                }
                edits.push((s + pos, s + pos + m.dest_raw.len(), new));
            }
            LinkKind::FmLink => {
                fm_targets.insert(row.target.clone());
            }
        }
    }
    for row in outgoing {
        let (Some((s, e)), Some(resolved)) = (row.span, row.resolved_path.as_deref()) else {
            continue;
        };
        let (s, e) = (usize::try_from(s).ok()?, usize::try_from(e).ok()?);
        match row.kind {
            LinkKind::Wikilink | LinkKind::Embed => {
                let embed = row.kind == LinkKind::Embed;
                let w = note
                    .wikilinks
                    .iter()
                    .find(|w| w.span.start == s && w.span.end == e && w.embed == embed)?;
                let Some(new) = rebased_wiki_target(&w.target, to, resolved) else {
                    continue;
                };
                if new == w.target {
                    continue;
                }
                let pre = if embed { 3 } else { 2 };
                let pos = text.get(s + pre..e)?.find(&w.target)?;
                edits.push((s + pre + pos, s + pre + pos + w.target.len(), new));
            }
            LinkKind::MdLink | LinkKind::MdImage => {
                let m = note
                    .md_links
                    .iter()
                    .find(|m| m.span.start == s && m.span.end == e)?;
                let LinkDest::Local { path, .. } = &m.dest else {
                    continue;
                };
                let pos = text.get(s..e)?.rfind(&m.dest_raw)?;
                let angle = pos > 0 && text.as_bytes().get(s + pos - 1) == Some(&b'<');
                let Some(new) = rebased_md_dest(&m.dest_raw, path, to, resolved, angle) else {
                    continue;
                };
                if new != m.dest_raw {
                    edits.push((s + pos, s + pos + m.dest_raw.len(), new));
                }
            }
            LinkKind::FmLink => {}
        }
    }
    if !fm_targets.is_empty() {
        let (meta, body) = ofm::frontmatter::split_frontmatter(text);
        meta?;
        let fm_end = text.len() - body.len();
        let fm = &text[..fm_end];
        for t in fm_targets {
            if t.trim().is_empty() {
                continue;
            }
            let new = new_wiki_target(&t, to, unique);
            if new == t {
                continue;
            }
            let needle = format!("[[{t}");
            let mut from_i = 0;
            while let Some(i) = fm[from_i..].find(&needle) {
                let at = from_i + i;
                let end = at + needle.len();
                if matches!(fm[end..].chars().next(), Some('|' | '#' | ']')) {
                    edits.push((at + 2, end, new.clone()));
                }
                from_i = end;
            }
        }
    }
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = text.to_string();
    let mut floor = usize::MAX;
    for (s, e, rep) in edits {
        if e > floor {
            continue; // overlapping duplicate (same link listed twice)
        }
        out.replace_range(s..e, &rep);
        floor = s;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(relative("a/b", "a/c/x.md"), "../c/x.md");
        assert_eq!(relative("", "x/y.md"), "x/y.md");
        assert_eq!(relative("a", "a/x.md"), "x.md");
        assert_eq!(relative("a/b", "x.md"), "../../x.md");
    }

    #[test]
    fn wiki_targets() {
        let yes = |_: &str| true;
        let no = |_: &str| false;
        assert_eq!(new_wiki_target("B", "d/C.md", &yes), "C");
        assert_eq!(new_wiki_target("B", "d/C.md", &no), "d/C");
        assert_eq!(new_wiki_target("x/B", "d/C.md", &yes), "d/C");
        assert_eq!(new_wiki_target("B.md", "d/C.md", &yes), "C.md");
        assert_eq!(new_wiki_target("i.png", "r/j.png", &yes), "j.png");
    }

    #[test]
    fn md_dests() {
        assert_eq!(
            new_md_dest(
                "../b/My%20B.md#h",
                "../b/My B.md",
                "a/s.md",
                "b/My B.md",
                "c/New B.md",
                false
            ),
            "../c/New%20B.md#h"
        );
        assert_eq!(
            new_md_dest("b/x.md", "b/x.md", "a/s.md", "b/x.md", "c/y.md", false),
            "c/y.md"
        );
    }
}
