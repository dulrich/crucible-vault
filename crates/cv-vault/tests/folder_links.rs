//! Folder links on the write side (plan folder-links-follow WP-1): confinement through any link
//! component (Decision 2), the automation guard (Decision 3), link-safe `rename_folder`
//! (Decision 4) and the trash storage guard. Synthetic temp dirs only: the "repo" is a temp dir
//! outside the vault root, linked in as the folder `ext`.

mod common;

use std::path::PathBuf;

use common::h;
use cv_vault::{VaultError, VaultWriter};
use tempfile::TempDir;

struct Fl {
    vault: TempDir,
    repo: TempDir,
    idx: TempDir,
}

impl Fl {
    /// A vault with `a.md` and `ext` → the repo (holding `n.md`).
    fn new() -> Self {
        let fl = Self {
            vault: tempfile::tempdir().unwrap(),
            repo: tempfile::tempdir().unwrap(),
            idx: tempfile::tempdir().unwrap(),
        };
        std::fs::write(fl.vault.path().join("a.md"), "# A\n").unwrap();
        std::fs::write(fl.repo.path().join("n.md"), "N").unwrap();
        std::os::unix::fs::symlink(fl.repo.path(), fl.vault.path().join("ext")).unwrap();
        fl
    }

    fn writer(&self, roots: Vec<PathBuf>) -> VaultWriter {
        VaultWriter::open(
            common::live_fs(self.vault.path()).with_link_write_roots(roots),
            &self.idx.path().join("index.db"),
            Box::new(common::MemIds::default()),
        )
        .unwrap()
    }

    fn allowed(&self) -> VaultWriter {
        self.writer(vec![self.repo.path().to_path_buf()])
    }

    fn refused(r: Result<impl std::fmt::Debug, VaultError>) {
        assert!(
            matches!(r, Err(VaultError::LinkWriteRefused { .. })),
            "{r:?}"
        );
    }
}

#[test]
fn allowed_root_write_through_a_folder_link_lands_and_keeps_the_link() {
    let fl = Fl::new();
    let mut w = fl.allowed();
    w.write("ext/new.md", b"hello", None).unwrap();
    w.write("ext/sub/deep.md", b"deep", None).unwrap();
    assert_eq!(
        std::fs::read(fl.repo.path().join("new.md")).unwrap(),
        b"hello"
    );
    assert!(fl.repo.path().join("sub/deep.md").is_file());
    let md = fl.vault.path().join("ext").symlink_metadata().unwrap();
    assert!(md.file_type().is_symlink(), "the folder link is kept");
}

#[test]
fn write_touch_delete_and_move_outside_every_root_are_refused() {
    let fl = Fl::new();
    let mut w = fl.writer(Vec::new());
    Fl::refused(w.write("ext/new.md", b"x", None));
    Fl::refused(w.write("ext/n.md", b"x", Some(h("N"))));
    Fl::refused(w.touch("ext/n.md"));
    Fl::refused(w.delete("ext/n.md", h("N")));
    Fl::refused(w.delete_with("ext/n.md", h("N"), true));
    Fl::refused(w.rename("ext/n.md", "moved.md", h("N")));
    Fl::refused(w.rename("a.md", "ext/a.md", h("# A\n")));
    assert_eq!(std::fs::read(fl.repo.path().join("n.md")).unwrap(), b"N");
    assert!(!fl.repo.path().join("new.md").exists());
    assert!(!fl.repo.path().join("a.md").exists());
    assert!(fl.vault.path().join("a.md").is_file());
    // A touch inside the vault still works.
    w.touch("a.md").unwrap();
}

