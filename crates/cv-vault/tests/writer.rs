mod common;

use common::{Fx, all_names, h};
use cv_vault::{RenameOutcome, VaultError};

#[test]
fn cas_write_conflict_and_exists() {
    let fx = Fx::new(&[("a.md", "one")]);
    let mut w = fx.live();
    let h2 = w.write("a.md", b"two", Some(h("one"))).unwrap();
    assert_eq!(h2, h("two"));
    let err = w.write("a.md", b"three", Some(h("one"))).unwrap_err();
    assert!(
        matches!(&err, VaultError::Conflict { path, expected, found } if path == "a.md" && *expected == Some(h("one")) && *found == Some(h("two"))),
        "{err:?}"
    );
    assert_eq!(fx.read("a.md"), "two");
    assert!(matches!(w.create("a.md", b"x"), Err(VaultError::Exists(_))));
    assert!(matches!(
        w.write("a.md", b"x", None),
        Err(VaultError::Conflict { .. })
    ));
    assert!(matches!(
        w.write("../x.md", b"x", None),
        Err(VaultError::InvalidPath(_))
    ));
    w.create("d/new.md", b"#tag").unwrap();
    assert!(w.index().query().file("d/new.md").unwrap().is_some());
    assert!(
        !all_names(fx.vault.path())
            .iter()
            .any(|n| n.starts_with(".cv-tmp-"))
    );
}

#[test]
fn recoverable_delete_into_trash() {
    let fx = Fx::new(&[("f/a.md", "A"), (".trash/f/a.md", "old")]);
    let mut w = fx.live();
    assert!(w.index().query().file("f/a.md").unwrap().is_some());
    assert!(matches!(
        w.delete("f/a.md", h("nope")),
        Err(VaultError::Conflict { .. })
    ));
    w.delete("f/a.md", h("A")).unwrap();
    assert!(!fx.vault.path().join("f/a.md").exists());
    assert_eq!(fx.read(".trash/f/a (1).md"), "A");
    assert_eq!(fx.read(".trash/f/a.md"), "old");
    assert!(w.index().query().file("f/a.md").unwrap().is_none());
    assert!(matches!(
        w.delete("f/a.md", h("A")),
        Err(VaultError::NotFound(_))
    ));
}

#[test]
fn permanent_delete_removes_outright_with_cas() {
    let fx = Fx::new(&[("f/a.md", "A")]);
    let mut w = fx.live();
    assert!(matches!(
        w.delete_with("f/a.md", h("nope"), true),
        Err(VaultError::Conflict { .. })
    ));
    assert_eq!(fx.read("f/a.md"), "A");
    w.delete_with("f/a.md", h("A"), true).unwrap();
    assert!(!fx.vault.path().join("f/a.md").exists());
    assert!(!fx.vault.path().join(".trash").exists());
    assert!(w.index().query().file("f/a.md").unwrap().is_none());
}

#[test]
fn delete_note_rechecks_references_at_delete_time() {
    let fx = Fx::new(&[
        ("n.md", "![[own.png]] ![[late.png]] ![[shared.png]]\n"),
        ("own.png", "o"),
        ("late.png", "l"),
        ("shared.png", "s"),
        ("m.md", "![[shared.png]]\n"),
        (".trash/own.png", "older"),
    ]);
    let mut w = fx.live();
    // A reference to late.png appears after the preview (here: written through the writer).
    w.create("x.md", b"![[late.png]]\n").unwrap();
    let atts: Vec<String> = ["own.png", "late.png", "shared.png", "gone.png"]
        .map(String::from)
        .to_vec();
    let (removed, skipped) = w.delete_note("n.md", &atts, false).unwrap();
    assert_eq!(removed, ["n.md", "own.png"]);
    assert_eq!(skipped, ["late.png", "shared.png", "gone.png"]);
    assert!(fx.vault.path().join("late.png").exists());
    assert!(fx.vault.path().join("shared.png").exists());
    // Trash collision still suffixes.
    assert_eq!(fx.read(".trash/own (1).png"), "o");
    assert_eq!(
        fx.read(".trash/n.md"),
        "![[own.png]] ![[late.png]] ![[shared.png]]\n"
    );
}

fn rename_fixture() -> Fx {
    Fx::new(&[
        ("notes/B.md", "# B\n## Sec\nself [[B#Sec]] and [[#Sec]]\n"),
        ("src1.md", "see [[B]] and [[notes/B|alias]]\n"),
        ("src2.md", "embed ![[B#Sec|Al]] here\n"),
        (
            "dir/src3.md",
            "rel [x](../notes/B.md) enc [y](notes/B.md#Sec)\n",
        ),
        ("src4.md", "---\nup: \"[[B]]\"\n---\nbody\n"),
        ("stale.md", "stale [[B]]\n"),
    ])
}

