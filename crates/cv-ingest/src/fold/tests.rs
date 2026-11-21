//! Fold layout and review-operation tests (synthetic fixtures only).
#![allow(clippy::field_reassign_with_default)]

use super::*;
use crate::enrich::test_support::opts;
use crate::enrich::{
    EnrichOpts, EnrichOutput, MARK_CLOSE, MARK_OPEN, Span, SpanKind, enrich, strip_structure,
};
use layout::{FOLD_HEADER, parse_folds};

// ---------------------------------------------------------------------------------------------
// Fold groups (plan intake-removals-sources-rescan WP-3)
// ---------------------------------------------------------------------------------------------

fn sp(kind: SpanKind, first: &str, last: &str) -> Span {
    Span {
        kind,
        first_line: first.into(),
        last_line: last.into(),
        confidence: 1.0,
    }
}

fn fold_only(body: &str, spans: Vec<Span>) -> String {
    let mut o = EnrichOutput::default();
    o.spans = spans;
    enrich(body, &o, &[], EnrichOpts { headings: false }).text
}

/// (body, spans) round-trip fixtures: start, middle between blanks, end, a blank inside, two
/// adjacent kinds, repeated identical groups.
fn round_trip_cases() -> Vec<(&'static str, Vec<Span>)> {
    vec![
        (
            "Sponsored by Acme.\n\nReal one.\nReal two.\nReal three.\nReal four.\n",
            vec![sp(
                SpanKind::Sponsor,
                "Sponsored by Acme.",
                "Sponsored by Acme.",
            )],
        ),
        (
            "Intro a.\nIntro b.\n\nAD LINE\n\nBody c.\nBody d.\nBody e.\n",
            vec![sp(SpanKind::Chrome, "AD LINE", "AD LINE")],
        ),
        (
            "Para a.\nPara b.\nPara c.\nPara d.\n\nFollow us.\nShare this.\n",
            vec![sp(SpanKind::Cta, "Follow us.", "Share this.")],
        ),
        (
            "A1.\nA2.\nA3.\n\nPromo start.\n\nPromo end.\n\nB1.\nB2.\nB3.\nB4.\n",
            vec![sp(SpanKind::Sponsor, "Promo start.", "Promo end.")],
        ),
        (
            "C1.\nC2.\nC3.\nSponsor X.\nClick here.\nC4.\nC5.\nC6.\n",
            vec![
                sp(SpanKind::Sponsor, "Sponsor X.", "Sponsor X."),
                sp(SpanKind::Cta, "Click here.", "Click here."),
            ],
        ),
        (
            "Same.\nAd one.\nSame.\nAd one.\nText 1.\nText 2.\nText 3.\nText 4.\nText 5.\n",
            vec![
                sp(SpanKind::Chrome, "Ad one.", "Ad one."),
                sp(SpanKind::Chrome, "Ad one.", "Ad one."),
            ],
        ),
    ]
}

#[test]
fn fold_then_keep_all_restores_every_body() {
    for (body, spans) in round_trip_cases() {
        let folded = fold_only(body, spans);
        assert!(folded.contains("%% cv-fold lines="), "{folded}");
        assert!(
            fold_groups(&folded)
                .iter()
                .all(|g| g.anchored && g.kind.is_some())
        );
        let r = resolve_groups(&folded, &GroupTarget::All, Verdict::Keep).unwrap();
        assert_eq!(r.text, body, "folded:\n{folded}");
        assert!(
            r.groups
                .iter()
                .all(|g| g.placement == Some(Placement::Anchor))
        );
    }
}

