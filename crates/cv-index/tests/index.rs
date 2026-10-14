//! Integration tests over synthetic vaults in temp dirs (I1: never the live vault).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use cv_index::{
    Error, Index, LinkKind, Reader, SCHEMA_VERSION, current_generation, generation_path,
};
use serde_json::json;

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "cv-index-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join("vault")).unwrap();
        Self(p)
    }
    fn vault(&self) -> PathBuf {
        self.0.join("vault")
    }
    fn db(&self) -> PathBuf {
        self.0.join("index.db")
    }
    fn write(&self, rel: &str, content: &str) {
        let p = self.vault().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const HUB: &str = "---\ntitle: Hub\ndone: true\ndone_str: \"true\"\ncount: 1\ncount_str: \"1\"\ntags: [project]\nup: \"[[Parent]]\"\n---\n# Hub\n\nLinks: [[Alpha]] and [[notes/Beta#Sec|b]] and ![[pic.png]] and [[Missing]] and [[#Hub]].\nAn image ![x](../_resources/pic.png) and [ext](https://example.org).\n\n## Todo\n- [ ] open task #work\n- [x] done task\n- [>] deferred task\n";

fn fixture(t: &Tmp) {
    t.write("Hub.md", HUB);
    t.write(
        "Alpha.md",
        "# Alpha\nlorem zebra\n- [ ] top-level task without heading\n",
    );
    t.write("notes/Beta.md", "# Sec\nBack to [[Hub]].\n");
    t.write("Parent.md", "parent\n");
    t.write("_resources/pic.png", "\u{89}PNG");
    t.write("_resources/ignored.md", "[[Alpha]] should never be parsed");
    t.write(".obsidian/app.md", "[[Alpha]]");
    t.write(".trash/old.md", "[[Alpha]]");
}

#[test]
fn rebuild_indexes_the_synthetic_vault() {
    let t = Tmp::new();
    fixture(&t);
    let idx = Index::create(&t.vault(), &t.db()).unwrap();
    let q = idx.query();
    assert_eq!(idx.generation().unwrap(), 1);
    // .obsidian/.trash skipped; _resources recorded (both files) but not parsed.
    assert_eq!(q.file_count().unwrap(), 6);
    assert!(q.file(".obsidian/app.md").unwrap().is_none());
    assert!(q.outgoing("_resources/ignored.md").unwrap().is_empty());
    let hub = q.file("Hub.md").unwrap().unwrap();
    assert!(hub.has_fm && hub.is_md);
    assert!(hub.ctime > 0);

    let out = q.outgoing("Hub.md").unwrap();
    let by_target = |t: &str| out.iter().find(|l| l.target == t).unwrap().clone();
    assert_eq!(
        by_target("Alpha").resolved_path.as_deref(),
        Some("Alpha.md")
    );
    let beta = by_target("notes/Beta");
    assert_eq!(beta.resolved_path.as_deref(), Some("notes/Beta.md"));
    assert_eq!(beta.fragment.as_deref(), Some("Sec"));
    assert_eq!(beta.alias.as_deref(), Some("b"));
    assert_eq!(beta.line, Some(12));
    let (s, e) = beta.span.unwrap();
    assert_eq!(&HUB[s as usize..e as usize], "[[notes/Beta#Sec|b]]");
    assert_eq!(beta.raw, "[[notes/Beta#Sec|b]]");
    let embed = by_target("pic.png");
    assert_eq!(embed.kind, LinkKind::Embed);
    assert_eq!(embed.resolved_path.as_deref(), Some("_resources/pic.png"));
    let img = by_target("../_resources/pic.png");
    assert_eq!(img.kind, LinkKind::MdImage);
    assert_eq!(img.resolved_path.as_deref(), Some("_resources/pic.png"));
    assert_eq!(by_target("Missing").resolved_path, None);
    let selfl = out.iter().find(|l| l.target.is_empty()).unwrap();
    assert_eq!(selfl.resolved_path.as_deref(), Some("Hub.md"));
    assert_eq!(selfl.fragment.as_deref(), Some("Hub"));
    let fm = by_target("Parent");
    assert_eq!(fm.kind, LinkKind::FmLink);
    assert_eq!(fm.resolved_path.as_deref(), Some("Parent.md"));
    assert_eq!((fm.line, fm.span), (None, None));
    let ext = by_target("https://example.org");
    assert!(ext.external && ext.resolved_path.is_none());

    // Backlinks with lines.
    let back = q.backlinks("Hub.md").unwrap();
    assert_eq!(back.len(), 2);
    assert_eq!(back[0].src_path, "Hub.md");
    assert_eq!(back[1].src_path, "notes/Beta.md");
    assert_eq!(back[1].line, Some(2));
    assert_eq!(q.backlinks("_resources/pic.png").unwrap().len(), 2);

    // Unresolved: missing notes only, or including external.
    let un = q.unresolved(false).unwrap();
    assert_eq!(un.len(), 1);
    assert_eq!(un[0].target, "Missing");
    assert_eq!(q.unresolved(true).unwrap().len(), 2);

    // Props keep JSON type.
    let props = q.props("Hub.md").unwrap();
    let get = |k: &str| props.iter().find(|p| p.key == k).unwrap().value.clone();
    assert_eq!(get("done"), json!(true));
    assert_eq!(get("done_str"), json!("true"));
    assert_eq!(get("count"), json!(1));
    assert_eq!(get("count_str"), json!("1"));
    assert_eq!(get("tags"), json!(["project"]));
    assert_eq!(q.files_with_prop("done", &json!(true)).unwrap().len(), 1);
    assert!(
        q.files_with_prop("done", &json!("true"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        q.files_with_prop("count_str", &json!("1")).unwrap().len(),
        1
    );
    assert!(
        q.files_with_prop("count_str", &json!(1))
            .unwrap()
            .is_empty()
    );

    // Tags: frontmatter and inline.
    let tags = q.tags_of("Hub.md").unwrap();
    assert!(
        tags.iter()
            .any(|t| t.tag == "project" && t.source == "fm" && t.line.is_none())
    );
    assert!(
        tags.iter()
            .any(|t| t.tag == "work" && t.source == "inline" && t.line == Some(16))
    );
    assert_eq!(q.tagged("work").unwrap().len(), 1);
    assert_eq!(q.tags().unwrap().len(), 2);

    // Tasks: any status char, nearest heading.
    let tasks = q.tasks(Some("Hub.md"), None).unwrap();
    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks[0].line, 16);
    assert_eq!(tasks[0].status, ' ');
    assert_eq!(tasks[0].text, "open task #work");
    assert_eq!(tasks[0].heading.as_deref(), Some("Todo"));
    assert_eq!(tasks[0].heading_level, Some(2));
    assert_eq!(tasks[2].status, '>');
    let alpha = q.tasks(Some("Alpha.md"), None).unwrap();
    assert_eq!(alpha[0].heading.as_deref(), Some("Alpha"));
    assert_eq!(q.tasks(None, Some('x')).unwrap().len(), 1);

    // Folders.
    let root: Vec<String> = q
        .folder_files("")
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(root, ["Alpha.md", "Hub.md", "Parent.md"]);
    assert_eq!(q.subfolders("").unwrap(), ["_resources", "notes"]);

    // FTS over bodies.
    let hits = q.fts("zebra", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "Alpha.md");
    assert!(q.fts("should", 10).unwrap().is_empty());
}

/// Names in the test dir other than `vault`, sorted.
fn listing(t: &Tmp) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(&t.0)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "vault")
        .collect();
    v.sort();
    v
}

