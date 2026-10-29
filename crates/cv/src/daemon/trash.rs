//! Trash glue (plan tidiness-trash-orphans WP-1): the writer's trash records reach the
//! `trash_entries` ledger, and `vault.toml` `[trash] location` reaches the writer.
//!
//! The writer thread hands each record to a channel (it never takes the state lock, so a caller
//! holding the state lock across a writer call cannot deadlock); a small std thread persists them.
//! A crash between the move and the insert leaves only an unrecorded file (WP-2 seeds/adopts it).

use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use cv_actions::TrashLocation;
use cv_state::{NewTrashEntry, StateDb};
use cv_vault::{SystemTrash, TrashRecord, TrashTarget, VaultWriter};

use super::Shared;
use crate::sync::lock;

/// The ledger row for `r`: the one record-to-row conversion (the writer's moves here, tidy's
/// seed/adopt rows via a [`TrashRecord`] too; plan tn-review-2026-10-26-remediation WP-R6).
pub(crate) fn entry_of(r: &TrashRecord) -> NewTrashEntry {
    NewTrashEntry {
        location: r.moved.location,
        trash_path: r.moved.trash_path.clone(),
        original_path: r.original_path.clone(),
        trashed_ms: r.trashed_ms,
        bytes: i64::try_from(r.moved.identity.bytes).unwrap_or(i64::MAX),
        reason: r.reason,
        mtime_ms: Some(r.moved.identity.mtime_ms),
        inode: i64::try_from(r.moved.identity.inode).ok(),
        info_name: r.info_name(),
    }
}

/// Installs the ledger sink on `writer`: records go over a channel to a persisting thread that
/// ends when the writer (the sender) is dropped.
pub(super) fn install_ledger(writer: &mut VaultWriter, state: Arc<Mutex<StateDb>>) {
    let (tx, rx) = std::sync::mpsc::channel::<TrashRecord>();
    let spawned = std::thread::Builder::new()
        .name("cv-trash-ledger".into())
        .spawn(move || {
            for r in rx {
                if let Err(e) = lock(&state).insert_trash_entry(&entry_of(&r)) {
                    tracing::warn!(error = %e, "recording a trash move");
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "trash ledger thread not started; moves stay unrecorded");
        return;
    }
    writer.set_trash_sink(Box::new(move |r| {
        if tx.send(r).is_err() {
            tracing::warn!("trash ledger gone; move unrecorded");
        }
    }));
}

/// The writer target for `location` (`system` needs a data home; without one: vault + warning).
pub(crate) fn target_for(location: TrashLocation, data_home: Option<&Path>) -> TrashTarget {
    match (location, data_home) {
        (TrashLocation::Vault, _) => TrashTarget::Vault,
        (TrashLocation::System, Some(home)) => TrashTarget::System(SystemTrash::new(
            home,
            Arc::new(|| chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()),
        )),
        (TrashLocation::System, None) => {
            tracing::warn!("trash location = system but no XDG_DATA_HOME/HOME; using the vault");
            TrashTarget::Vault
        }
    }
}

/// Applies the runner's current `vault.toml` trash location to the writer.
pub(super) fn apply_trash_config(shared: &Shared) {
    let Some(runner) = shared.runner() else {
        return;
    };
    let location = runner.config().vault().trash.location;
    let target = target_for(location, shared.config.data_home.as_deref());
    if let Some(w) = shared.writer()
        && let Err(e) = w.call(move |w| w.fs_mut().set_trash_target(target))
    {
        tracing::warn!(error = %e, "applying the trash location");
    }
}

/// The system trash root (`<data_home>/Trash`), if a data home is known.
fn system_root(data_home: Option<&Path>) -> Option<PathBuf> {
    data_home.map(|h| h.join("Trash"))
}

/// Deletes ledger rows whose trash file is gone; the one function the daemon start, tidy and the
/// trash watcher share. Returns the rows deleted (failures are logged, never fatal).
pub(crate) fn prune_ledger(
    state: &Mutex<StateDb>,
    vault_root: &Path,
    data_home: Option<&Path>,
) -> usize {
    let sys = system_root(data_home);
    match lock(state).prune_missing_trash(vault_root, sys.as_deref()) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "pruning the trash ledger");
            0
        }
    }
}

/// Debounce window: a burst of trash events (an emptied bin) prunes once, after it settles.
const PRUNE_DEBOUNCE: Duration = Duration::from_millis(1500);

