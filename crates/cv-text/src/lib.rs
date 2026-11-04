//! Pure text helpers for crucible-vault: the template token engine, Moment-format translation,
//! markdown section editing, attachment-path planning, vault-path helpers and link resolution, next-daily-date resolution, URL canonicalization and near-duplicate text similarity (shingle containment).
//!
//! Sync, I/O-free and clock-free: every time-dependent function takes its `now` as an argument.

pub mod attachments;
pub mod blocks;
mod civil;
pub mod file_kind;
mod md_refs;
pub mod moment;
pub mod remote_media;
pub mod resolver;
pub mod sections;
pub mod similar;
pub mod template;
pub mod url_canon;
pub mod vpath;
pub mod wikilink;

pub use civil::days_from_civil;
pub use moment::{
    MomentFormatError, format_moment, moment_to_strftime, parse_moment_date_strict,
    resolve_next_daily_date,
};
pub use remote_media::{
    HtmlMedia, HtmlMediaTag, RemoteKind, VideoProvider, classify_remote, scan_html_media,
};
pub use sections::{
    SectionInsertMode, SectionRange, find_section_range, insert_into_section, is_section_empty,
};
pub use template::{
    AttachmentTemplateContext, TemplateContext, TemplateError, apply_attachment_template,
    apply_template, collapse_whitespace, fold_nfkd_ascii, rewrite_input_alias, slugify,
};
pub use url_canon::{
    CanonMethod, CanonicalizedUrl, PostIdOptions, TrackedSourceHint, XStatusRef,
    build_blog_canon_host_map, canonical_x_status_url, canonicalize_url, extract_video_id_from_url,
    extract_x_status_from_url, is_youtube_video, post_id_from_url, short_hash, slug_for_canonical,
};
