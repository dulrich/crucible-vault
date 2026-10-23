//! The note model: frontmatter span + editing, lint (WP-6 of `plans/v0-core-daemon.md`). Pure: no I/O, no clock.
//!
//! - [`frontmatter`]: the canonical fence [`span`], minimal-diff `set` / `remove` / `append_to_list` over raw note text.
//! - [`lint`]: the `LINT_STEPS` registry and [`lint_note`] (one run → one edited text).
//! - [`wordcount`]: prose-only word count.
//! - [`transcript`]: YouTube transcript cleanup.
//! - [`vault`]: vault-wide property rename/remove as per-file edits over caller-supplied paths.

pub mod frontmatter;
pub mod lint;
pub mod transcript;
pub mod vault;
pub mod wordcount;

pub use frontmatter::{FmError, FrontmatterDoc, js_text, span};
pub use lint::{
    LINT_STEPS, LintContext, LintOutcome, LintSettings, LintStep, LintTarget, ParamValue,
    derive_source_ids, lint_note,
};
pub use ofm::frontmatter::JsValue;
pub use transcript::{TranscriptEvidence, cleanup_youtube_transcript, transcript_evidence};
pub use vault::{PropertyOp, PropertyOpError, VaultEditError, VaultEditPlan, plan_vault_edits};
pub use wordcount::{calculate_word_count, count_words, strip_non_prose_content};
