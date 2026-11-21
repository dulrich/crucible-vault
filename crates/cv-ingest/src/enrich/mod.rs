//! Intake auto-enrich model (plan `intake-auto-enrich` WP-2; measurements in
//! `docs/spikes/auto-intake-llm/FINDINGS.md`): parse the model's JSON answer, resolve its text
//! anchors against the note body, and render the edit — optional `## <chapter>` headings and one
//! `> [!removed]-` fold callout at the end of the body. The top `> [!abstract]-` block is no longer
//! written (plan abstract-removal-confidence-deletion WP-2, Decision 1); a legacy one is removed by
//! every enrich write ([`enrich`]'s strip, [`strip_top`]).
//!
//! Pure: no I/O, no clock. The daemon step (WP-3) owns provider calls, `enriched:` frontmatter
//! and the forced/non-forced skip decision; banner detection is WP-5's.
//!
//! The module is split along its seams: `model` (answer types and parse), `body` (generated-block
//! strip), `pass` (the phases) and, in [`crate::fold`], the fold layout and review operations.

mod body;
mod model;
mod pass;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

use body::strip_generated;
pub use body::{has_legacy_top, strip_structure, strip_top};
pub use model::{
    BannerRef, Chapter, EnrichEdit, EnrichError, EnrichOpts, EnrichOutput, EnrichReport, Span,
    SpanKind, parse, squeeze,
};
use pass::{assemble, banners_and_cap, plan_groups, plan_headings, render_fold, resolve_fold};

// The fold layout and review operations live in [`crate::fold`]; re-exported so every
// `cv_ingest::enrich::…` path keeps resolving.
pub use crate::fold::{
    FOLD_KIND, FOLD_MARKER, FoldError, FoldGroup, FoldRange, GroupTarget, Placement, Resolution,
    ResolvedGroup, Trimmed, Verdict, fold_groups, fold_ranges, removed_spans, resolve_groups,
    trim_lines,
};

/// Marker closing a generated block ([`cv_text::blocks::ENRICH_CLOSE`]).
pub use cv_text::blocks::ENRICH_CLOSE as MARK_CLOSE;
/// Marker line preceding an inserted chapter heading ([`cv_text::blocks::ENRICH_HEADING`]).
pub use cv_text::blocks::ENRICH_HEADING as MARK_HEADING;
/// Marker opening a generated block ([`cv_text::blocks::ENRICH_OPEN`]).
pub use cv_text::blocks::ENRICH_OPEN as MARK_OPEN;

/// Apply `output` to the frontmatter-free note `body` (the caller reattaches its frontmatter).
/// Re-running on an enriched note replaces the generated blocks (a legacy top block is dropped,
/// not re-rendered) and keeps prior folds. Runs the phases in order — resolve the fold, add
/// banners under the half-body cap, plan headings, render the fold block, assemble — and merges
/// their reports.
pub fn enrich(
    body: &str,
    output: &EnrichOutput,
    banners: &[BannerRef],
    opts: EnrichOpts,
) -> EnrichEdit {
    let st = strip_generated(body);
    let lines = &st.lines;
    let sq: Vec<String> = lines.iter().map(|l| squeeze(l)).collect();
    let mut rep = EnrichReport::default();

    let prior: Vec<String> = st
        .prior_fold
        .iter()
        .map(|l| squeeze(crate::fold::unquote(l)))
        .collect();
    let (spans, mut kinds, resolved) = resolve_fold(output, &sq, &prior);
    rep.merge(&resolved);
    let (fold, capped) = banners_and_cap(spans, &mut kinds, banners, &sq);
    rep.merge(&capped);
    let plan = plan_headings(output, opts, lines, &sq, &fold);
    rep.merge(&plan.report);
    let (fold_block, fold_rep) = render_fold(
        &plan_groups(lines, &sq, &fold, &kinds),
        &st.prior_meta,
        &st.prior_fold,
        lines,
    );
    rep.merge(&fold_rep);

    if !st.had_generated && fold_block.is_empty() && plan.inserts.is_empty() {
        return EnrichEdit {
            text: body.to_owned(),
            report: rep,
        };
    }
    EnrichEdit {
        text: assemble(lines, &fold, &plan.inserts, fold_block),
        report: rep,
    }
}

impl EnrichReport {
    /// Adds `other`'s counters and flags to `self` (each phase reports only its own fields).
    fn merge(&mut self, other: &Self) {
        self.anchors_resolved += other.anchors_resolved;
        self.anchors_dropped += other.anchors_dropped;
        self.anchors_already_folded += other.anchors_already_folded;
        self.spans_folded += other.spans_folded;
        self.lines_folded += other.lines_folded;
        self.banners_folded += other.banners_folded;
        self.fold_refused_over_half |= other.fold_refused_over_half;
        self.headings_inserted += other.headings_inserted;
    }
}
