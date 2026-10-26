//! Plan tidiness-trash-orphans WP-1: the trash seam and its records. Every system move goes to a
//! temp data-home (never `~/.local/share/Trash`).

mod common;

use std::sync::Arc;

use common::{Fx, h};
use cv_vault::{SystemTrash, TrashReason, TrashTarget, TrashedIn, cv_owned_system_entries};

const DATE: &str = "2026-10-26T12:34:56";

fn system(home: &std::path::Path) -> SystemTrash {
    SystemTrash::new(home, Arc::new(|| DATE.to_string()))
}

#[test]
fn vault_move_records_deleted_row() {
    let fx = Fx::new(&[("f/a.md", "A")]);
    let mut w = fx.live();
    w.delete("f/a.md", h("A")).unwrap();
    let recs = w.take_trash_records();
    assert_eq!(recs.len(), 1);
    let r = &recs[0];
    assert_eq!(r.moved.location, TrashedIn::Vault);
    assert_eq!(r.moved.trash_path, ".trash/f/a.md");
    assert_eq!(r.original_path, "f/a.md");
    assert_eq!(r.moved.bytes, 1);
    assert_eq!(r.reason, TrashReason::Deleted);
    assert!(r.info_name().is_none());
    let md = std::fs::symlink_metadata(fx.vault.path().join(".trash/f/a.md")).unwrap();
    assert_eq!(
        r.moved.inode,
        std::os::unix::fs::MetadataExt::ino(&md),
        "rename keeps the inode"
    );
}

#[test]
fn permanent_and_failed_deletes_record_nothing() {
    let fx = Fx::new(&[("a.md", "A"), ("b.md", "B")]);
    let mut w = fx.live();
    w.delete_with("a.md", h("A"), true).unwrap();
    assert!(w.delete("b.md", h("nope")).is_err());
    assert!(w.delete("gone.md", h("x")).is_err());
    assert!(w.take_trash_records().is_empty());
}

#[test]
fn replaced_and_note_attachment_reasons() {
    let fx = Fx::new(&[("r.md", "R"), ("n.md", "![[own.png]]\n"), ("own.png", "o")]);
    let mut w = fx.live();
    w.delete_replaced("r.md", h("R"), "other.md").unwrap();
    w.delete_note("n.md", &["own.png".to_string()], false)
        .unwrap();
    let got: Vec<(String, TrashReason)> = w
        .take_trash_records()
        .into_iter()
        .map(|r| (r.original_path, r.reason))
        .collect();
    assert_eq!(
        got,
        [
            ("r.md".to_string(), TrashReason::Replaced),
            ("n.md".to_string(), TrashReason::Deleted),
            ("own.png".to_string(), TrashReason::NoteAttachment),
        ]
    );
}

#[test]
fn trash_with_reason_and_sink_receive_records() {
    let fx = Fx::new(&[("a.png", "a"), ("b.png", "b")]);
    let mut w = fx.live();
    w.trash_with_reason("a.png", h("a"), TrashReason::Orphan)
        .unwrap();
    let got = Arc::new(std::sync::Mutex::new(Vec::new()));
    let g = Arc::clone(&got);
    // The queued record is handed over at install; later ones go straight to the sink.
    w.set_trash_sink(Box::new(move |r| g.lock().unwrap().push(r)));
    w.delete("b.png", h("b")).unwrap();
    let got = got.lock().unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].reason, TrashReason::Orphan);
    assert_eq!(got[1].original_path, "b.png");
    assert!(w.take_trash_records().is_empty());
}

#[test]
fn system_move_writes_trashinfo() {
    let fx = Fx::new(&[("My notes/caf\u{e9}.md", "C")]);
    let home = tempfile::tempdir().unwrap();
    let mut w = fx.live();
    w.fs_mut()
        .set_trash_target(TrashTarget::System(system(home.path())));
    w.delete("My notes/caf\u{e9}.md", h("C")).unwrap();
    let t = home.path().join("Trash");
    assert_eq!(
        std::fs::read_to_string(t.join("files/caf\u{e9}.md")).unwrap(),
        "C"
    );
    let info = std::fs::read_to_string(t.join("info/caf\u{e9}.md.trashinfo")).unwrap();
    let root = fx.root().to_string_lossy().into_owned();
    let lines: Vec<&str> = info.lines().collect();
    assert_eq!(lines[0], "[Trash Info]");
    assert_eq!(
        lines[1],
        format!(
            "Path={}/My%20notes/caf%C3%A9.md",
            cv_vault::trash::percent_encode_path(&root)
        )
    );
    assert_eq!(lines[2], format!("DeletionDate={DATE}"));
    assert!(!fx.vault.path().join(".trash").exists());
    let r = w.take_trash_records().remove(0);
    assert_eq!(r.moved.location, TrashedIn::System);
    assert_eq!(r.moved.trash_path, "caf\u{e9}.md");
    assert_eq!(r.info_name().as_deref(), Some("caf\u{e9}.md.trashinfo"));
    // The crash receipt: cv-owned and adoptable.
    let owned = cv_owned_system_entries(&t).unwrap();
    assert_eq!(owned.len(), 1);
    assert_eq!(owned[0].original_path, "My notes/caf\u{e9}.md");
    assert_eq!(owned[0].inode, r.moved.inode);
}

#[test]
fn system_name_collision_is_unique() {
    let fx = Fx::new(&[("a.md", "new"), ("b/a.md", "B")]);
    let home = tempfile::tempdir().unwrap();
    let t = home.path().join("Trash");
    std::fs::create_dir_all(t.join("files")).unwrap();
    std::fs::create_dir_all(t.join("info")).unwrap();
    std::fs::write(t.join("files/a.md"), "taken").unwrap();
    std::fs::write(t.join("info/a (1).md.trashinfo"), "[Trash Info]\n").unwrap();
    let mut w = fx.live();
    w.fs_mut()
        .set_trash_target(TrashTarget::System(system(home.path())));
    w.delete("a.md", h("new")).unwrap();
    w.delete("b/a.md", h("B")).unwrap();
    assert_eq!(
        std::fs::read_to_string(t.join("files/a (2).md")).unwrap(),
        "new"
    );
    assert_eq!(
        std::fs::read_to_string(t.join("files/a (3).md")).unwrap(),
        "B"
    );
    assert!(t.join("info/a (2).md.trashinfo").exists());
    assert_eq!(
        std::fs::read_to_string(t.join("files/a.md")).unwrap(),
        "taken"
    );
}

#[test]
fn cross_device_falls_back_to_vault_trash() {
    let fx = Fx::new(&[("x/a.md", "A")]);
    let home = tempfile::tempdir().unwrap();
    let exdev = system(home.path()).with_rename(Arc::new(|_, _| {
        Err(std::io::Error::from(std::io::ErrorKind::CrossesDevices))
    }));
    let mut w = fx.live();
    w.fs_mut().set_trash_target(TrashTarget::System(exdev));
    w.delete("x/a.md", h("A")).unwrap();
    assert_eq!(fx.read(".trash/x/a.md"), "A");
    let t = home.path().join("Trash");
    assert_eq!(
        std::fs::read_dir(t.join("info")).unwrap().count(),
        0,
        "no stray .trashinfo"
    );
    assert_eq!(std::fs::read_dir(t.join("files")).unwrap().count(), 0);
    let r = w.take_trash_records().remove(0);
    assert_eq!(r.moved.location, TrashedIn::Vault);
    assert_eq!(r.moved.trash_path, ".trash/x/a.md");
}
