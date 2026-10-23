//! `cv-ingest`: pure parsing and note-building helpers for the A2 automation pipelines (plan
//! `v0-automation-pipelines`). No async runtime, no HTTP, no GUI: callers fetch, this crate parses.
//!
//! WP-1 lays the foundations: URL extraction ([`urls`]), RSS/Atom feeds ([`feed`]) and
//! HTML to Markdown ([`html`]). Later WPs add modules (one `mod` line each).

pub mod access;
pub mod blogs;
pub mod boilerplate;
pub mod brief;
pub mod feed;
pub mod fm;
pub mod html;
pub mod mail;
pub mod signals;
pub mod supersede;
pub mod tracker;
pub mod urls;
pub mod x;
pub mod youtube;

pub use feed::{FeedError, FeedItem, parse_feed};
pub use html::{html_to_markdown, word_count};
pub use urls::{extract_canonical_urls, extract_urls};
