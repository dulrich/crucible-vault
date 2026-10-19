//! `cv-index` — the derived, rebuildable vault index (`index.db`), per WP-5 of
//! `plans/v0-core-daemon.md` and S2's measured design (`docs/spikes/core-index/FINDINGS.md`,
//! "Index design" and "Recommendation").
//!
//! - [`Index`] is the single writer: full atomic [`Index::rebuild`] under a generation number,
//!   and incremental [`Index::upsert_file`] / [`Index::remove_file`] / [`Index::rename_file`]
//!   with an in-memory [`Resolver`] kept in sync with `files`.
//! - [`Reader`] is the read-only client handle (`SQLITE_OPEN_READ_ONLY`); both expose the same
//!   [`Query`] API (backlinks, outgoing links, tags, props, folders, tasks, unresolved, FTS).
//! - Layout: `CV_INDEX_DB` is a *base* path, never itself a DB. Generation `n` is its own WAL
//!   database at [`generation_path`] (`index.db` → `index-7.db`) with its own `-wal`/`-shm`, so
//!   readers never block the writer and no WAL ever pairs with a different main file. The current
//!   and the previous generation stay on disk; older ones are unlinked (open readers keep their
//!   inode). For WP-8: `hello`'s `index_db` carries the base path and `generation` the number;
//!   clients open `Reader::open(base, generation)` and reopen on `index_generation`.
//!
//! Pure and sync: no async runtime (AGENTS.md → Stack).

mod error;
mod extract;
mod generation;
mod ignore;
mod index;
pub mod queries;
mod query;
mod schema;

pub use cv_text::resolver::Resolver;
pub use error::{Error, Result};
pub use extract::{is_parsed_path, is_skipped_path, is_target_only_path, note_tags};
pub use generation::{current_generation, generation_path};
pub use ignore::{GITIGNORE, IgnoreRules, is_gitignore_path};
pub use index::{Index, file_times, rel_path};
pub use queries::attachments::{
    OrphanRow, attachment_vault_paths, delete_preview, orphaned_attachments, referenced_elsewhere,
};
pub use query::{
    BlockRow, FileRow, FtsHit, LinkKind, LinkRow, PropRow, Query, Reader, TagCount, TagRow, TaskRow,
};
pub use schema::SCHEMA_VERSION;
