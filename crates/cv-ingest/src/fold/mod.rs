//! The fold: the `> [!removed]-` callout of removed spans that [`crate::enrich`] writes, its
//! layout (metadata codec, parse, render) and the review operations on it (resolve, trim, fold
//! ranges).

mod layout;
mod meta;
mod resolve;
#[cfg(test)]
mod tests;
mod trim;

pub(crate) use layout::{FOLD_HEADER, closing, quote, write_fold};
pub use layout::{FOLD_KIND, FOLD_MARKER};
pub(crate) use meta::{Anchor, anchor_of, parse_meta, render_meta};
pub use resolve::{
    FoldError, FoldGroup, GroupTarget, Placement, Resolution, ResolvedGroup, Verdict, fold_groups,
    removed_spans, resolve_groups,
};
pub use trim::{FoldRange, Trimmed, fold_ranges, trim_lines};
