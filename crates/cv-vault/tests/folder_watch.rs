//! The owned watch set for folder links (plan folder-links-follow WP-2b): the root watch never
//! follows links; every directory a folder link reaches outside the vault is one repo-owned
//! `NonRecursive` watch keyed by canonical directory, refreshed on topology changes. Synthetic
//! temp dirs and real inotify only: each "repo" is a temp dir outside the vault, linked in.

mod common;

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use cv_vault::{EventKind, VaultEvent, VaultWriter, WatchConfig, WriterHandle};
use tempfile::TempDir;

const DEB: Duration = Duration::from_millis(40);
const WAIT: Duration = Duration::from_secs(5);

struct Fw {
    vault: TempDir,
    repo: TempDir,
    idx: TempDir,
}

impl Fw {
    fn new() -> Self {
        let fw = Self {
            vault: tempfile::tempdir().unwrap(),
            repo: tempfile::tempdir().unwrap(),
            idx: tempfile::tempdir().unwrap(),
        };
        std::fs::write(fw.vault.path().join("a.md"), "# A\n").unwrap();
        fw
    }

    fn repo(&self, rel: &str) -> PathBuf {
        self.repo.path().join(rel)
    }

    fn put(&self, rel: &str, text: &str) {
        let p = self.repo(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn link(&self, target: &Path, name: &str) {
        std::os::unix::fs::symlink(target, self.vault.path().join(name)).unwrap();
    }

    fn spawn(&self) -> (WriterHandle, Receiver<VaultEvent>) {
        let w = VaultWriter::open(
            common::live_fs(self.vault.path()),
            &self.idx.path().join("index.db"),
            Box::new(common::MemIds::default()),
        )
        .unwrap();
        let cfg = WatchConfig {
            debounce: DEB,
            echo_ttl: Duration::from_secs(2),
        };
        let (wh, rx, _j) = cv_vault::spawn_writer_with_watcher(w, cfg).unwrap();
        (wh, rx)
    }
}

/// Collects events until `pred` matches one (bounded by [`WAIT`]); panics with what arrived.
fn wait_for(
    rx: &Receiver<VaultEvent>,
    what: &str,
    pred: impl Fn(&VaultEvent) -> bool,
) -> Vec<VaultEvent> {
    let end = Instant::now() + WAIT;
    let mut seen = Vec::new();
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(e) => {
                let hit = pred(&e);
                seen.push(e);
                if hit {
                    return seen;
                }
            }
            Err(_) => break,
        }
    }
    panic!("no event: {what}; got {seen:?}");
}

fn is(path: &str, kind: fn(&EventKind) -> bool) -> impl Fn(&VaultEvent) -> bool {
    move |e: &VaultEvent| e.path == path && kind(&e.kind)
}

fn created(k: &EventKind) -> bool {
    matches!(k, EventKind::Created | EventKind::Modified)
}

fn deleted(k: &EventKind) -> bool {
    matches!(k, EventKind::Deleted { .. })
}

fn indexed(wh: &WriterHandle, path: &str) -> bool {
    let p = path.to_string();
    wh.call(move |w| w.index().query().file(&p).unwrap().is_some())
        .unwrap()
}

/// Drains whatever arrives in a short quiet window.
fn drain(rx: &Receiver<VaultEvent>) {
    while rx.recv_timeout(DEB * 5).is_ok() {}
}

#[test]
fn removing_one_alias_leaves_the_other_receiving_events() {
    let fw = Fw::new();
    fw.put("n.md", "N");
    fw.link(fw.repo.path(), "a1");
    fw.link(fw.repo.path(), "b2");
    let (wh, rx) = fw.spawn();
    assert!(indexed(&wh, "a1/n.md") && !indexed(&wh, "b2/n.md"));
    std::fs::remove_file(fw.vault.path().join("a1")).unwrap();
    wait_for(&rx, "a1/n.md deleted", is("a1/n.md", deleted));
    fw.put("x.md", "X");
    wait_for(&rx, "b2/x.md created", is("b2/x.md", created));
    assert!(indexed(&wh, "b2/n.md") && indexed(&wh, "b2/x.md") && !indexed(&wh, "a1/n.md"));
}

