//! Symlinked notes (plan `editor-cutover-blockers` WP-1): indexed under the vault path, confined
//! CAS writes through the link, rename/delete act on the link, the watcher follows the target.
//! Synthetic temp dirs only: the "repo" is a temp dir outside the vault root.

mod common;

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use common::h;
use cv_vault::{
    EventKind, Origin, VaultError, VaultEvent, VaultFs, VaultWriter, WatchConfig,
    spawn_writer_with_watcher,
};
use tempfile::TempDir;

const DEB: Duration = Duration::from_millis(50);

struct Lx {
    vault: TempDir,
    repo: TempDir,
    idx: TempDir,
}

impl Lx {
    /// A vault with `a.md` and `f/plain.md`, and a repo holding `INITIATIVE.md`, linked from
    /// `f/linked.md`.
    fn new() -> Self {
        let lx = Self {
            vault: tempfile::tempdir().unwrap(),
            repo: tempfile::tempdir().unwrap(),
            idx: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(lx.vault.path().join("f")).unwrap();
        std::fs::write(lx.vault.path().join("a.md"), "# A\n").unwrap();
        std::fs::write(lx.vault.path().join("f/plain.md"), "plain").unwrap();
        std::fs::write(lx.target(), "T [[a]]").unwrap();
        lx.link(&lx.target(), "f/linked.md");
        lx
    }

    fn target(&self) -> PathBuf {
        self.repo.path().join("INITIATIVE.md")
    }

    fn link(&self, to: &Path, rel: &str) {
        let at = self.vault.path().join(rel);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(to, at).unwrap();
    }

    fn is_link(&self, rel: &str) -> bool {
        self.vault
            .path()
            .join(rel)
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    }

    fn base(&self) -> PathBuf {
        self.idx.path().join("index.db")
    }

    fn fs(&self, roots: Vec<PathBuf>) -> VaultFs {
        common::live_fs(self.vault.path()).with_link_write_roots(roots)
    }

    /// Live writer allowlisting the repo.
    fn writer(&self) -> VaultWriter {
        VaultWriter::open(self.fs(vec![self.repo.path().to_path_buf()]), &self.base()).unwrap()
    }

    fn target_text(&self) -> String {
        std::fs::read_to_string(self.target()).unwrap()
    }
}

#[test]
fn cas_write_through_link_updates_target_and_keeps_link() {
    let lx = Lx::new();
    let mut w = lx.writer();
    assert!(w.index().query().file("f/linked.md").unwrap().is_some());
    let h2 = w
        .write("f/linked.md", b"T2 #x", Some(h("T [[a]]")))
        .unwrap();
    assert_eq!(h2, h("T2 #x"));
    assert_eq!(lx.target_text(), "T2 #x");
    assert!(lx.is_link("f/linked.md"));
    assert_eq!(w.index().query().tags_of("f/linked.md").unwrap().len(), 1);
    // A stale hash conflicts and changes nothing.
    let err = w
        .write("f/linked.md", b"T3", Some(h("T [[a]]")))
        .unwrap_err();
    assert!(matches!(err, VaultError::Conflict { .. }), "{err:?}");
    assert_eq!(lx.target_text(), "T2 #x");
    assert!(lx.is_link("f/linked.md"));
    // No temp file left beside the target.
    let names: Vec<_> = std::fs::read_dir(lx.repo.path()).unwrap().collect();
    assert_eq!(names.len(), 1);
}

#[test]
fn link_write_outside_root_and_allowlist_is_refused_until_allowlisted() {
    let lx = Lx::new();
    let mut w = VaultWriter::open(lx.fs(Vec::new()), &lx.base()).unwrap();
    let err = w
        .write("f/linked.md", b"nope", Some(h("T [[a]]")))
        .unwrap_err();
    match &err {
        VaultError::LinkWriteRefused { path, .. } => assert_eq!(path, "f/linked.md"),
        other => panic!("expected LinkWriteRefused, got {other:?}"),
    }
    assert!(!err.to_string().contains("T [[a]]"));
    assert_eq!(lx.target_text(), "T [[a]]");
    assert!(lx.is_link("f/linked.md"));
    drop(w);
    let mut w = lx.writer();
    w.write("f/linked.md", b"yes", Some(h("T [[a]]"))).unwrap();
    assert_eq!(lx.target_text(), "yes");
    assert!(lx.is_link("f/linked.md"));
}

#[test]
fn link_into_the_vault_root_is_writable_without_an_allowlist() {
    let lx = Lx::new();
    lx.link(&lx.vault.path().join("a.md"), "alias.md");
    let mut w = VaultWriter::open(lx.fs(Vec::new()), &lx.base()).unwrap();
    w.write("alias.md", b"# A2\n", Some(h("# A\n"))).unwrap();
    assert_eq!(
        std::fs::read_to_string(lx.vault.path().join("a.md")).unwrap(),
        "# A2\n"
    );
    assert!(lx.is_link("alias.md"));
}

#[test]
fn dangling_link_write_is_refused_and_the_link_kept() {
    let lx = Lx::new();
    lx.link(&lx.repo.path().join("gone.md"), "dangling.md");
    let mut w = lx.writer();
    assert!(w.index().query().file("dangling.md").unwrap().is_none());
    let err = w.create("dangling.md", b"x").unwrap_err();
    assert!(
        matches!(err, VaultError::LinkWriteRefused { .. }),
        "{err:?}"
    );
    assert!(lx.is_link("dangling.md"));
    assert!(!lx.repo.path().join("gone.md").exists());
}

#[test]
fn rename_and_delete_act_on_the_link_and_leave_the_target() {
    let lx = Lx::new();
    let mut w = lx.writer();
    w.rename("f/linked.md", "g/moved.md", h("T [[a]]")).unwrap();
    assert!(lx.is_link("g/moved.md"));
    assert!(
        lx.vault
            .path()
            .join("f/linked.md")
            .symlink_metadata()
            .is_err()
    );
    assert_eq!(lx.target_text(), "T [[a]]");
    assert_eq!(w.read("g/moved.md").unwrap().unwrap(), b"T [[a]]");
    assert!(w.index().query().file("g/moved.md").unwrap().is_some());
    w.delete("g/moved.md", h("T [[a]]")).unwrap();
    assert!(
        lx.vault
            .path()
            .join("g/moved.md")
            .symlink_metadata()
            .is_err()
    );
    assert!(
        lx.vault
            .path()
            .join(".trash/g/moved.md")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(lx.target_text(), "T [[a]]");
    assert!(w.index().query().file("g/moved.md").unwrap().is_none());
}

#[test]
fn rename_folder_moves_regular_and_linked_notes_and_keeps_the_target() {
    let lx = Lx::new();
    // A relative link, which must still resolve from its new folder.
    std::fs::write(lx.vault.path().join("a2.md"), "A2").unwrap();
    lx.link(Path::new("../a2.md"), "f/rel.md");
    let mut w = lx.writer();
    w.rename_folder("f", "deep/g").unwrap();
    assert_eq!(
        std::fs::read_to_string(lx.vault.path().join("deep/g/plain.md")).unwrap(),
        "plain"
    );
    assert!(lx.is_link("deep/g/linked.md"));
    assert!(lx.is_link("deep/g/rel.md"));
    assert_eq!(w.read("deep/g/linked.md").unwrap().unwrap(), b"T [[a]]");
    assert_eq!(w.read("deep/g/rel.md").unwrap().unwrap(), b"A2");
    assert_eq!(lx.target_text(), "T [[a]]");
    assert!(!lx.vault.path().join("f").join("linked.md").exists());
    let q = w.index().query();
    assert!(q.file("deep/g/linked.md").unwrap().is_some());
    assert!(q.file("deep/g/plain.md").unwrap().is_some());
}

#[test]
fn schema_1_index_is_rebuilt_on_open_and_contains_the_linked_note() {
    let lx = Lx::new();
    drop(lx.writer());
    // Stamp the existing generation as schema 1 (the generation that omitted links).
    let gen1 = cv_index::generation_path(
        &lx.base(),
        cv_index::current_generation(&lx.base()).unwrap().unwrap(),
    );
    {
        let c = rusqlite::Connection::open(&gen1).unwrap();
        c.execute(
            "UPDATE meta SET value = '1' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
        c.execute("DELETE FROM files WHERE path = 'f/linked.md'", [])
            .unwrap();
    }
    assert!(matches!(
        cv_index::Index::open(&lx.base()),
        Err(cv_index::Error::SchemaMismatch { .. })
    ));
    let w = lx.writer();
    assert!(w.index().query().file("f/linked.md").unwrap().is_some());
}

// ---- watcher ---------------------------------------------------------------------------------

fn cfg() -> WatchConfig {
    WatchConfig {
        debounce: DEB,
        echo_ttl: Duration::from_secs(2),
    }
}

/// Events until one for `path` arrives (≤ 3 s), plus a short quiet tail.
fn until(rx: &Receiver<VaultEvent>, path: &str) -> Vec<VaultEvent> {
    let end = Instant::now() + Duration::from_secs(3);
    let mut out = Vec::new();
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(e) => {
                let hit = e.path == path;
                out.push(e);
                if hit {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let tail_end = Instant::now() + DEB * 4;
    while let Some(left) = tail_end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(e) => out.push(e),
            Err(_) => break,
        }
    }
    out
}

#[test]
fn external_edit_to_the_target_emits_a_change_for_the_vault_path() {
    let lx = Lx::new();
    let (wh, rx, _j) = spawn_writer_with_watcher(lx.writer(), cfg()).unwrap();
    std::fs::write(lx.target(), "T2 #ext").unwrap();
    let evs = until(&rx, "f/linked.md");
    assert!(
        evs.contains(&VaultEvent {
            path: "f/linked.md".into(),
            kind: EventKind::Modified,
            origin: Origin::External,
            seq: None,
            id: None,
            hash: None,
        }),
        "{evs:?}"
    );
    let tags = wh
        .call(|w| w.index().query().tags_of("f/linked.md").unwrap())
        .unwrap();
    assert_eq!(tags.len(), 1);
    // The daemon's own write through the link surfaces as an echo, not an external change.
    wh.write("f/linked.md", b"T3".to_vec(), Some(h("T2 #ext")))
        .unwrap();
    let evs = until(&rx, "f/linked.md");
    assert!(evs.iter().all(|e| e.origin == Origin::Daemon), "{evs:?}");
    assert!(lx.is_link("f/linked.md"));
}

#[test]
fn dangling_link_is_indexed_once_its_target_appears() {
    let lx = Lx::new();
    let later = lx.repo.path().join("later.md");
    lx.link(&later, "later.md");
    let (wh, rx, _j) = spawn_writer_with_watcher(lx.writer(), cfg()).unwrap();
    assert!(
        wh.call(|w| w.index().query().file("later.md").unwrap())
            .unwrap()
            .is_none()
    );
    std::fs::write(&later, "now #here").unwrap();
    let evs = until(&rx, "later.md");
    assert!(
        evs.contains(&VaultEvent {
            path: "later.md".into(),
            kind: EventKind::Created,
            origin: Origin::External,
            seq: None,
            id: None,
            hash: None,
        }),
        "{evs:?}"
    );
    assert!(
        wh.call(|w| w.index().query().file("later.md").unwrap())
            .unwrap()
            .is_some()
    );
}

#[test]
fn link_added_in_the_vault_refreshes_the_watch_set() {
    let lx = Lx::new();
    let other = tempfile::tempdir().unwrap();
    let t2 = other.path().join("N.md");
    std::fs::write(&t2, "N").unwrap();
    let (wh, rx, _j) = spawn_writer_with_watcher(lx.writer(), cfg()).unwrap();
    lx.link(&t2, "n.md");
    let evs = until(&rx, "n.md");
    assert!(
        evs.iter()
            .any(|e| e.path == "n.md" && e.kind == EventKind::Created),
        "{evs:?}"
    );
    // The new target's directory is now watched.
    std::fs::write(&t2, "N2 #t").unwrap();
    let evs = until(&rx, "n.md");
    assert!(
        evs.iter()
            .any(|e| e.path == "n.md" && e.kind == EventKind::Modified),
        "{evs:?}"
    );
    assert_eq!(
        wh.call(|w| w.index().query().tags_of("n.md").unwrap())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn daemon_move_of_a_linked_note_is_one_renamed() {
    let lx = Lx::new();
    let (wh, rx, _j) = spawn_writer_with_watcher(lx.writer(), cfg()).unwrap();
    wh.rename("f/linked.md", "g/moved.md", h("T [[a]]"))
        .unwrap();
    let evs = until(&rx, "g/moved.md");
    assert_eq!(
        evs,
        vec![VaultEvent {
            path: "g/moved.md".into(),
            kind: EventKind::Renamed {
                from: "f/linked.md".into()
            },
            origin: Origin::Daemon,
            seq: None,
            id: None,
            hash: None,
        }]
    );
    assert!(lx.is_link("g/moved.md"));
}