#[test]
fn rename_rewrites_inbound_links() {
    let fx = rename_fixture();
    let mut w = fx.live();
    // stale.md changed behind the index's back.
    std::fs::write(fx.vault.path().join("stale.md"), "edited!! stale [[B]]\n").unwrap();
    let out = w
        .rename("notes/B.md", "moved/New B.md", h(&fx.read("notes/B.md")))
        .unwrap();
    assert_eq!(
        out,
        RenameOutcome {
            rewritten: vec![
                "dir/src3.md".into(),
                "moved/New B.md".into(),
                "src1.md".into(),
                "src2.md".into(),
                "src4.md".into(),
            ],
            skipped_conflicts: vec!["stale.md".into()],
            ..RenameOutcome::default()
        }
    );
    assert_eq!(
        fx.read("src1.md"),
        "see [[New B]] and [[moved/New B|alias]]\n"
    );
    assert_eq!(fx.read("src2.md"), "embed ![[New B#Sec|Al]] here\n");
    assert_eq!(
        fx.read("dir/src3.md"),
        "rel [x](../moved/New%20B.md) enc [y](moved/New%20B.md#Sec)\n"
    );
    assert_eq!(fx.read("src4.md"), "---\nup: \"[[New B]]\"\n---\nbody\n");
    assert_eq!(
        fx.read("moved/New B.md"),
        "# B\n## Sec\nself [[New B#Sec]] and [[#Sec]]\n"
    );
    assert_eq!(fx.read("stale.md"), "edited!! stale [[B]]\n");
    let q = w.index().query();
    assert!(q.file("notes/B.md").unwrap().is_none());
    let back: Vec<_> = q
        .backlinks("moved/New B.md")
        .unwrap()
        .into_iter()
        .map(|l| l.src_path)
        .collect();
    for s in [
        "src1.md",
        "src2.md",
        "dir/src3.md",
        "src4.md",
        "moved/New B.md",
    ] {
        assert!(back.iter().any(|b| b == s), "{s} not in {back:?}");
    }
}

#[test]
fn rename_encoded_md_link_and_ambiguous_basename() {
    let fx = Fx::new(&[
        ("a/My Note.md", "x"),
        ("s.md", "[l](a/My%20Note.md) [[My Note]]\n"),
        ("other/Target.md", "namesake"),
    ]);
    let mut w = fx.live();
    w.rename("a/My Note.md", "b/Target.md", h("x")).unwrap();
    // `Target` is now ambiguous (other/Target.md): fall back to the path form.
    assert_eq!(fx.read("s.md"), "[l](b/Target.md) [[b/Target]]\n");
    let fx = Fx::new(&[("a/My Note.md", "x"), ("s.md", "[l](a/My%20Note.md)\n")]);
    let mut w = fx.live();
    w.rename("a/My Note.md", "b/Your Note.md", h("x")).unwrap();
    assert_eq!(fx.read("s.md"), "[l](b/Your%20Note.md)\n");
}

#[test]
fn rename_md_to_txt_reupserts_and_exists() {
    let fx = Fx::new(&[("a.md", "#tag [[c]]"), ("c.md", "")]);
    let mut w = fx.live();
    assert!(matches!(
        w.rename("a.md", "c.md", h("#tag [[c]]")),
        Err(VaultError::Exists(_))
    ));
    w.rename("a.md", "a.txt", h("#tag [[c]]")).unwrap();
    let q = w.index().query();
    let f = q.file("a.txt").unwrap().unwrap();
    assert!(!f.is_md);
    assert!(q.tags_of("a.txt").unwrap().is_empty());
    assert!(q.backlinks("c.md").unwrap().is_empty());
}

#[test]
fn folder_rename_moves_every_file() {
    let fx = Fx::new(&[
        ("old/x.md", "X"),
        ("old/sub/y.md", "[[x]]"),
        ("r.md", "[[old/x]]"),
    ]);
    let mut w = fx.live();
    let out = w.rename_folder("old", "new").unwrap();
    assert!(out.skipped_conflicts.is_empty());
    assert!(!fx.vault.path().join("old").exists());
    assert_eq!(fx.read("new/x.md"), "X");
    assert_eq!(fx.read("r.md"), "[[new/x]]");
    assert_eq!(fx.read("new/sub/y.md"), "[[x]]");
    assert!(w.index().query().file("new/sub/y.md").unwrap().is_some());
}

/// SR9: a collision mid-folder no longer aborts the rename — the other files still move and the
/// merged outcome reports both the moved files and the one left in place.
#[test]
fn folder_rename_continues_past_a_mid_folder_conflict() {
    let fx = Fx::new(&[
        ("old/a.md", "A"),
        ("old/b.md", "B"),
        ("old/c.md", "C"),
        ("new/b.md", "already here"),
        ("r.md", "[[old/c]]"),
    ]);
    let mut w = fx.live();
    let out = w.rename_folder("old", "new").unwrap();
    assert_eq!(out.moved, vec!["new/a.md".to_string(), "new/c.md".into()]);
    assert_eq!(out.failed.len(), 1);
    assert_eq!(out.failed[0].0, "old/b.md");
    assert_eq!(fx.read("new/a.md"), "A");
    assert_eq!(fx.read("new/c.md"), "C");
    assert_eq!(fx.read("new/b.md"), "already here");
    assert_eq!(fx.read("old/b.md"), "B");
    assert_eq!(fx.read("r.md"), "[[new/c]]");
    assert!(out.rewritten.contains(&"r.md".to_string()));
}

