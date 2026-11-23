//! Folder-link discovery (plan folder-links-follow WP-2a, Decision 5) over synthetic vaults with
//! links to directories outside them (I1: never the live vault).

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use cv_index::{Index, ReconcileCounts, SkipCounts, SkipReason, discover, discover_listing};

/// A temp root holding `vault/`, an external tree `ext/` and the index files.
struct Fx(tempfile::TempDir);

impl Fx {
    fn new() -> Self {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("vault")).unwrap();
        std::fs::create_dir_all(d.path().join("ext")).unwrap();
        Self(d)
    }
    fn vault(&self) -> PathBuf {
        self.0.path().join("vault")
    }
    fn ext(&self) -> PathBuf {
        self.0.path().join("ext")
    }
    fn db(&self) -> PathBuf {
        self.0.path().join("index.db")
    }
    fn put(&self, p: &Path, content: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
    fn link(&self, target: &Path, rel: &str) {
        let p = self.vault().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        symlink(target, p).unwrap();
    }
}

fn paths(t: &Fx) -> Vec<String> {
    discover(&t.vault())
        .unwrap()
        .files
        .into_iter()
        .map(|f| f.path)
        .collect()
}

#[test]
fn linked_notes_are_indexed_under_their_vault_path() {
    let t = Fx::new();
    t.put(&t.vault().join("a.md"), "[[inner]]\n");
    t.put(&t.ext().join("proj/sub/inner.md"), "# inner\n");
    t.link(&t.ext().join("proj"), "links/proj");
    let idx = Index::create(&t.vault(), &t.db()).unwrap();
    let q = idx.query();
    assert!(q.file("links/proj/sub/inner.md").unwrap().is_some());
    assert_eq!(q.backlinks("links/proj/sub/inner.md").unwrap().len(), 1);
    let own = idx.ownership();
    let canon = std::fs::canonicalize(t.ext().join("proj")).unwrap();
    assert_eq!(own.owner_of(&canon), Some("links/proj"));
    let ext: Vec<&Path> = own.external_dirs().collect();
    assert_eq!(ext, vec![canon.as_path(), canon.join("sub").as_path()]);
    assert_eq!(
        own.vault_path_of(&canon.join("sub/inner.md")).as_deref(),
        Some("links/proj/sub/inner.md")
    );
}

#[test]
fn loops_aliases_and_dangling_links_are_skipped_and_counted() {
    let t = Fx::new();
    t.put(&t.vault().join("real/r.md"), "r");
    t.put(&t.ext().join("shared/s.md"), "s");
    t.put(&t.ext().join("loopy/l.md"), "l");
    // Alias of an in-vault directory: the physical path wins.
    t.link(&t.vault().join("real"), "z-alias-of-real");
    // Two aliases of one external directory: the first in sorted vault-path order wins.
    t.link(&t.ext().join("shared"), "b-shared");
    t.link(&t.ext().join("shared"), "a-shared");
    // A loop inside a linked subtree (points back at its own ancestor).
    symlink(t.ext().join("loopy"), t.ext().join("loopy/back")).unwrap();
    t.link(&t.ext().join("loopy"), "loopy");
    // A link to the vault's own parent is a loop too.
    t.link(t.vault().parent().unwrap(), "up");
    t.link(&t.ext().join("gone"), "dangling");

    let d = discover(&t.vault()).unwrap();
    assert_eq!(
        d.counts,
        SkipCounts {
            loops: 2,
            aliases: 2,
            dangling: 1,
        }
    );
    let files: Vec<&str> = d.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        files,
        vec!["a-shared/s.md", "loopy/l.md", "real/r.md"],
        "only owners are listed"
    );
    let skipped: Vec<(&str, SkipReason)> = d.ownership.skipped().collect();
    assert_eq!(
        skipped,
        vec![
            ("b-shared", SkipReason::Alias),
            ("dangling", SkipReason::Dangling),
            ("loopy/back", SkipReason::Loop),
            ("up", SkipReason::Loop),
            ("z-alias-of-real", SkipReason::Alias),
        ]
    );
    let own = &d.ownership;
    let e = own.admit("b-shared/s.md").unwrap_err();
    assert_eq!((e.link.as_str(), e.reason), ("b-shared", SkipReason::Alias));
    assert!(own.admit("b-shared").is_err());
    assert!(own.admit("dangling/x.md").is_err());
    assert!(
        own.admit("dangling").is_ok(),
        "a dangling entry may be a file link"
    );
    assert!(own.admit("a-shared/new.md").is_ok());
    assert_eq!(
        own.winner_of("b-shared/s.md").as_deref(),
        Some("a-shared/s.md")
    );
    assert_eq!(
        own.winner_of("z-alias-of-real/r.md").as_deref(),
        Some("real/r.md")
    );
    assert_eq!(own.winner_of("loopy/back/l.md"), None);
    // The index agrees and refuses the alias through its own upsert path.
    let mut idx = Index::create(&t.vault(), &t.db()).unwrap();
    assert_eq!(idx.query().file_count().unwrap(), 3);
    assert_eq!(idx.upsert_file("b-shared/s.md", b"s", 1, 1).unwrap(), None);
    assert!(idx.query().file("b-shared/s.md").unwrap().is_none());
}