#[test]
fn rebuild_swaps_atomically_and_bumps_generation() {
    let t = Tmp::new();
    fixture(&t);
    let mut idx = Index::create(&t.vault(), &t.db()).unwrap();
    assert_eq!(idx.db_path(), generation_path(&t.db(), 1));
    assert_eq!(idx.base_path(), t.db());
    assert_eq!(idx.query().journal_mode().unwrap(), "wal");
    let mut old = Reader::open(&t.db(), 1).unwrap();
    assert_eq!(old.query().generation().unwrap(), 1);
    assert_eq!(old.query().journal_mode().unwrap(), "wal");

    t.write("New.md", "fresh [[Alpha]]\n");
    std::fs::remove_file(t.vault().join("Parent.md")).unwrap();
    assert_eq!(idx.rebuild(&t.vault()).unwrap(), 2);
    assert_eq!(idx.generation().unwrap(), 2);
    assert_eq!(idx.query().generation().unwrap(), 2);

    // The old reader still sees generation 1, consistently.
    let (g, n, parent, new) = old
        .snapshot(|q| {
            Ok((
                q.generation()?,
                q.file_count()?,
                q.file("Parent.md")?.is_some(),
                q.file("New.md")?.is_some(),
            ))
        })
        .unwrap();
    assert_eq!((g, n, parent, new), (1, 6, true, false));

    let fresh = Reader::open_current(&t.db()).unwrap();
    assert_eq!(fresh.generation(), 2);
    let q = fresh.query();
    assert_eq!(q.generation().unwrap(), 2);
    assert!(q.file("Parent.md").unwrap().is_none());
    assert_eq!(q.backlinks("Alpha.md").unwrap().len(), 2);

    // Generation survives a reopen and further rebuilds/creates.
    drop(idx);
    let mut idx = Index::open(&t.db()).unwrap();
    assert_eq!(idx.generation().unwrap(), 2);
    assert_eq!(idx.rebuild(&t.vault()).unwrap(), 3);
    drop(idx);
    assert_eq!(
        Index::create(&t.vault(), &t.db())
            .unwrap()
            .generation()
            .unwrap(),
        4
    );
    assert!(!t.db().exists());
}

