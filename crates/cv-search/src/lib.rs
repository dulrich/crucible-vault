//! The search-companion feed (plan `v0-core-daemon` WP-13): a byte-compatible port of
//! Crucible's chunker and file preparation, the companion HTTP client (split into a read-only
//! and a live-only mutating half), the local-vs-remote comparison WP-14's parity check reuses,
//! and the `search_upsert` / reconcile / `search_delete` job bodies.
//!
//! Feature reference: `https://github.com/dulrich/obsidian-crucible/blob/master/src/search/{chunker,filePreparation,client}.ts`,
//! `docs/search-companion.md`. Per-module docs cite the exact lines and what was dropped.

pub mod chunker;
pub mod client;
pub mod compare;
pub mod error;
pub mod feed;
pub mod js;
pub mod prepare;

pub use chunker::{
    BuildChunksInput, SearchChunk, build_search_chunks, hash_search_content, hash_string,
    is_search_indexable_path,
};
pub use client::{
    CompanionReader, CompanionWriter, DEFAULT_COMPANION_URL, FileState, Health, IndexedPath,
    LiveProof, REQUIRED_SCHEMA_VERSION,
};
pub use compare::{CompareCounts, CompareReport, EmbedCheck, RemoteEntry, compare};
pub use error::{SearchError, UnavailableKind};
pub use feed::{EmbedTarget, FeedConfig, FeedReport, LocalNote, Mode};
pub use prepare::{
    EmbedRef, LinkedSource, PrepareHost, PrepareInput, PreparedFile, build_prepared_chunks,
    prepare_search_file,
};