#[test]
fn automation_skips_removals_and_moves_outside_the_vault_whatever_the_roots() {
    let fl = Fl::new();
    let mut w = fl.allowed();
    let (r, skipped) = w.automation(|w| w.delete("ext/n.md", h("N")));
    assert!(
        matches!(r, Err(VaultError::AutomationSkipped { .. })),
        "{r:?}"
    );
    assert_eq!(skipped, vec!["ext/n.md".to_string()]);
    let (r, skipped) = w.automation(|w| w.rename("ext/n.md", "n.md", h("N")));
    assert!(
        r.as_ref().is_err_and(VaultError::is_automation_skip),
        "{r:?}"
    );
    assert_eq!(skipped, vec!["ext/n.md".to_string(), "n.md".to_string()]);
    let (r, _) = w.automation(|w| w.rename("a.md", "ext/a.md", h("# A\n")));
    assert!(r.is_err_and(|e| e.is_automation_skip()));
    assert_eq!(std::fs::read(fl.repo.path().join("n.md")).unwrap(), b"N");
    assert!(fl.vault.path().join("a.md").is_file());
    // In-vault automation is untouched; the same user delete (Decision 2) is allowed.
    let (r, skipped) = w.automation(|w| w.delete("a.md", h("# A\n")));
    r.unwrap();
    assert!(skipped.is_empty());
    w.delete("ext/n.md", h("N")).unwrap();
    assert!(!fl.repo.path().join("n.md").exists());
}

#[test]
fn rename_folder_never_moves_a_file_through_a_link() {
    let fl = Fl::new();
    std::fs::create_dir_all(fl.vault.path().join("p")).unwrap();
    std::fs::write(fl.vault.path().join("p/x.md"), "x").unwrap();
    std::os::unix::fs::symlink(fl.repo.path(), fl.vault.path().join("p/nested")).unwrap();
    let mut w = fl.allowed();
    // Source: a nested folder link under the renamed folder; the folder link itself.
    Fl::refused(w.rename_folder("p", "q"));
    Fl::refused(w.rename_folder("ext", "inside"));
    assert!(fl.vault.path().join("p/x.md").is_file());
    assert!(fl.repo.path().join("n.md").is_file());
    // Destination through a folder link (E1), even under an allowed root.
    std::fs::remove_file(fl.vault.path().join("p/nested")).unwrap();
    Fl::refused(w.rename_folder("p", "ext/p"));
    Fl::refused(w.rename_folder("p", "ext"));
    assert!(fl.vault.path().join("p/x.md").is_file());
    assert!(!fl.repo.path().join("p").exists());
    // A plain folder rename still works.
    let o = w.rename_folder("p", "q").unwrap();
    assert_eq!(o.moved, vec!["q/x.md".to_string()]);
}

#[test]
fn a_linked_trash_parent_is_refused() {
    let fl = Fl::new();
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), fl.vault.path().join(".trash")).unwrap();
    let mut w = fl.allowed();
    Fl::refused(w.delete("a.md", h("# A\n")));
    assert!(fl.vault.path().join("a.md").is_file());
    assert!(
        std::fs::read_dir(elsewhere.path())
            .unwrap()
            .next()
            .is_none()
    );
    assert!(cv_vault::trash::storage_path(fl.vault.path(), ".trash/a.md").is_err());
    // A linked system trash folder is refused at adoption too.
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(data.path().join("Trash")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), data.path().join("Trash/info")).unwrap();
    assert!(cv_vault::cv_owned_system_entries(&data.path().join("Trash")).is_err());
}

#[test]
fn automation_moves_and_deletes_an_in_vault_file_link_to_an_external_file() {
    let fl = Fl::new();
    let ext = fl.repo.path().join("n.md");
    std::os::unix::fs::symlink(&ext, fl.vault.path().join("l.md")).unwrap();
    let mut w = fl.writer(Vec::new());
    let (r, skipped) = w.automation(|w| w.rename("l.md", "f/l.md", h("N")));
    r.unwrap();
    assert!(skipped.is_empty());
    let moved = fl.vault.path().join("f/l.md");
    assert!(moved.symlink_metadata().unwrap().file_type().is_symlink());
    let (r, skipped) = w.automation(|w| w.delete("f/l.md", h("N")));
    r.unwrap();
    assert!(skipped.is_empty());
    assert!(moved.symlink_metadata().is_err(), "the link entry is gone");
    assert_eq!(
        std::fs::read(&ext).unwrap(),
        b"N",
        "the referent is untouched"
    );
}