#[test]
fn a_new_winning_alias_takes_over_the_note_id() {
    let fw = Fw::new();
    fw.put("n.md", "N");
    fw.link(fw.repo.path(), "m");
    let (wh, rx) = fw.spawn();
    let id = wh.bind_note("m/n.md").unwrap();
    fw.link(fw.repo.path(), "c");
    let evs = wait_for(&rx, "c/n.md takes over", |e| e.path == "c/n.md");
    let moved = evs
        .iter()
        .find(|e| matches!(&e.kind, EventKind::Renamed { from } if from == "m/n.md"));
    assert_eq!(moved.and_then(|e| e.id), Some(id), "{evs:?}");
    // The losing alias is declined: no row, no new id.
    assert!(!indexed(&wh, "m/n.md") && indexed(&wh, "c/n.md"));
    assert!(wh.bind_note("m/n.md").is_err());
}

#[test]
fn a_file_in_a_new_subdir_under_a_link_is_indexed() {
    let fw = Fw::new();
    fw.put("n.md", "N");
    fw.link(fw.repo.path(), "ext");
    let (wh, rx) = fw.spawn();
    fw.put("sub/deeper/f.md", "F");
    wait_for(
        &rx,
        "ext/sub/deeper/f.md",
        is("ext/sub/deeper/f.md", created),
    );
    assert!(indexed(&wh, "ext/sub/deeper/f.md"));
    // The new directories are watched afterwards too.
    fw.put("sub/deeper/g.md", "G");
    wait_for(
        &rx,
        "ext/sub/deeper/g.md",
        is("ext/sub/deeper/g.md", created),
    );
}

#[test]
fn a_directory_moved_in_is_indexed_and_moved_out_is_removed() {
    let fw = Fw::new();
    let outside = tempfile::tempdir().unwrap();
    fw.put("n.md", "N");
    fw.link(fw.repo.path(), "ext");
    std::fs::create_dir_all(outside.path().join("pop/inner")).unwrap();
    std::fs::write(outside.path().join("pop/q.md"), "Q").unwrap();
    std::fs::write(outside.path().join("pop/inner/r.md"), "R").unwrap();
    let (wh, rx) = fw.spawn();
    std::fs::rename(outside.path().join("pop"), fw.repo("moved")).unwrap();
    wait_for(&rx, "ext/moved/inner/r.md", |e| {
        e.path == "ext/moved/inner/r.md" && created(&e.kind)
    });
    drain(&rx);
    assert!(indexed(&wh, "ext/moved/q.md") && indexed(&wh, "ext/moved/inner/r.md"));
    std::fs::rename(fw.repo("moved"), outside.path().join("back")).unwrap();
    wait_for(
        &rx,
        "ext/moved/inner/r.md deleted",
        is("ext/moved/inner/r.md", deleted),
    );
    drain(&rx);
    assert!(!indexed(&wh, "ext/moved/q.md"));
}

#[test]
fn an_owned_target_moved_away_is_removed_and_reappears_by_rename() {
    let fw = Fw::new();
    fw.put("t/x.md", "X");
    fw.link(&fw.repo("t"), "ext");
    let (wh, rx) = fw.spawn();
    assert!(indexed(&wh, "ext/x.md"));
    // The link target itself moves (`MOVE_SELF`): the link dangles.
    std::fs::rename(fw.repo("t"), fw.repo("t2")).unwrap();
    wait_for(&rx, "ext/x.md deleted", is("ext/x.md", deleted));
    drain(&rx);
    std::fs::rename(fw.repo("t2"), fw.repo("t")).unwrap();
    wait_for(&rx, "ext/x.md back", is("ext/x.md", created));
    fw.put("t/y.md", "Y");
    wait_for(&rx, "ext/y.md", is("ext/y.md", created));
}

#[test]
fn a_dangling_target_appearing_by_mkdir_reconciles() {
    let fw = Fw::new();
    fw.link(&fw.repo("missing"), "ext");
    let (wh, rx) = fw.spawn();
    fw.put("missing/f.md", "F");
    wait_for(&rx, "ext/f.md", is("ext/f.md", created));
    assert!(indexed(&wh, "ext/f.md"));
}

#[test]
fn a_dangling_target_appearing_by_rename_reconciles() {
    let fw = Fw::new();
    fw.put("staging/f.md", "F");
    fw.link(&fw.repo("missing"), "ext");
    let (wh, rx) = fw.spawn();
    std::fs::rename(fw.repo("staging"), fw.repo("missing")).unwrap();
    wait_for(&rx, "ext/f.md", is("ext/f.md", created));
    assert!(indexed(&wh, "ext/f.md"));
}

