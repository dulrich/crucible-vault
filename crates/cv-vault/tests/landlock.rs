use cv_vault::{LandlockStatus, restrict_shadow};

#[test]
fn landlock_makes_live_root_read_only_on_thread() {
    let live = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    std::fs::write(live.path().join("a.md"), "A").unwrap();
    let (lp, sp) = (live.path().to_path_buf(), scratch.path().to_path_buf());
    let status = std::thread::spawn(move || {
        let status = restrict_shadow(&lp, std::slice::from_ref(&sp)).unwrap();
        if status == LandlockStatus::NotEnforced {
            eprintln!("landlock not enforced on this kernel: skipping assertions");
            return status;
        }
        let err = std::fs::write(lp.join("b.md"), "x").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
        let err = std::fs::write(lp.join("a.md"), "x").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
        assert_eq!(std::fs::read_to_string(lp.join("a.md")).unwrap(), "A");
        std::fs::write(sp.join("ok.md"), "x").unwrap();
        status
    })
    .join()
    .unwrap();
    eprintln!("landlock status: {status:?}");
    // The restriction stayed on that thread.
    std::fs::write(live.path().join("after.md"), "x").unwrap();
}

#[test]
fn landlock_refuses_writable_overlapping_live_root() {
    let live = tempfile::tempdir().unwrap();
    let inner = live.path().join("scratch");
    std::fs::create_dir(&inner).unwrap();
    assert!(restrict_shadow(live.path(), &[inner]).is_err());
}