#[test]
fn new_layout_groups_keep_blank_lines_kinds_and_distinct_ids() {
    let cases = round_trip_cases();
    // A blank line inside a group no longer splits it.
    let t = fold_only(cases[3].0, cases[3].1.clone());
    assert_eq!(removed_spans(&t), ["Promo start.\n\nPromo end."]);
    // Two adjacent kinds merge into one group, kind of its first line.
    let t = fold_only(cases[4].0, cases[4].1.clone());
    let g = fold_groups(&t);
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].kind, Some(SpanKind::Sponsor));
    assert_eq!(g[0].lines, ["Sponsor X.", "Click here."]);
    // Identical content: distinct identities.
    let t = fold_only(cases[5].0, cases[5].1.clone());
    let g = fold_groups(&t);
    assert_eq!(g.len(), 2);
    assert_eq!(g[1].id, format!("{}-2", g[0].id));
    // Metadata is hidden: no span carries it.
    assert!(removed_spans(&t).iter().all(|s| !s.contains("cv-fold")));
}

const OLD: &str = "Intro.\nMore text.\n\n%% cv-enrich %%\n> [!removed]-\n> one\n> two\n>\n> three\n\n%% /cv-enrich %%\n";

#[test]
fn old_layout_parses_reruns_and_keeps_at_end() {
    assert_eq!(removed_spans(OLD), ["one\ntwo", "three"]);
    let e = enrich(OLD, &EnrichOutput::default(), &[], opts());
    assert_eq!(e.text, OLD);
    let g = fold_groups(OLD);
    assert_eq!(g.len(), 2);
    assert!(g.iter().all(|g| !g.anchored && g.kind.is_none()));
    let r = resolve_groups(OLD, &GroupTarget::One(g[1].id.clone()), Verdict::Keep).unwrap();
    assert_eq!(r.groups[0].placement, Some(Placement::End));
    assert_eq!(
        r.text,
        "Intro.\nMore text.\n\nthree\n\n%% cv-enrich %%\n> [!removed]-\n> one\n> two\n\n%% /cv-enrich %%\n"
    );
    let all = resolve_groups(OLD, &GroupTarget::All, Verdict::Keep).unwrap();
    // Two End-placed groups: each its own block, document order.
    assert_eq!(all.text, "Intro.\nMore text.\n\none\ntwo\n\nthree\n");
    assert!(
        all.groups
            .iter()
            .all(|g| g.placement == Some(Placement::End))
    );
}

#[test]
fn mixed_layout_fold_lists_both_and_rerun_preserves_identities() {
    let base = OLD.replace("More text.\n", "More text.\nLine c.\nLine d.\nLine e.\n");
    let mixed = fold_only(&base, vec![sp(SpanKind::Cta, "More text.", "More text.")]);
    let g = fold_groups(&mixed);
    assert_eq!(g.len(), 3);
    assert_eq!(
        g.iter().map(|g| g.anchored).collect::<Vec<_>>(),
        [false, false, true]
    );
    assert_eq!(&removed_spans(&mixed)[..2], ["one\ntwo", "three"]);
    // A re-run with nothing new re-renders byte-identically; a re-run adding a group keeps the
    // prior groups' identities, kinds and anchors.
    assert_eq!(fold_only(&mixed, vec![]), mixed);
    let again = fold_only(&mixed, vec![sp(SpanKind::Footer, "Intro.", "Intro.")]);
    let g2 = fold_groups(&again);
    assert_eq!(g2.len(), 4);
    assert_eq!(&g2[..3], &g[..]);
    assert_eq!(g2[3].kind, Some(SpanKind::Footer));
}

#[test]
fn delete_one_of_several_groups() {
    let (body, _) = &round_trip_cases()[1];
    let t = fold_only(
        body,
        vec![
            sp(SpanKind::Chrome, "AD LINE", "AD LINE"),
            sp(SpanKind::Footer, "Body e.", "Body e."),
        ],
    );
    let g = fold_groups(&t);
    assert_eq!(g.len(), 2);
    let r = resolve_groups(&t, &GroupTarget::One(g[0].id.clone()), Verdict::Delete).unwrap();
    assert_eq!(r.groups.len(), 1);
    assert_eq!(r.groups[0].lines, ["AD LINE"]);
    assert_eq!(r.groups[0].placement, None);
    assert!(!r.text.contains("AD LINE"));
    assert_eq!(fold_groups(&r.text), g[1..]);
    // The remaining group still restores in place.
    let k = resolve_groups(&r.text, &GroupTarget::All, Verdict::Keep).unwrap();
    assert_eq!(k.text, "Intro a.\nIntro b.\n\nBody c.\nBody d.\nBody e.\n");
}

