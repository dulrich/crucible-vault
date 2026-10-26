//! OFM (comrak AST) → render-model lowering for the reading pane (WP-3).
//!
//! One parser (D12): the body is parsed with `ofm::comrak_options()` plus
//! `extension.footnotes = true` (plan "Dependency posture", audit M3), and wiki-links are matched
//! with `ofm::wikilink::match_wikilink` (the shared grammar). Frontmatter is split with
//! `ofm::frontmatter::split_frontmatter` and parsed into [`RenderModel::properties`] (the
//! Reading properties block, [`crate::props`]), never into the body. OFM forms comrak does not
//! model (callouts, `==highlight==`, `#tags`, `$$` display math, `![[embeds]]`) are recognised on
//! the lowered tree, the same way `ofm::parse` recognises them over comrak's output.
//!
//! The model is a pure tree: no iced, no I/O. Transclusion content is filled by
//! [`transclude`] with a caller-supplied fetch (the editor resolves + reads), depth 1 only.
//!
//! Layout: [`model`] (the tree types),
//! [`lower`](mod@lower) (comrak → model), [`transclude`](mod@transclude) (depth-1 embeds),
//! [`query`] (walks: tasks, links, images, inline text). Source-text edits live in
//! [`crate::edits`].

pub mod lower;
pub mod model;
pub mod query;
pub mod transclude;

pub use lower::{is_image_target, is_remote, lower, lower_with};
pub use model::*;
pub use query::{images, inline_text, link_targets, pdf_embeds, task_lines};
pub use transclude::transclude;

#[cfg(test)]
mod tests;
