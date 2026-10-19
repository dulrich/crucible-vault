//! Semantic search for cv (plan `v0-core-daemon` WP-13; plan semsearch-crate WP-3, D22): a
//! native-`str` port of Crucible's chunker and file preparation, the `search_upsert` /
//! reconcile / `search_delete` job bodies writing the in-process `semsearch` index, and the
//! daemon's shared index handle.
//!
//! Feature reference: `https://github.com/dulrich/obsidian-crucible/blob/master/src/search/{chunker,filePreparation}.ts`. Per-module
//! docs cite the exact lines and what was dropped.

pub mod chunker;
pub mod error;
pub mod feed;
pub mod index;
pub mod prepare;

pub use chunker::{
    BuildChunksInput, SearchChunk, build_search_chunks, hash_search_content, hash_string,
    is_search_indexable_path,
};
pub use error::SearchError;
pub use feed::{
    EmbedTarget, FeedConfig, FeedCounts, FeedReport, LocalNote, ShadowScope, space_for, space_model,
};
pub use index::{SearchDb, SharedIndex};
pub use prepare::{
    EmbedRef, LinkedSource, PrepareHost, PrepareInput, PreparedFile, build_prepared_chunks,
    prepare_search_file,
};
pub use semsearch;