#[test]
fn delete_all_leaves_no_fold_block() {
    for (body, spans) in round_trip_cases() {
        let t = fold_only(body, spans);
        let r = resolve_groups(&t, &GroupTarget::All, Verdict::Delete).unwrap();
        for m in ["%% cv-enrich", "cv-fold", "[!removed]"] {
            assert!(!r.text.contains(m), "{m} in {:?}", r.text);
        }
        assert!(
            r.text.ends_with('\n') && !r.text.ends_with("\n\n"),
            "{:?}",
            r.text
        );
    }
    let r = resolve_groups(OLD, &GroupTarget::All, Verdict::Delete).unwrap();
    assert_eq!(r.text, "Intro.\nMore text.\n");
}

#[test]
fn stale_identity_is_refused() {
    let (body, spans) = round_trip_cases().remove(0);
    let t = fold_only(body, spans);
    assert_eq!(
        resolve_groups(&t, &GroupTarget::One("deadbeef".into()), Verdict::Keep),
        Err(FoldError::UnknownGroup("deadbeef".into()))
    );
    // No fold at all: All is a no-op.
    let r = resolve_groups(body, &GroupTarget::All, Verdict::Delete).unwrap();
    assert_eq!((r.text.as_str(), r.groups.len()), (body, 0));
}

#[test]
fn unresolved_anchor_falls_back_to_the_end() {
    let (body, spans) = round_trip_cases().remove(1);
    let t = fold_only(body, spans).replacen("Intro b.", "Rewritten b.", 1);
    let r = resolve_groups(&t, &GroupTarget::All, Verdict::Keep).unwrap();
    assert_eq!(r.groups[0].placement, Some(Placement::End));
    assert_eq!(
        r.text,
        "Intro a.\nRewritten b.\n\nBody c.\nBody d.\nBody e.\n\nAD LINE\n"
    );
}

#[test]
fn trim_removes_range_and_refuses_bad_ranges() {
    let (body, spans) = round_trip_cases().remove(1);
    let t = fold_only(body, spans);
    let r = trim_lines(&t, 0, 1).unwrap();
    assert_eq!(r.removed, ["Intro a.", "Intro b."]);
    assert!(r.text.starts_with("\nBody c."));
    assert_eq!(trim_lines(&t, 2, 1), Err(FoldError::EmptyRange));
    let n = t.split('\n').count();
    assert!(matches!(
        trim_lines(&t, 0, n),
        Err(FoldError::OutOfBounds { .. })
    ));
    let open = t.split('\n').position(|l| l == MARK_OPEN).unwrap();
    assert_eq!(trim_lines(&t, open - 1, open), Err(FoldError::Generated));
    let h = "a\n%% cv-enrich heading %%\n## H\n\nb\n";
    assert_eq!(trim_lines(h, 1, 1), Err(FoldError::Generated));
    assert_eq!(
        trim_lines(h, 2, 2).unwrap().text,
        "a\n%% cv-enrich heading %%\n\nb\n"
    );
}

/// Hand-edited, malformed and CRLF folds never panic; bad metadata reads as old layout.
#[test]
fn malformed_folds_are_total() {
    let (body, spans) = round_trip_cases().remove(3);
    let t = fold_only(body, spans);
    let bad = t.replace("lines=3", "lines=99");
    assert_eq!(removed_spans(&bad), ["Promo start.", "Promo end."]);
    assert!(fold_groups(&bad).iter().all(|g| !g.anchored));
    for s in [
        bad.as_str(),
        &t.replace('\n', "\r\n"),
        &t.replace("%% /cv-enrich %%", ""),
        "%% cv-enrich %%\n%% cv-fold %%\n> [!removed]-\n\n%% /cv-enrich %%",
        "%% cv-enrich %%\n%% cv-fold lines=1 kind=%G at=after occ=9 | x %%\n> [!removed]-\n> y\n%% /cv-enrich %%",
        "",
    ] {
        let _ = fold_groups(s);
        let _ = removed_spans(s);
        let _ = resolve_groups(s, &GroupTarget::All, Verdict::Keep);
        let _ = resolve_groups(s, &GroupTarget::All, Verdict::Delete);
        let _ = trim_lines(s, 0, 0);
    }
}

