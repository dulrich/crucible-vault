//! Landlock for shadow mode (plan WP-7, "Landlock in shadow mode"): belt-and-braces on top of
//! [`crate::VaultFs`]'s typed Live/Shadow split, making the live vault read-only for the process.

use std::path::{Path, PathBuf};

use landlock::{
    ABI, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    path_beneath_rules,
};

use crate::error::{Result, VaultError};

/// How much of the requested Landlock restriction the kernel enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandlockStatus {
    /// Every requested restriction is enforced.
    Enforced,
    /// Best-effort: an older ABI enforced a subset.
    PartiallyEnforced,
    /// The kernel has no (usable) Landlock: nothing is enforced; the caller logs it.
    NotEnforced,
}

/// Restricts the **calling thread and its future children** to: read (and execute) everywhere,
/// read-write only beneath `writable` (scratch, state/index dirs, socket dir, config dir — the
/// caller supplies them; each must exist). Nothing under `live_root` is writable: a `writable`
/// entry equal to/under `live_root` (or containing it) is refused.
///
/// Call it on the daemon's main thread **before spawning any other thread**: Landlock applies to
/// the calling thread and threads/processes it creates afterwards, never to threads that already
/// exist. Best-effort ABI (V5): never panics on old kernels — it reports
/// [`LandlockStatus::NotEnforced`].
pub fn restrict_shadow(live_root: &Path, writable: &[PathBuf]) -> Result<LandlockStatus> {
    let live = std::fs::canonicalize(live_root).unwrap_or_else(|_| live_root.to_path_buf());
    for w in writable {
        let cw = std::fs::canonicalize(w).unwrap_or_else(|_| w.clone());
        if cw.starts_with(&live) || live.starts_with(&cw) {
            return Err(VaultError::Landlock(format!(
                "writable path {} overlaps the live root {}",
                cw.display(),
                live.display()
            )));
        }
    }
    let abi = ABI::V5;
    let err = |e: landlock::RulesetError| VaultError::Landlock(e.to_string());
    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .map_err(err)?
        .create()
        .map_err(err)?
        .add_rules(path_beneath_rules(["/"], AccessFs::from_read(abi)))
        .map_err(err)?
        .add_rules(path_beneath_rules(writable, AccessFs::from_all(abi)))
        .map_err(err)?
        .restrict_self()
        .map_err(err)?;
    Ok(match status.ruleset {
        RulesetStatus::FullyEnforced => LandlockStatus::Enforced,
        RulesetStatus::PartiallyEnforced => LandlockStatus::PartiallyEnforced,
        RulesetStatus::NotEnforced => LandlockStatus::NotEnforced,
    })
}