#[test]
fn writer_never_waits_on_a_reader() {
    let t = Tmp::new();
    fixture(&t);
    let mut idx = Index::create(&t.vault(), &t.db()).unwrap();
    let mut r = Reader::open(&t.db(), 1).unwrap();
    let elapsed = r
        .snapshot(|q| {
            assert_eq!(q.file_count()?, 6);
            let start = std::time::Instant::now();
            for i in 0..20 {
                let p = format!("w/N{i}.md");
                assert!(idx.upsert_file(&p, b"x [[Alpha]]\n", 1, 1)?.is_some());
            }
            for i in 0..10 {
                assert!(idx.remove_file(&format!("w/N{i}.md"))?);
            }
            assert!(idx.remove_file("Parent.md")?);
            let elapsed = start.elapsed();
            // The held read transaction still sees its pre-write snapshot.
            assert_eq!(q.file_count()?, 6);
            assert!(q.file("Parent.md")?.is_some());
            Ok(elapsed)
        })
        .unwrap();
    eprintln!("writer sequence under a held reader: {elapsed:?}");
    assert!(elapsed < std::time::Duration::from_secs(1), "{elapsed:?}");
    // A new read transaction sees the writes.
    assert_eq!(r.query().file_count().unwrap(), 6 + 10 - 1);
    assert_eq!(idx.query().file_count().unwrap(), 15);
}

#[test]
fn reader_mid_read_across_two_swaps() {
    let t = Tmp::new();
    fixture(&t);
    let mut idx = Index::create(&t.vault(), &t.db()).unwrap();
    let mut r = Reader::open(&t.db(), 1).unwrap();
    r.snapshot(|q| {
        let before = (q.file_count()?, q.backlinks("Alpha.md")?.len());
        t.write("New.md", "[[Alpha]]\n");
        std::fs::remove_file(t.vault().join("Parent.md")).unwrap();
        assert_eq!(idx.rebuild(&t.vault())?, 2);
        assert_eq!(idx.rebuild(&t.vault())?, 3);
        assert!(!generation_path(&t.db(), 1).exists());
        assert_eq!(q.generation()?, 1);
        assert_eq!((q.file_count()?, q.backlinks("Alpha.md")?.len()), before);
        assert!(q.file("Parent.md")?.is_some());
        assert!(q.file("New.md")?.is_none());
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        Reader::open(&t.db(), 1),
        Err(Error::GenerationMissing { generation: 1, .. })
    ));
    let cur = Reader::open_current(&t.db()).unwrap();
    assert_eq!(cur.generation(), 3);
    assert_eq!(cur.query().file_count().unwrap(), 6);
}

