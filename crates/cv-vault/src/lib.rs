//! `cv-vault` — the vault writer, watcher and filesystem (WP-7 of `plans/v0-core-daemon.md`).
//!
//! - [`VaultFs`]: the only place that touches vault files; it writes the vault root.
//! - [`VaultWriter`]: content-hash CAS writes (tmp+rename, I2), recoverable delete, rename with
//!   inbound link rewrite; owns the `index.db` writer handle and keeps it in sync.
//! - [`spawn_writer`] / [`spawn_writer_with_watcher`]: one std thread owns the writer (and so the
//!   index, its single writer); external watcher events are applied on that same thread and
//!   emitted as typed [`VaultEvent`]s with echo suppression and dedupe. The initial scan emits
//!   nothing (I4).
//!
//! Std threads + `std::sync::mpsc` only — no async runtime.

mod error;
mod fs;
mod hash;
mod path;
mod rewrite;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
mod thread;
pub mod trash;
mod watcher;
mod writer;

pub use error::{Result, VaultError};
pub use fs::VaultFs;
pub use hash::ContentHash;
pub use path::validate_rel_path;
pub use thread::{WriterHandle, spawn_writer, spawn_writer_with_watcher};
pub use trash::{
    SystemTrash, SystemTrashEntry, TrashMove, TrashReason, TrashRecord, TrashTarget, TrashedIn,
    cv_owned_system_entries,
};
pub use watcher::{EventKind, Origin, VaultEvent, WatchConfig};
pub use writer::{RenameOutcome, VaultWriter};
