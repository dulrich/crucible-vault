//! `cv-index` — the derived, rebuildable vault index (`index.db`), per WP-5 of
//! `plans/v0-core-daemon.md` and S2's measured design (`runs/spike/core-index/FINDINGS.md`,
//! "Index design" and "Recommendation").
//!
//! - [`Index`] is the single writer: full atomic [`Index::rebuild`] under a generation number,
//!   and incremental [`Index::upsert_file`] / [`Index::remove_file`] / [`Index::rename_file`]
//!   with an in-memory [`Resolver`] kept in sync with `files`.
//! - [`Reader`] is the read-only client handle (`SQLITE_OPEN_READ_ONLY`); both expose the same
//!   [`Query`] API (backlinks, outgoing links, tags, props, folders, tasks, unresolved, FTS).
//!
//! Pure and sync: no async runtime (AGENTS.md → Stack).

mod error;
mod extract;
mod index;
mod query;
mod resolver;
mod schema;

pub use error::{Error, Result};
pub use extract::{is_parsed_path, is_skipped_path, is_target_only_path};
pub use index::Index;
pub use query::{
    BlockRow, FileRow, FtsHit, LinkKind, LinkRow, PropRow, Query, Reader, TagCount, TagRow, TaskRow,
};
pub use resolver::Resolver;
pub use schema::SCHEMA_VERSION;