#[test]
fn no_stale_wal_pairing_or_leftovers() {
    let t = Tmp::new();
    fixture(&t);
    let mut idx = Index::create(&t.vault(), &t.db()).unwrap();
    for g in 2..=4 {
        assert_eq!(idx.rebuild(&t.vault()).unwrap(), g);
    }
    let allowed = |n: &str| {
        ["index-3.db", "index-4.db"]
            .iter()
            .any(|m| n == *m || n == format!("{m}-wal") || n == format!("{m}-shm"))
    };
    let names = listing(&t);
    assert!(names.iter().all(|n| allowed(n)), "{names:?}");
    assert!(names.contains(&"index-3.db".into()) && names.contains(&"index-4.db".into()));
    assert!(!t.db().exists());
    drop(idx);

    // Planted stale build + orphan WAL are swept by open; the next build numbers past them.
    std::fs::write(t.0.join("index-9.db.building"), b"junk").unwrap();
    std::fs::write(t.0.join("index-8.db-wal"), b"junk").unwrap();
    let mut idx = Index::open(&t.db()).unwrap();
    assert_eq!(idx.generation().unwrap(), 4);
    let names = listing(&t);
    assert!(names.iter().all(|n| allowed(n)), "{names:?}");
    assert_eq!(current_generation(&t.db()).unwrap(), Some(4));
    // Plant again: the build must still number past every generation-shaped file.
    std::fs::write(t.0.join("index-9.db.building"), b"junk").unwrap();
    assert_eq!(idx.rebuild(&t.vault()).unwrap(), 10);
    let names = listing(&t);
    assert!(!names.iter().any(|n| n.contains(".building")), "{names:?}");
    assert!(names.contains(&"index-10.db".into()));
}

#[test]
fn failed_build_keeps_current_generation_live() {
    let t = Tmp::new();
    fixture(&t);
    let mut idx = Index::create(&t.vault(), &t.db()).unwrap();
    let missing = t.0.join("no-such-vault");
    assert!(matches!(idx.rebuild(&missing), Err(Error::Io { .. })));
    assert_eq!(idx.generation().unwrap(), 1);
    assert_eq!(idx.query().file_count().unwrap(), 6);
    idx.upsert_file("After.md", b"ok\n", 1, 1).unwrap();
    let r = Reader::open_current(&t.db()).unwrap();
    assert_eq!(r.generation(), 1);
    assert_eq!(r.query().file_count().unwrap(), 7);
    let names = listing(&t);
    assert!(!names.iter().any(|n| n.contains(".building")), "{names:?}");
    assert!(matches!(
        Index::create(&missing, &t.db()),
        Err(Error::Io { .. })
    ));
    assert_eq!(current_generation(&t.db()).unwrap(), Some(1));
}