/// Daemon start: prunes the ledger once, then watches the trash folders (vault `.trash`, system
/// `Trash/files`) with a dedicated debounced watcher that only prunes -- trash events never reach
/// the vault watcher, note indexing or event starters. A folder that does not exist yet is not
/// watched (the start and tidy prunes cover it).
pub(super) fn start_prune(shared: &Arc<Shared>) {
    let n = prune_ledger(
        &shared.state,
        &shared.vault_root,
        shared.config.data_home.as_deref(),
    );
    if n > 0 {
        tracing::info!(rows = n, "pruned trash ledger rows without a file");
    }
    let dirs: Vec<PathBuf> = [
        Some(shared.vault_root.join(".trash")),
        system_root(shared.config.data_home.as_deref()).map(|r| r.join("files")),
    ]
    .into_iter()
    .flatten()
    .filter(|d| d.is_dir())
    .collect();
    if dirs.is_empty() {
        return;
    }
    let weak = Arc::downgrade(shared);
    let spawned = std::thread::Builder::new()
        .name("cv-trash-watch".into())
        .spawn(move || watch_loop(&dirs, &weak));
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "trash watcher not started");
    }
}

fn watch_loop(dirs: &[PathBuf], shared: &Weak<Shared>) {
    use notify::Watcher;
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let handler = move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = tx.send(());
        }
    };
    let mut watcher = match notify::recommended_watcher(handler) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(error = %e, "trash watcher unavailable");
            return;
        }
    };
    let mut watching = 0;
    for d in dirs {
        match watcher.watch(d, notify::RecursiveMode::Recursive) {
            Ok(()) => watching += 1,
            Err(e) => tracing::warn!(error = %e, "watching a trash folder"),
        }
    }
    if watching == 0 {
        return;
    }
    // Wake periodically so the thread ends once the daemon is gone.
    loop {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(()) => {
                // Settle: drain until the folder is quiet for the debounce window.
                while rx.recv_timeout(PRUNE_DEBOUNCE).is_ok() {}
                let Some(sh) = shared.upgrade() else { return };
                prune_ledger(&sh.state, &sh.vault_root, sh.config.data_home.as_deref());
            }
            Err(RecvTimeoutError::Timeout) => {
                if shared.strong_count() == 0 {
                    return;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cv_vault::{FileIdentity, TrashMove, TrashReason};

    #[test]
    fn entry_of_maps_every_field() {
        let r = TrashRecord {
            moved: TrashMove {
                location: TrashLocation::System,
                trash_path: "a.png".into(),
                identity: FileIdentity {
                    bytes: 9,
                    mtime_ms: 7,
                    inode: 5,
                },
            },
            original_path: "x/a.png".into(),
            trashed_ms: 11,
            reason: TrashReason::NoteAttachment,
        };
        let e = entry_of(&r);
        assert_eq!(e.location, TrashLocation::System);
        assert_eq!(e.reason, TrashReason::NoteAttachment);
        assert_eq!(e.info_name.as_deref(), Some("a.png.trashinfo"));
        assert_eq!((e.bytes, e.mtime_ms, e.inode), (9, Some(7), Some(5)));
        assert!(matches!(
            target_for(TrashLocation::System, None),
            TrashTarget::Vault
        ));
    }

    /// The daemon-start / tidy / watcher call: a vault row whose file is gone is pruned, a
    /// present one kept.
    #[test]
    fn prune_ledger_drops_only_missing_rows() {
        let vault = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(vault.path().join(".trash")).unwrap();
        std::fs::write(vault.path().join(".trash/here.md"), "x").unwrap();
        let db = tempfile::tempdir().unwrap();
        let mut s = StateDb::open(
            &db.path().join("state.db"),
            1,
            cv_state::QueueConfig::default(),
        )
        .unwrap();
        for n in ["here.md", "gone.md"] {
            s.insert_trash_entry(&NewTrashEntry {
                location: TrashLocation::Vault,
                trash_path: format!(".trash/{n}"),
                original_path: n.into(),
                trashed_ms: 1,
                bytes: 1,
                reason: TrashReason::Deleted,
                mtime_ms: None,
                inode: None,
                info_name: None,
            })
            .unwrap();
        }
        let state = Mutex::new(s);
        assert_eq!(prune_ledger(&state, vault.path(), None), 1);
        let left = lock(&state).trash_entries().unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].entry.trash_path, ".trash/here.md");
    }
}