#[test]
fn a_deleted_then_recreated_target_reconciles() {
    let fw = Fw::new();
    fw.put("t/a.md", "A");
    fw.link(&fw.repo("t"), "ext");
    let (wh, rx) = fw.spawn();
    std::fs::remove_dir_all(fw.repo("t")).unwrap();
    wait_for(&rx, "ext/a.md deleted", is("ext/a.md", deleted));
    drain(&rx);
    fw.put("t/b.md", "B");
    wait_for(&rx, "ext/b.md", is("ext/b.md", created));
    fw.put("t/c.md", "C");
    wait_for(&rx, "ext/c.md", is("ext/c.md", created));
    assert!(indexed(&wh, "ext/b.md") && !indexed(&wh, "ext/a.md"));
}

#[test]
fn a_live_equal_stat_link_retarget_reindexes() {
    let fw = Fw::new();
    fw.put("one/n.md", "one #first");
    fw.put("two/n.md", "two #other");
    let mtime = std::fs::metadata(fw.repo("one/n.md"))
        .unwrap()
        .modified()
        .unwrap();
    std::fs::File::options()
        .write(true)
        .open(fw.repo("two/n.md"))
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    fw.link(&fw.repo("one"), "ext");
    let (wh, rx) = fw.spawn();
    // Retarget atomically (a new link renamed over the old one).
    std::os::unix::fs::symlink(fw.repo("two"), fw.vault.path().join("tmp-link")).unwrap();
    std::fs::rename(
        fw.vault.path().join("tmp-link"),
        fw.vault.path().join("ext"),
    )
    .unwrap();
    let evs = wait_for(&rx, "ext/n.md modified", |e| {
        e.path == "ext/n.md" && e.hash == Some(common::h("two #other"))
    });
    assert!(evs.iter().any(|e| e.path == "ext/n.md"), "{evs:?}");
    let size = wh
        .call(|w| w.index().query().file("ext/n.md").unwrap().map(|r| r.size))
        .unwrap();
    assert_eq!(size, Some("two #other".len() as i64));
}

#[test]
fn a_gitignore_edit_inside_a_linked_subtree_updates_ownership() {
    let fw = Fw::new();
    fw.put("sub/s.md", "S");
    fw.put("n.md", "N");
    fw.link(fw.repo.path(), "ext");
    let (wh, rx) = fw.spawn();
    let owned = |wh: &WriterHandle| {
        wh.call(|w| w.index().ownership().canonical_of("ext/sub").is_some())
            .unwrap()
    };
    assert!(owned(&wh));
    fw.put(".gitignore", "sub/\n");
    let end = Instant::now() + WAIT;
    while owned(&wh) && Instant::now() < end {
        let _ = rx.recv_timeout(DEB);
    }
    assert!(!owned(&wh), "ext/sub still owned after the .gitignore edit");
    drain(&rx);
    fw.put("sub/s2.md", "S2");
    fw.put("n2.md", "N2");
    let evs = wait_for(&rx, "ext/n2.md", is("ext/n2.md", created));
    assert!(
        evs.iter().all(|e| !e.path.starts_with("ext/sub/")),
        "{evs:?}"
    );
    assert!(!indexed(&wh, "ext/sub/s2.md"));
}

#[test]
fn the_resync_entry_point_restores_delivery_after_a_replaced_dir() {
    let fw = Fw::new();
    fw.put("t/a.md", "A");
    fw.link(&fw.repo("t"), "ext");
    let (wh, rx) = fw.spawn();
    // Replace the external directory at the same canonical path, then resync directly (what an
    // overflow's `Rescan` triggers).
    std::fs::remove_dir_all(fw.repo("t")).unwrap();
    fw.put("t/a.md", "A2");
    wh.resync_watches(true).unwrap();
    drain(&rx);
    fw.put("t/z.md", "Z");
    wait_for(&rx, "ext/z.md", is("ext/z.md", created));
    assert!(indexed(&wh, "ext/z.md"));
    // The in-vault root watch was re-registered too.
    std::fs::write(fw.vault.path().join("b.md"), "B").unwrap();
    wait_for(&rx, "b.md", is("b.md", created));
}