#[test]
fn schema_mismatch_is_refused() {
    let t = Tmp::new();
    fixture(&t);
    drop(Index::create(&t.vault(), &t.db()).unwrap());
    {
        let c = rusqlite::Connection::open(generation_path(&t.db(), 1)).unwrap();
        c.execute(
            "UPDATE meta SET value = '999' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
    }
    match Reader::open(&t.db(), 1) {
        Err(Error::SchemaMismatch { found, expected }) => {
            assert_eq!(found, Some(999));
            assert_eq!(expected, SCHEMA_VERSION);
        }
        other => panic!("expected mismatch, got {:?}", other.err()),
    }
    assert!(matches!(
        Index::open(&t.db()),
        Err(Error::SchemaMismatch { .. })
    ));
    assert_eq!(current_generation(&t.db()).unwrap(), None);
    assert!(matches!(
        Reader::open_current(&t.db()),
        Err(Error::NoIndex(_))
    ));
    // Rebuild (D14) replaces it with generation 2 and retires the stale file.
    let idx = Index::create(&t.vault(), &t.db()).unwrap();
    assert_eq!(idx.generation().unwrap(), 2);
    assert!(!generation_path(&t.db(), 1).exists());

    let empty = t.0.join("empty.db");
    drop(rusqlite::Connection::open(generation_path(&empty, 1)).unwrap());
    assert!(matches!(
        Reader::open(&empty, 1),
        Err(Error::SchemaMismatch { found: None, .. })
    ));
    assert!(matches!(
        Index::open(&t.0.join("none.db")),
        Err(Error::NoIndex(_))
    ));
}

#[test]
fn generation_mismatch_is_refused() {
    let t = Tmp::new();
    fixture(&t);
    drop(Index::create(&t.vault(), &t.db()).unwrap());
    std::fs::copy(generation_path(&t.db(), 1), generation_path(&t.db(), 5)).unwrap();
    assert!(matches!(
        Reader::open(&t.db(), 5),
        Err(Error::GenerationMismatch { file: 5, stored: 1 })
    ));
    assert_eq!(current_generation(&t.db()).unwrap(), Some(1));
}

fn sync_check(idx: &Index) {
    assert_eq!(
        idx.resolver().len() as i64,
        idx.query().file_count().unwrap(),
        "resolver out of sync with files"
    );
}

fn resolved(idx: &Index, src: &str, target: &str) -> Option<String> {
    idx.query()
        .outgoing(src)
        .unwrap()
        .into_iter()
        .find(|l| l.target == target)
        .unwrap()
        .resolved_path
}

#[test]
fn incremental_create_modify_delete_rename() {
    let t = Tmp::new();
    fixture(&t);
    let mut idx = Index::create(&t.vault(), &t.db()).unwrap();
    sync_check(&idx);
    assert_eq!(resolved(&idx, "Hub.md", "Missing"), None);

    // Create: a NULL link re-resolves.
    let id = idx
        .upsert_file("deep/Missing.md", b"# M\nnow here #fresh\n", 10, 5)
        .unwrap()
        .unwrap();
    sync_check(&idx);
    assert_eq!(
        resolved(&idx, "Hub.md", "Missing").as_deref(),
        Some("deep/Missing.md")
    );
    assert!(idx.query().unresolved(false).unwrap().is_empty());
    let f = idx.query().file("deep/Missing.md").unwrap().unwrap();
    assert_eq!((f.mtime, f.ctime, f.size), (10, 5, 20));

    // A shorter-path namesake created later takes over (S2 shortest-path rule).
    idx.upsert_file("Missing.md", b"root\n", 1, 1).unwrap();
    assert_eq!(
        resolved(&idx, "Hub.md", "Missing").as_deref(),
        Some("Missing.md")
    );

    // Modify: same id, rows replaced.
    let id2 = idx
        .upsert_file("deep/Missing.md", b"changed [[Alpha]] #other\n", 20, 5)
        .unwrap()
        .unwrap();
    assert_eq!(id, id2);
    let tags: Vec<String> = idx
        .query()
        .tags_of("deep/Missing.md")
        .unwrap()
        .into_iter()
        .map(|t| t.tag)
        .collect();
    assert_eq!(tags, ["other"]);
    assert_eq!(idx.query().fts("fresh", 5).unwrap().len(), 0);
    assert_eq!(
        idx.query().fts("changed", 5).unwrap()[0].path,
        "deep/Missing.md"
    );
    sync_check(&idx);

    // Delete: inbound links fall back to the remaining candidate, then to NULL.
    assert!(idx.remove_file("Missing.md").unwrap());
    sync_check(&idx);
    assert_eq!(
        resolved(&idx, "Hub.md", "Missing").as_deref(),
        Some("deep/Missing.md")
    );
    assert!(idx.remove_file("deep/Missing.md").unwrap());
    sync_check(&idx);
    assert_eq!(resolved(&idx, "Hub.md", "Missing"), None);
    assert!(idx.query().fts("changed", 5).unwrap().is_empty());
    assert!(!idx.remove_file("deep/Missing.md").unwrap());
    // Outgoing links of a deleted note are gone with it.
    assert_eq!(idx.query().backlinks("Alpha.md").unwrap().len(), 1);

    // Rename: inbound by-name links re-resolve; the id is kept.
    let alpha_id = idx.query().file("Alpha.md").unwrap().unwrap().id;
    let rid = idx.rename_file("Alpha.md", "archive/Alpha.md").unwrap();
    assert_eq!(rid, alpha_id);
    sync_check(&idx);
    assert_eq!(
        resolved(&idx, "Hub.md", "Alpha").as_deref(),
        Some("archive/Alpha.md")
    );
    // Renamed to a different basename: the old-name link goes NULL, a matching NULL resolves.
    idx.rename_file("archive/Alpha.md", "archive/Missing.md")
        .unwrap();
    sync_check(&idx);
    assert_eq!(resolved(&idx, "Hub.md", "Alpha"), None);
    assert_eq!(
        resolved(&idx, "Hub.md", "Missing").as_deref(),
        Some("archive/Missing.md")
    );
    // The moved note's relative links re-resolve from its new folder.
    idx.upsert_file("a/Rel.md", b"r\n", 1, 1).unwrap();
    idx.upsert_file("a/Src.md", b"[r](./Rel.md)\n", 1, 1)
        .unwrap();
    assert_eq!(
        resolved(&idx, "a/Src.md", "./Rel.md").as_deref(),
        Some("a/Rel.md")
    );
    idx.upsert_file("b/Rel.md", b"r\n", 1, 1).unwrap();
    assert_eq!(
        resolved(&idx, "a/Src.md", "./Rel.md").as_deref(),
        Some("a/Rel.md")
    );
    idx.rename_file("a/Src.md", "b/Src.md").unwrap();
    assert_eq!(
        resolved(&idx, "b/Src.md", "./Rel.md").as_deref(),
        Some("b/Rel.md")
    );

    assert!(matches!(
        idx.rename_file("Hub.md", "Parent.md"),
        Err(Error::AlreadyExists(_))
    ));
    assert!(matches!(
        idx.rename_file("nope.md", "x.md"),
        Err(Error::NotFound(_))
    ));
    sync_check(&idx);
}

#[test]
fn upsert_edge_cases() {
    let t = Tmp::new();
    let mut idx = Index::create(&t.vault(), &t.db()).unwrap();
    assert_eq!(idx.query().file_count().unwrap(), 0);
    assert_eq!(
        idx.upsert_file(".obsidian/x.md", b"[[a]]", 0, 0).unwrap(),
        None
    );
    for bad in ["", "/abs.md", "a/../b.md", "a//b.md", "a\\b.md"] {
        assert!(
            matches!(idx.upsert_file(bad, b"", 0, 0), Err(Error::InvalidPath(_))),
            "{bad}"
        );
    }
    // Attachments and _resources notes are targets only.
    idx.upsert_file("_resources/n.md", b"[[ghost]] #t", 0, 0)
        .unwrap();
    assert!(idx.query().outgoing("_resources/n.md").unwrap().is_empty());
    assert!(!idx.query().file("_resources/n.md").unwrap().unwrap().is_md);
    // Invalid UTF-8 is recorded without content.
    idx.upsert_file("bad.md", &[0xff, 0xfe, b'#'], 0, 0)
        .unwrap();
    assert!(idx.query().file("bad.md").unwrap().is_some());
    // Fragment-only links are self links even through incremental upserts.
    idx.upsert_file("Self.md", b"# H\nsee [[#H]] and [[#^b]]\n", 0, 0)
        .unwrap();
    let out = idx.query().outgoing("Self.md").unwrap();
    assert_eq!(out.len(), 2);
    assert!(
        out.iter()
            .all(|l| l.resolved_path.as_deref() == Some("Self.md"))
    );
    sync_check(&idx);
}

#[test]
fn reader_is_read_only() {
    let t = Tmp::new();
    fixture(&t);
    drop(Index::create(&t.vault(), &t.db()).unwrap());
    let r = Reader::open(&t.db(), 1).unwrap();
    // A write through the reader's own connection must fail; exercised via a raw open with the
    // same flags the Reader uses.
    let raw = rusqlite::Connection::open_with_flags(
        generation_path(&t.db(), 1),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert!(raw.execute("DELETE FROM files", []).is_err());
    assert_eq!(r.query().file_count().unwrap(), 6);
}

// ---- symlinked notes (plan editor-cutover-blockers WP-1) ------------------------------------

#[test]
fn symlinked_note_outside_the_root_is_indexed_under_its_vault_path() {
    let t = Tmp::new();
    t.write("a.md", "# A\n");
    let repo = t.0.join("repo");
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    std::fs::write(
        repo.join("INITIATIVE.md"),
        "---\nstatus: active\n---\n[[a]] #init\n",
    )
    .unwrap();
    std::fs::create_dir_all(t.vault().join("initiatives/x")).unwrap();
    std::os::unix::fs::symlink(
        repo.join("INITIATIVE.md"),
        t.vault().join("initiatives/x/INITIATIVE.md"),
    )
    .unwrap();
    // A dangling link is skipped (not an error); a directory link is not followed.
    std::os::unix::fs::symlink(repo.join("gone.md"), t.vault().join("dangling.md")).unwrap();
    std::fs::write(repo.join("sub/inner.md"), "inner").unwrap();
    std::os::unix::fs::symlink(repo.join("sub"), t.vault().join("dirlink")).unwrap();

    let idx = Index::create(&t.vault(), &t.db()).unwrap();
    let q = idx.query();
    assert_eq!(q.file_count().unwrap(), 2);
    assert!(q.file("initiatives/x/INITIATIVE.md").unwrap().is_some());
    assert_eq!(q.tags_of("initiatives/x/INITIATIVE.md").unwrap().len(), 1);
    assert_eq!(q.props("initiatives/x/INITIATIVE.md").unwrap().len(), 1);
    assert_eq!(q.backlinks("a.md").unwrap().len(), 1);
    assert!(q.file("dangling.md").unwrap().is_none());
    assert!(q.file("dirlink/inner.md").unwrap().is_none());
}