fn fr(first: usize, last: usize, kind: SpanKind) -> FoldRange {
    FoldRange { first, last, kind }
}

/// WP-5: `fold_ranges` writes byte-identical groups to `enrich`, appends to an existing fold, and
/// round-trips through `fold_groups` + `resolve_groups` (Keep restores at the anchor, Delete drops).
#[test]
fn fold_ranges_matches_enrich_and_round_trips() {
    // (body, ranges) mirroring round_trip_cases 0, 1, 2, 4.
    type Case = (&'static str, Vec<FoldRange>, Vec<Span>);
    let cases: Vec<Case> = vec![
        (
            round_trip_cases()[0].0,
            vec![fr(0, 0, SpanKind::Sponsor)],
            round_trip_cases()[0].1.clone(),
        ),
        (
            round_trip_cases()[1].0,
            vec![fr(3, 3, SpanKind::Chrome)],
            round_trip_cases()[1].1.clone(),
        ),
        (
            round_trip_cases()[2].0,
            vec![fr(5, 6, SpanKind::Cta)],
            round_trip_cases()[2].1.clone(),
        ),
        (
            round_trip_cases()[4].0,
            vec![fr(3, 4, SpanKind::Sponsor)],
            round_trip_cases()[4].1.clone(),
        ),
    ];
    for (body, ranges, spans) in cases {
        let ours = fold_ranges(body, &ranges).unwrap();
        assert_eq!(ours, fold_only(body, spans), "body:\n{body}");
        let g = fold_groups(&ours);
        assert_eq!(g.len(), ranges.len());
        assert!(g.iter().all(|g| g.anchored));
        let k = resolve_groups(&ours, &GroupTarget::All, Verdict::Keep).unwrap();
        assert_eq!(k.text, body);
        let d = resolve_groups(&ours, &GroupTarget::All, Verdict::Delete).unwrap();
        assert!(!d.text.contains("[!removed]"));
    }
    // An enriched note's fold gains a footer group; Keep of that group restores it in place.
    let body = "Sponsored by Acme.\n\nReal one.\nReal two.\nReal three.\nReal four.\n\nFooter a line\nFooter b line\n";
    let first = fold_only(body, round_trip_cases()[0].1.clone());
    let lines: Vec<&str> = first.split('\n').collect();
    let a = lines.iter().position(|l| *l == "Footer a line").unwrap();
    let both = fold_ranges(&first, &[fr(a, a + 1, SpanKind::Footer)]).unwrap();
    assert_eq!(both.matches("> [!removed]-").count(), 1, "{both:?}");
    let g = fold_groups(&both);
    assert_eq!(g.len(), 2);
    assert_eq!(g[1].kind, Some(SpanKind::Footer));
    let k = resolve_groups(&both, &GroupTarget::One(g[1].id.clone()), Verdict::Keep).unwrap();
    assert_eq!(k.text, first);
    assert_eq!(
        resolve_groups(&both, &GroupTarget::All, Verdict::Keep)
            .unwrap()
            .text,
        body
    );
    // A range touching the generated fold is refused.
    let f = both.split('\n').position(|l| l == "> [!removed]-").unwrap();
    assert_eq!(
        fold_ranges(&both, &[fr(f, f, SpanKind::Footer)]),
        Err(FoldError::Generated)
    );
}

/// A body exercising every generated-line class: a top block, an inserted heading (marker,
/// heading, blank), a lone marker, a fold block adjacent to a second fold, an unclosed opener.
const GEN_BODY: &str = "Intro\n%% cv-enrich %%\n> [!abstract]- Summary\n> s\n\n%% /cv-enrich %%\n\n%% cv-enrich heading %%\n### Chapter\n\nBody a\n%% cv-enrich heading %%\nBody b\n%% cv-enrich %%\n> [!removed]-\n> gone\n%% /cv-enrich %%\n%% cv-enrich %%\n> [!removed]-\n> also\n%% /cv-enrich %%\n%% cv-enrich %%\ntail";

#[test]
fn parity_trim_lines_refuses_marker_and_blocks_only() {
    let n = GEN_BODY.split('\n').count();
    let refused: Vec<usize> = (0..n)
        .filter(|&k| matches!(trim_lines(GEN_BODY, k, k), Err(FoldError::Generated)))
        .collect();
    assert_eq!(
        refused,
        [1, 2, 3, 4, 5, 7, 11, 13, 14, 15, 16, 17, 18, 19, 20]
    );
}

#[test]
fn parity_strip_structure_keeps_folds() {
    assert_eq!(
        strip_structure(GEN_BODY),
        "Intro\nBody a\nBody b\n%% cv-enrich %%\n> [!removed]-\n> gone\n%% /cv-enrich %%\n%% cv-enrich %%\n> [!removed]-\n> also\n%% /cv-enrich %%\n%% cv-enrich %%\ntail"
    );
}

#[test]
fn parity_parse_folds_boundaries() {
    let raw: Vec<&str> = GEN_BODY.split('\n').collect();
    let got: Vec<(usize, usize, usize)> = parse_folds(&raw)
        .iter()
        .map(|f| (f.open, f.close, f.groups.len()))
        .collect();
    assert_eq!(got, [(13, 16, 1), (17, 20, 1)]);
}

/// A fold whose groups carry metadata only in part (an old-layout group, then a `cv-fold` group)
/// keeps its stored lines byte-for-byte when `fold_ranges` or `enrich` appends a group.
#[test]
fn appending_to_a_partial_metadata_fold_keeps_its_lines() {
    let pre = format!("{MARK_OPEN}\n%% cv-fold lines=1 kind=cta at=start gap=0 join=0 %%");
    let content = format!("{FOLD_HEADER}\n> Old group.\n>\n> Meta group.");
    let body = format!("Intro.\n\nBody one.\n\nTail line.\n\n{pre}\n{content}\n\n{MARK_CLOSE}\n");
    let by_range = fold_ranges(&body, &[fr(4, 4, SpanKind::Footer)]).unwrap();
    let by_enrich = fold_only(
        &body,
        vec![sp(SpanKind::Footer, "Tail line.", "Tail line.")],
    );
    for out in [&by_range, &by_enrich] {
        assert!(
            out.contains(&format!("{pre}\n%% cv-fold lines=1 kind=footer")),
            "{out}"
        );
        assert!(
            out.contains(&format!("{content}\n>\n> Tail line.\n")),
            "{out}"
        );
        let kinds: Vec<_> = fold_groups(out).into_iter().map(|g| g.kind).collect();
        assert_eq!(kinds, [None, Some(SpanKind::Cta), Some(SpanKind::Footer)]);
    }
}

/// Adjacent ranges of different kinds stay two groups through `fold_ranges`.
#[test]
fn fold_ranges_keeps_adjacent_ranges_apart() {
    let body = "Intro.\n\nAd one.\nFollow us.\n\nEnd.\n";
    let out = fold_ranges(
        body,
        &[fr(2, 2, SpanKind::Sponsor), fr(3, 3, SpanKind::Cta)],
    )
    .unwrap();
    let g = fold_groups(&out);
    let kinds: Vec<_> = g.iter().map(|g| g.kind.clone()).collect();
    assert_eq!(kinds, [Some(SpanKind::Sponsor), Some(SpanKind::Cta)]);
    assert_eq!(
        resolve_groups(&out, &GroupTarget::All, Verdict::Keep)
            .unwrap()
            .text,
        body
    );
}
