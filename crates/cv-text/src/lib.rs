//! Pure text helpers for crucible-vault: the template token engine, Moment-format translation,
//! markdown section editing, attachment-path planning, vault-path helpers and link resolution, next-daily-date resolution and URL canonicalization.
//!
//! Sync, I/O-free and clock-free: every time-dependent function takes its `now` as an argument.

pub mod attachments;
pub mod moment;
pub mod resolver;
pub mod sections;
pub mod template;
pub mod url_canon;
pub mod vpath;

pub use moment::{
    MomentFormatError, format_moment, moment_to_strftime, parse_moment_date_strict,
    resolve_next_daily_date,
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
    extract_x_status_from_url, post_id_from_url, short_hash, slug_for_canonical,
};
