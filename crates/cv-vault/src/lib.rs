//! `cv-vault` — the vault writer, watcher and shadow filesystem (WP-7 of
//! `plans/v0-core-daemon.md`).
//!
//! - [`VaultFs`]: the only place that touches vault files. `Live` writes the vault root (refused
//!   under the protected live vault unless the cut-over marker is present, I1); `Shadow` reads the
//!   live vault through a scratch overlay and logs every would-be write to a [`ShadowLog`].
//! - [`VaultWriter`]: content-hash CAS writes (tmp+rename, I2), recoverable delete, rename with
//!   inbound link rewrite; owns the `index.db` writer handle and keeps it in sync in Live mode.
//! - [`spawn_writer`] / [`spawn_writer_with_watcher`]: one std thread owns the writer (and so the
//!   index, its single writer); external watcher events are applied on that same thread and
//!   emitted as typed [`VaultEvent`]s with echo suppression and dedupe. The initial scan emits
//!   nothing (I4).
//! - [`restrict_shadow`]: Landlock, making the live vault read-only for the calling thread and its
//!   future children in shadow mode.
//!
//! Std threads + `std::sync::mpsc` only — no async runtime.

mod error;
mod fs;
mod hash;
mod landlock_guard;
mod path;
mod rewrite;
mod thread;
mod watcher;
mod writer;

pub use error::{Result, VaultError};
pub use fs::{
    CutoverMarker, DEFAULT_PROTECTED_ROOT, DEFAULT_REPO_ROOTS, MemoryShadowLog, NoCutover,
    ShadowLog, VaultFs, validate_scratch,
};
pub use hash::ContentHash;
pub use landlock_guard::{LandlockStatus, restrict_shadow};
pub use path::validate_rel_path;
pub use thread::{WriterHandle, spawn_writer, spawn_writer_with_watcher};
pub use watcher::{EventKind, Origin, VaultEvent, WatchConfig};
pub use writer::{RenameOutcome, VaultWriter};
