//! The `fanfic_title` dry-run review note: what a backfill found and its pure renderer (plan
//! `tn-review-2026-11-10-remediation` WP-R8, review S3; the path, the write and the link rule
//! are [`super::review_note`]'s).

use cv_ingest::rename_plan::Withheld;
use cv_text::vpath;

use super::fanfic_title::BackfillVars;
use super::review_note::link;

/// Review note name suffix (`<kind><suffix><date>.md`, e.g. `fanfic-titles-2026-11-08.md`).
pub(super) const REVIEW_SUFFIX: &str = "-titles-";

/// What a dry run found, in path order.
#[derive(Debug, Default)]
pub(super) struct Findings {
    /// `(old path, new path)`.
    pub(super) rename: Vec<(String, String)>,
    pub(super) collision: Vec<(String, String, Withheld)>,
    pub(super) undetected: Vec<String>,
    /// Already named right, with a stale `title`.
    pub(super) retitle: Vec<String>,
}

fn section(out: &mut String, title: &str, lines: Vec<String>) {
    out.push_str(&format!("\n## {title} ({})\n\n", lines.len()));
    if lines.is_empty() {
        out.push_str("- (none)\n");
    }
    for l in lines {
        out.push_str(&format!("- {l}\n"));
    }
}

/// `fanfic` → `Fanfic` (the review heading).
fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| {
        f.to_uppercase().chain(c).collect::<String>()
    })
}

/// The pure review-note renderer.
pub(super) fn render_review(kind: &str, date: &str, v: &BackfillVars, f: &Findings) -> String {
    let base = |p: &str| vpath::stem(p).to_owned();
    let mut out = format!(
        "# {} title backfill \u{2014} dry run {date}\n\nScanned {}, already {}, would rename {}, would retitle {}, collision {}, undetected {}.\n\nApply: run this backfill again with Dry run off.\n",
        capitalized(kind),
        v.scanned,
        v.already,
        v.would_rename,
        v.would_retitle,
        v.collision,
        v.undetected
    );
    section(
        &mut out,
        "Would rename",
        f.rename
            .iter()
            .map(|(o, n)| format!("{} \u{2192} {}", link(o, true), base(n)))
            .collect(),
    );
    section(
        &mut out,
        "Would retitle",
        f.retitle.iter().map(|o| link(o, true)).collect(),
    );
    section(
        &mut out,
        "Collision",
        f.collision
            .iter()
            .map(|(o, n, w)| format!("{} \u{2192} {} ({})", link(o, true), base(n), w.label()))
            .collect(),
    );
    section(
        &mut out,
        "Undetected",
        f.undetected.iter().map(|o| link(o, true)).collect(),
    );
    out
}
