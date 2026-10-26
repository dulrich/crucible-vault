//! Trash glue (plan tidiness-trash-orphans WP-1): the writer's trash records reach the
//! `trash_entries` ledger, and `vault.toml` `[trash] location` reaches the writer.
//!
//! The writer thread hands each record to a channel (it never takes the state lock, so a caller
//! holding the state lock across a writer call cannot deadlock); a small std thread persists them.
//! A crash between the move and the insert leaves only an unrecorded file (WP-2 seeds/adopts it).

use std::path::Path;
use std::sync::{Arc, Mutex};

use cv_actions::TrashLocation;
use cv_state::{NewTrashEntry, StateDb};
use cv_vault::{SystemTrash, TrashRecord, TrashTarget, VaultWriter};

use super::Shared;
use crate::sync::lock;

/// The ledger row for `r`.
pub(crate) fn entry_of(r: &TrashRecord) -> NewTrashEntry {
    NewTrashEntry {
        location: r.moved.location.as_str().to_string(),
        trash_path: r.moved.trash_path.clone(),
        original_path: r.original_path.clone(),
        trashed_ms: r.trashed_ms,
        bytes: i64::try_from(r.moved.bytes).unwrap_or(i64::MAX),
        reason: r.reason.as_str().to_string(),
        mtime_ms: Some(r.moved.mtime_ms),
        inode: i64::try_from(r.moved.inode).ok(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use cv_vault::{TrashMove, TrashReason, TrashedIn};

    #[test]
    fn entry_of_maps_every_field() {
        let r = TrashRecord {
            moved: TrashMove {
                location: TrashedIn::System,
                trash_path: "a.png".into(),
                bytes: 9,
                mtime_ms: 7,
                inode: 5,
            },
            original_path: "x/a.png".into(),
            trashed_ms: 11,
            reason: TrashReason::NoteAttachment,
        };
        let e = entry_of(&r);
        assert_eq!(e.location, "system");
        assert_eq!(e.reason, "note-attachment");
        assert_eq!(e.info_name.as_deref(), Some("a.png.trashinfo"));
        assert_eq!((e.bytes, e.mtime_ms, e.inode), (9, Some(7), Some(5)));
        assert!(matches!(
            target_for(TrashLocation::System, None),
            TrashTarget::Vault
        ));
    }
}
