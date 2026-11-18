//! Plan `intake-removals-sources-rescan` WP-6: the Delete / Keep controls Reading draws on the
//! `[!removed]` fold. The groups come from `cv_ingest::enrich::fold_groups` (the one fold parse,
//! stored on the [`super::Doc`]); this module is the pure part — one [`FoldRow`] per group with
//! its kind label, a short preview and the two [`RemovalOp`]s its buttons send — so the drawn
//! controls and the requests they make are testable without a renderer.

use cv_ingest::enrich::FoldGroup;
use cv_protocol::{RemovalOp, RemovalVerdict};

/// The longest preview shown for a group (characters).
const PREVIEW_CHARS: usize = 60;

/// One group's controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldRow {
    /// The group's identity (`FoldGroup::id`).
    pub id: String,
    /// The kind label (`sponsor` | `cta` | `chrome` | `footer` | `banner`…); `None` for an
    /// old-layout group, which shows no kind label (a neutral label would claim a kind it lacks).
    pub kind: Option<String>,
    /// How many original lines the group holds.
    pub lines: usize,
    /// The group's first non-blank line, cut to [`PREVIEW_CHARS`] (empty for an all-blank group).
    pub preview: String,
    pub delete: RemovalOp,
    pub keep: RemovalOp,
}

/// The rows of `groups`, in order.
#[must_use]
pub fn rows(groups: &[FoldGroup]) -> Vec<FoldRow> {
    groups
        .iter()
        .map(|g| {
            let first = g.lines.iter().map(|l| l.trim()).find(|l| !l.is_empty());
            let mut preview: String = first
                .unwrap_or_default()
                .chars()
                .take(PREVIEW_CHARS)
                .collect();
            if first.is_some_and(|f| f.chars().count() > PREVIEW_CHARS) {
                preview.push('…');
            }
            let op = |verdict| RemovalOp::Group {
                group: g.id.clone(),
                verdict,
            };
            FoldRow {
                id: g.id.clone(),
                kind: g.kind.as_ref().map(|k| k.as_str().to_owned()),
                lines: g.lines.len(),
                preview,
                delete: op(RemovalVerdict::Delete),
                keep: op(RemovalVerdict::Keep),
            }
        })
        .collect()
}

/// What the fold title's "Delete all" sends.
#[must_use]
pub fn delete_all() -> RemovalOp {
    RemovalOp::All {
        verdict: RemovalVerdict::Delete,
    }
}

#[cfg(test)]
mod tests {
    use cv_ingest::enrich::{FoldRange, SpanKind, fold_groups, fold_ranges};

    use super::*;

    /// Requirement: two groups draw two Delete/Keep pairs with the right kind labels and group
    /// ids, plus Delete all; an old-layout group shows no kind label.
    #[test]
    fn rows_carry_kind_ids_and_ops() {
        let body = "intro\n\nSponsor one\nSponsor two\n\nmiddle\n\nSubscribe now\n\nend\n";
        let body = fold_ranges(
            body,
            &[
                FoldRange {
                    first: 2,
                    last: 3,
                    kind: SpanKind::Sponsor,
                },
                FoldRange {
                    first: 7,
                    last: 7,
                    kind: SpanKind::Cta,
                },
            ],
        )
        .expect("fold");
        let groups = fold_groups(&body);
        let r = rows(&groups);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].kind.as_deref(), Some("sponsor"));
        assert_eq!(r[1].kind.as_deref(), Some("cta"));
        assert_eq!((r[0].lines, r[0].preview.as_str()), (2, "Sponsor one"));
        for (row, g) in r.iter().zip(&groups) {
            assert_eq!(row.id, g.id);
            assert_eq!(
                row.delete,
                RemovalOp::Group {
                    group: g.id.clone(),
                    verdict: RemovalVerdict::Delete
                }
            );
            assert_eq!(
                row.keep,
                RemovalOp::Group {
                    group: g.id.clone(),
                    verdict: RemovalVerdict::Keep
                }
            );
        }
        assert_eq!(
            delete_all(),
            RemovalOp::All {
                verdict: RemovalVerdict::Delete
            }
        );
        let old =
            fold_groups("x\n\n%% cv-enrich %%\n> [!removed]-\n> old line\n\n%% /cv-enrich %%\n");
        assert_eq!(rows(&old)[0].kind, None);
    }
}
