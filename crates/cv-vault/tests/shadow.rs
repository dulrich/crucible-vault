mod common;

use std::path::PathBuf;

use common::{Fx, h};
use cv_vault::{CutoverMarker, MemoryShadowLog, NoCutover, VaultError, VaultFs, validate_scratch};

struct Present;
impl CutoverMarker for Present {
    fn present(&self) -> cv_vault::Result<bool> {
        Ok(true)
    }
}

#[test]
fn live_refused_under_protected_root_without_marker() {
    let prot = tempfile::tempdir().unwrap();
    let vault = prot.path().join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    for root in [prot.path().to_path_buf(), vault.clone()] {
        let err = VaultFs::live(&root, prot.path(), &NoCutover).unwrap_err();
        assert!(matches!(err, VaultError::ProtectedRoot { .. }), "{err:?}");
        assert!(VaultFs::live(&root, prot.path(), &Present).is_ok());
    }
    // An ancestor of the protected root reaches into it: refused too.
    let parent = prot.path().parent().unwrap().to_path_buf();
    let err = VaultFs::live(&parent, &vault, &NoCutover).unwrap_err();
    assert!(matches!(err, VaultError::ProtectedRoot { .. }), "{err:?}");
    assert!(VaultFs::live(&parent, &vault, &Present).is_ok());
    let err = VaultFs::live(prot.path(), &vault, &NoCutover).unwrap_err();
    assert!(matches!(err, VaultError::ProtectedRoot { .. }), "{err:?}");
    let elsewhere = tempfile::tempdir().unwrap();
    assert!(VaultFs::live(elsewhere.path(), prot.path(), &NoCutover).is_ok());
    assert_eq!(
        cv_vault::DEFAULT_PROTECTED_ROOT,
        "~/vault"
    );
}

#[test]
fn scratch_validation_refusals() {
    let live = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let ok = tempfile::tempdir().unwrap();
    let repos = [repo.path().to_path_buf()];
    let refused = |p: PathBuf| {
        let e = validate_scratch(&p, live.path(), &repos).unwrap_err();
        assert!(matches!(e, VaultError::ScratchRefused { .. }), "{e:?}");
    };
    refused(live.path().to_path_buf());
    refused(live.path().join("scratch"));
    refused(repo.path().join("x/scratch"));
    // A scratch dir containing the live root.
    let outer = tempfile::tempdir().unwrap();
    let inner_live = outer.path().join("live");
    std::fs::create_dir(&inner_live).unwrap();
    let e = validate_scratch(outer.path(), &inner_live, &repos).unwrap_err();
    assert!(matches!(e, VaultError::ScratchRefused { .. }), "{e:?}");
    let gitty = tempfile::tempdir().unwrap();
    std::fs::create_dir(gitty.path().join(".git")).unwrap();
    refused(gitty.path().join("deep/scratch"));
    let s = validate_scratch(&ok.path().join("s"), live.path(), &repos).unwrap();
    assert!(s.is_dir());
}

#[test]
fn shadow_writes_go_to_overlay_and_log() {
    let fx = Fx::new(&[("a.md", "A"), ("b.md", "B"), ("c.md", "C")]);
    let scratch = tempfile::tempdir().unwrap();
    let log = MemoryShadowLog::default();
    let mut w = fx.shadow(scratch.path(), log.clone());
    let before_files = w.index().query().file_count().unwrap();
    let a_row = w.index().query().file("a.md").unwrap();

    w.write("a.md", b"A2", Some(h("A"))).unwrap();
    w.create("new.md", b"N").unwrap();
    w.delete("b.md", h("B")).unwrap();
    w.rename("c.md", "d/c2.md", h("C")).unwrap();

    // Live root untouched.
    assert_eq!(fx.read("a.md"), "A");
    assert_eq!(fx.read("b.md"), "B");
    assert_eq!(fx.read("c.md"), "C");
    assert!(!fx.vault.path().join("new.md").exists());
    assert!(!fx.vault.path().join("d").exists());
    assert!(!fx.vault.path().join(".trash").exists());
    // Overlay holds the writes; reads see it.
    let ov = scratch.path().join("vault");
    assert_eq!(std::fs::read_to_string(ov.join("a.md")).unwrap(), "A2");
    assert_eq!(w.read("a.md").unwrap().unwrap(), b"A2");
    assert_eq!(w.read("new.md").unwrap().unwrap(), b"N");
    assert_eq!(w.read("b.md").unwrap(), None);
    assert_eq!(w.read("c.md").unwrap(), None);
    assert_eq!(w.read("d/c2.md").unwrap().unwrap(), b"C");
    assert_eq!(
        w.fs().list_files("").unwrap(),
        vec!["a.md".to_string(), "d/c2.md".into(), "new.md".into()]
    );
    // Re-creating a tombstoned path works and un-hides it.
    w.create("b.md", b"B2").unwrap();
    assert_eq!(w.read("b.md").unwrap().unwrap(), b"B2");
    // Every mutation logged.
    let ops: Vec<(String, String)> = log.entries().into_iter().map(|e| (e.op, e.path)).collect();
    assert_eq!(
        ops,
        vec![
            ("modify".into(), "a.md".into()),
            ("create".into(), "new.md".into()),
            ("delete".into(), "b.md".into()),
            ("rename_to".into(), "d/c2.md".into()),
            ("rename_from".into(), "c.md".into()),
            ("create".into(), "b.md".into()),
        ]
    );
    // Index unchanged (it reflects the live vault).
    let q = w.index().query();
    assert_eq!(q.file_count().unwrap(), before_files);
    assert_eq!(q.file("a.md").unwrap(), a_row);
    assert!(q.file("new.md").unwrap().is_none());
    assert!(q.file("c.md").unwrap().is_some());
    // Tombstones survive a reopen.
    drop(w);
    let w = fx.shadow(scratch.path(), MemoryShadowLog::default());
    assert_eq!(w.read("c.md").unwrap(), None);
}

#[test]
fn shadow_state_db_log() {
    let fx = Fx::new(&[("a.md", "A")]);
    let scratch = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let db = cv_state::StateDb::open(
        &state.path().join("state.db"),
        1,
        cv_state::QueueConfig::default(),
    )
    .unwrap();
    let fs = VaultFs::shadow(
        fx.vault.path(),
        scratch.path(),
        &[fx.prot.path().to_path_buf()],
        Box::new(db),
    )
    .unwrap();
    let mut w = cv_vault::VaultWriter::open(fs, &fx.base()).unwrap();
    w.fs_mut().set_job_id(Some("job-1".into()));
    w.write("a.md", b"A2", Some(h("A"))).unwrap();
    drop(w);
    let r = cv_state::StateReader::open(&state.path().join("state.db")).unwrap();
    let rows = r.shadow_writes(0, 0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, "a.md");
    assert_eq!(rows[0].job_id.as_deref(), Some("job-1"));
    assert_eq!(rows[0].before_hash.as_deref(), Some(h("A").as_str()));
}