#[test]
fn nested_alias_order_is_sorted_vault_path_order() {
    // `a-x` sorts before `a/sub` ('-' < '/'), so it owns ext/top/sub even though `a` is scanned
    // first.
    let t = Fx::new();
    t.put(&t.ext().join("top/sub/n.md"), "n");
    t.link(&t.ext().join("top"), "a");
    t.link(&t.ext().join("top/sub"), "a-x");
    assert_eq!(paths(&t), vec!["a-x/n.md"]);
    let d = discover(&t.vault()).unwrap();
    assert_eq!(d.ownership.admit("a/sub/n.md").unwrap_err().link, "a/sub");
}

#[test]
fn gitignore_applies_inside_linked_subtrees() {
    let t = Fx::new();
    t.put(&t.ext().join("p/keep.md"), "k");
    t.put(&t.ext().join("p/build/out.md"), "o");
    t.put(&t.ext().join("p/x.log"), "l");
    t.put(&t.ext().join("p/.gitignore"), "build/\n");
    t.put(&t.vault().join(".gitignore"), "*.log\n");
    t.link(&t.ext().join("p"), "p");
    assert_eq!(paths(&t), vec!["p/keep.md"]);
    // A listing sees the excluded files, flagged, with identical ownership.
    let l = discover_listing(&t.vault(), &|_| false).unwrap();
    let all: Vec<(&str, bool)> = l
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.excluded))
        .collect();
    assert!(all.contains(&("p/build/out.md", true)));
    assert!(all.contains(&("p/keep.md", false)));
    assert!(all.contains(&(".gitignore", true)));
}

#[test]
fn equal_stat_retarget_while_stopped_reindexes() {
    let t = Fx::new();
    t.put(&t.ext().join("one/n.md"), "# one\n[[A]]\n");
    t.put(&t.ext().join("two/n.md"), "# two\n[[B]]\n");
    let mtime = std::fs::metadata(t.ext().join("one/n.md"))
        .unwrap()
        .modified()
        .unwrap();
    std::fs::File::options()
        .write(true)
        .open(t.ext().join("two/n.md"))
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    t.link(&t.ext().join("one"), "l");
    drop(Index::create(&t.vault(), &t.db()).unwrap());
    // "Daemon stopped": retarget the link onto an equal-(size, mtime) file.
    std::fs::remove_file(t.vault().join("l")).unwrap();
    t.link(&t.ext().join("two"), "l");
    let mut idx = Index::open(&t.db()).unwrap();
    let c = idx.reconcile(&t.vault()).unwrap();
    assert_eq!(
        c,
        ReconcileCounts {
            updated: 1,
            ..ReconcileCounts::default()
        }
    );
    assert!(!idx.query().fts("two", 10).unwrap().is_empty());
    let again = idx.reconcile(&t.vault()).unwrap();
    assert_eq!(again.unchanged, 1);
}