const MOVED: &str = "rel [x](../t/x.md) img ![i](sub/y.png) wiki [[../t/x]] self [[m]] base [[x]] vault [[a/t/x]]\n";

#[test]
fn moved_note_relative_outgoing_links_rebased() {
    for (to, want) in [
        (
            "a/b/c/d/m.md",
            "rel [x](../../../t/x.md) img ![i](../../sub/y.png) wiki [[../../../t/x]] self [[m]] base [[x]] vault [[a/t/x]]\n",
        ),
        (
            "m.md",
            "rel [x](a/t/x.md) img ![i](a/b/sub/y.png) wiki [[./a/t/x]] self [[m]] base [[x]] vault [[a/t/x]]\n",
        ),
    ] {
        let fx = Fx::new(&[
            ("a/b/m.md", MOVED),
            ("a/t/x.md", "X"),
            ("a/b/sub/y.png", "png"),
        ]);
        let mut w = fx.live();
        let before = w.index().query().outgoing("a/b/m.md").unwrap();
        assert!(
            before.iter().all(|l| l.resolved_path.is_some()),
            "{before:?}"
        );
        let out = w.rename("a/b/m.md", to, h(MOVED)).unwrap();
        assert_eq!(out.rewritten, vec![to.to_string()]);
        assert_eq!(fx.read(to), want);
        let after = w.index().query().outgoing(to).unwrap();
        let targets = |v: &[cv_index::LinkRow]| -> Vec<Option<String>> {
            v.iter()
                .map(|l| {
                    l.resolved_path
                        .clone()
                        .map(|p| if p == "a/b/m.md" { to.to_string() } else { p })
                })
                .collect()
        };
        assert_eq!(targets(&after), targets(&before), "{after:?}");
    }
}

#[test]
fn moved_note_with_only_vault_links_unchanged_but_self() {
    let text = "base [[x]] vault [[a/t/x]] md [v](a/t/x.md) self [[m#h]]\n# h\n";
    let fx = Fx::new(&[("a/b/m.md", text), ("a/t/x.md", "X"), ("other/m2.md", "")]);
    let mut w = fx.live();
    w.rename("a/b/m.md", "z/n.md", h(text)).unwrap();
    assert_eq!(fx.read("z/n.md"), text.replace("[[m#h]]", "[[n#h]]"));
}

#[test]
fn open_reconciles_files_created_while_stopped() {
    let t = Fx::new(&[("Desktop.md", "[[Ingestion]]\n"), ("Old.md", "old\n")]);
    let generation = t.live().index().generation().unwrap();
    // Changes while the daemon is down.
    std::fs::create_dir_all(t.vault.path().join("core")).unwrap();
    std::fs::write(t.vault.path().join("core/Ingestion.md"), "# Ingestion\n").unwrap();
    std::fs::remove_file(t.vault.path().join("Old.md")).unwrap();
    // Reopen: the existing generation is reused and reconciled in place. Opening the writer
    // never emits watcher events (the watcher is a separate component seeded afterwards), so
    // this asserts the index content.
    let w = t.live();
    let idx = w.index();
    assert_eq!(idx.generation().unwrap(), generation);
    let q = idx.query();
    let ing = q.file("core/Ingestion.md").unwrap().expect("indexed");
    assert!(q.file("Old.md").unwrap().is_none());
    assert_eq!(
        idx.resolver().resolve("Desktop.md", 0, "Ingestion"),
        Some(ing.id)
    );
}

/// Decision 4: a failing id sink makes the write report the failure. The file operation has
/// already happened and is not rolled back.
#[test]
fn failing_sink_reports_the_failure_after_the_write() {
    let fx = Fx::new(&[("a.md", "one")]);
    let ids = common::MemIds::default();
    let fail = std::sync::Arc::clone(&ids.fail);
    let mut w =
        cv_vault::VaultWriter::open(common::live_fs(fx.vault.path()), &fx.base(), Box::new(ids))
            .unwrap();
    fail.store(true, std::sync::atomic::Ordering::SeqCst);
    let err = w.write("a.md", b"two", Some(h("one"))).unwrap_err();
    assert!(matches!(err, VaultError::NoteId(_)), "{err:?}");
    assert_eq!(fx.read("a.md"), "two", "the write itself landed");
    assert!(w.bind_note("a.md").is_err());
    fail.store(false, std::sync::atomic::Ordering::SeqCst);
    w.write("a.md", b"three", Some(h("two"))).unwrap();
}
