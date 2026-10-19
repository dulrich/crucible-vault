//! The protected-root + cut-over-marker guard (I1): live mode on (or above) the protected vault
//! is refused without the marker. Re-homed from the retired `tests/shadow.rs` (plan
//! tn-review-baseline-remediation WP-R6); every root here is a temp dir, never the live vault.

use cv_vault::{CutoverMarker, NoCutover, VaultError, VaultFs};

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
