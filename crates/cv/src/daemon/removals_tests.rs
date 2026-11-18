//! Pure half of `resolve_removals` (plan intake-removals-sources-rescan WP-4): label rows per
//! verdict / origin over synthetic bodies.

use cv_protocol::{RemovalKind, RemovalOp, RemovalVerdict};

use super::apply;

/// A new-layout fold: one `sponsor` group anchored at the start, two lines.
pub(crate) const BODY: &str = "Intro paragraph of the synthetic post.\n\n%% cv-enrich %%\n%% cv-fold lines=2 kind=sponsor at=start gap=0 join=0 %%\n> [!removed]-\n> Sponsored by the synthetic widget company\n> ![[media/ad_MD5.png]]\n\n%% /cv-enrich %%\n";

#[test]
fn delete_and_keep_label_each_non_blank_group_line() {
    let g = cv_ingest::enrich::fold_groups(BODY);
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].kind.as_ref().map(|k| k.as_str()), Some("sponsor"));
    let del = apply(
        BODY,
        &RemovalOp::Group {
            group: g[0].id.clone(),
            verdict: RemovalVerdict::Delete,
        },
    )
    .unwrap();
    assert_eq!(del.groups, 1);
    assert!(!del.body.contains("widget"));
    let rows: Vec<_> = del
        .labels
        .iter()
        .map(|l| (l.kind.as_str(), l.verdict.as_str(), l.origin.as_str()))
        .collect();
    assert_eq!(rows, [("sponsor", "delete", "fold"); 2]);
    assert_eq!(del.deleted.len(), 2);

    let keep = apply(
        BODY,
        &RemovalOp::All {
            verdict: RemovalVerdict::Keep,
        },
    )
    .unwrap();
    assert!(keep.body.contains("widget") && !keep.body.contains("[!removed]"));
    assert!(
        keep.labels
            .iter()
            .all(|l| l.verdict == RemovalVerdict::Keep)
    );
    assert!(keep.deleted.is_empty(), "keep deletes nothing");
    assert_eq!(keep.at_anchor + keep.at_end, 2);
}

#[test]
fn trim_labels_selection_and_unknown_group_is_refused() {
    let body = "keep me here\nsubscribe to the synthetic list\n\nshare this post now\nend\n";
    let t = apply(
        body,
        &RemovalOp::Trim {
            first: 1,
            last: 3,
            kind: Some(RemovalKind::Cta),
        },
    )
    .unwrap();
    assert_eq!(t.body, "keep me here\nend\n");
    let rows: Vec<_> = t
        .labels
        .iter()
        .map(|l| {
            (
                l.kind.as_str(),
                l.verdict.as_str(),
                l.origin.as_str(),
                l.line.as_str(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                "cta",
                "delete",
                "selection",
                "subscribe to the synthetic list"
            ),
            ("cta", "delete", "selection", "share this post now"),
        ],
        "the blank line is not labelled"
    );
    let e = apply(
        BODY,
        &RemovalOp::Group {
            group: "nope".into(),
            verdict: RemovalVerdict::Delete,
        },
    )
    .err()
    .unwrap();
    assert_eq!(
        super::refusal_of(&e),
        cv_protocol::RemovalRefusal::UnknownGroup
    );
}
