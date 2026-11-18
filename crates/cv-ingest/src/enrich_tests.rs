//! Synthetic newsletter/blog/transcript fixtures (invented text only).
#![allow(clippy::field_reassign_with_default)]

use super::*;

const BODY: &str = "Welcome to this week's letter about widgets.

Widgets are getting   smaller every year, and the cost keeps falling.
Our analysis shows the median widget shrank by a third.

## Sponsor
This issue is brought to you by Acme Gizmos.
Use code WIDGET for ten percent off.

Manufacturers now ship widgets in bulk to regional hubs.
The hubs reduce shipping time considerably.
Retailers report steady demand through the season.
Some analysts expect a plateau next year.
Others argue demand will keep rising with new uses.
That wraps up the main story.

Subscribe now to get every issue in your inbox.
Share this letter with a friend.
";

const NO_HEADINGS: &str = "Host: Welcome back to the show, today we talk about gardens.
Guest: Thanks for having me, I love gardens.
Host: Let's start with soil.
Guest: Soil is everything, really.
Host: What about water?
Guest: Water less often but deeper.
Host: And pests?
Guest: Pests are a sign of imbalance.
";

fn opts() -> EnrichOpts {
    EnrichOpts { top_block: true }
}

fn full() -> EnrichOutput {
    parse(
        r###"Here you go:
```json
{
  "summary": "A letter on shrinking widgets and regional hubs.",
  "entities": [{"name": "Acme Gizmos", "type": "org", "aliases": [], "mentions": 1}],
  "key_claims": [{"claim": "Widgets shrank.", "quote": "the median widget shrank by a third"}],
  "chapters": [{"title": "Intro", "starts_at": "Welcome to this week"}],
  "spans": [{"kind": "sponsor", "first_line": "## Sponsor", "last_line": "Use code WIDGET for ten percent off.", "confidence": 0.99}],
  "content_ends_after": "That wraps up the main story.",
  "scores": {"density": 3}
}
```
Thanks!"###,
    )
    .unwrap()
}

fn note() -> String {
    BODY.to_owned()
}

#[test]
fn parse_strips_fence_and_prose_and_ignores_unknown_keys() {
    let o = full();
    assert_eq!(
        o.summary.as_deref(),
        Some("A letter on shrinking widgets and regional hubs.")
    );
    assert_eq!(o.entities[0].kind, "org");
    assert_eq!(o.spans[0].kind, SpanKind::Sponsor);
    assert_eq!(o.malformed, 0);
}

#[test]
fn parse_drops_wrong_typed_items_and_counts_them() {
    let o = parse(
        r#"{"summary": 7, "entities": [{"name": "A"}, 3, {"type": "x"}],
            "spans": [{"kind": "weird", "first_line": "a", "last_line": "b"}, {"kind": "cta"}],
            "chapters": "nope"}"#,
    )
    .unwrap();
    assert_eq!(o.summary, None);
    assert_eq!(o.entities.len(), 1);
    assert_eq!(o.spans.len(), 1);
    assert_eq!(o.spans[0].kind, SpanKind::Other("weird".into()));
    assert_eq!(o.malformed, 1 + 2 + 1 + 1);
}

#[test]
fn parse_errors_are_typed() {
    assert_eq!(parse("no json here"), Err(EnrichError::NoObject));
    assert!(matches!(
        parse("{not json}"),
        Err(EnrichError::InvalidJson(_))
    ));
    assert_eq!(parse("[1, {\"a\": 2}]"), Err(EnrichError::NotAnObject));
}

/// Plan round-23 WP-6: the reader mirrors the writer, one span per disjoint folded group.
#[test]
fn removed_spans_mirror_the_writer() {
    let e = enrich(&note(), &full(), &[], opts());
    let spans = removed_spans(&e.text);
    assert_eq!(spans.len(), e.report.spans_folded);
    assert!(spans[0].starts_with("## Sponsor\nThis issue"), "{spans:?}");
    assert!(spans[1].starts_with("Subscribe now"), "{spans:?}");
    assert!(spans[1].ends_with("Share this letter with a friend."));
    // The reader sees the same spans after a re-run (prior folds carried).
    let again = enrich(&e.text, &full(), &[], opts()).text;
    assert_eq!(removed_spans(&again), spans);
}

#[test]
fn removed_spans_hand_written_and_degenerate() {
    let fold = "text\n%% cv-enrich %%\n> [!removed]-\n> one\n> two\n>\n> three\n>\n>\n> four\n\n%% /cv-enrich %%\n";
    assert_eq!(removed_spans(fold), ["one\ntwo", "three", "four"]);
    // The top block (no `[!removed]-` header) and a loose callout are not folds.
    let top = "%% cv-enrich %%\n> [!abstract]-\n> S\n\n%% /cv-enrich %%\n> [!removed]-\n> loose\n";
    assert!(removed_spans(top).is_empty());
    // An unterminated block is ordinary text.
    assert!(removed_spans("%% cv-enrich %%\n> [!removed]-\n> x\n").is_empty());
    assert!(removed_spans("").is_empty());
}

#[test]
fn squeeze_collapses_whitespace() {
    assert_eq!(squeeze("  a \t b\n c  "), "a b c");
}

#[test]
fn full_enrich_renders_top_block_and_fold() {
    let e = enrich(&note(), &full(), &[], opts());
    let t = &e.text;
    assert!(t.starts_with("%% cv-enrich %%\n> [!abstract]-\n> A letter"));
    assert!(t.contains("> Entities: Acme Gizmos (org)"));
    assert!(t.contains("> - Widgets shrank. — \"the median widget shrank by a third\""));
    // Body has a heading (## Sponsor) but it is folded, so chapters apply.
    assert!(t.contains("%% cv-enrich heading %%\n## Intro\n\nWelcome"));
    assert!(t.contains("> - [[#Intro]]"));
    assert!(t.contains("> [!removed]-\n> ## Sponsor\n> This issue"));
    assert!(
        t.contains(">\n> Subscribe now to get every issue in your inbox.\n> Share this letter")
    );
    assert!(t.ends_with("%% /cv-enrich %%\n"));
    assert!(!t[..t.find("[!removed]").unwrap()].contains("Acme Gizmos.\n"));
    assert_eq!(e.report.spans_folded, 2);
    assert_eq!(e.report.lines_folded, 3 + 2);
    assert_eq!(e.report.headings_inserted, 1);
    assert_eq!(e.report.anchors_dropped, 0);
    assert_eq!(e.report.anchors_resolved, 4);
    assert!(e.report.top_block);
}

#[test]
fn idempotent_rerun_replaces_blocks() {
    let once = enrich(&note(), &full(), &[], opts()).text;
    let twice = enrich(&once, &full(), &[], opts()).text;
    assert_eq!(once, twice);
    assert_eq!(twice.matches("[!abstract]-").count(), 1);
    assert_eq!(twice.matches("[!removed]-").count(), 1);
    assert_eq!(twice.matches("## Intro").count(), 1);
}

#[test]
fn forced_rerun_keeps_prior_fold_and_adds_new() {
    let once = enrich(&note(), &full(), &[], opts()).text;
    let mut o2 = full();
    o2.spans = vec![Span {
        kind: SpanKind::Cta,
        first_line: "Some analysts expect a plateau next year.".into(),
        last_line: "Some analysts expect a plateau next year.".into(),
        confidence: 0.9,
    }];
    o2.content_ends_after = None;
    let e = enrich(&once, &o2, &[], opts());
    let fold = &e.text[e.text.find("[!removed]").unwrap()..];
    assert!(fold.contains("> ## Sponsor"));
    assert!(fold.contains("> Subscribe now"));
    assert!(fold.contains(">\n> Some analysts expect a plateau next year.\n\n%% /cv-enrich %%"));
    assert_eq!(e.text.matches("[!removed]-").count(), 1);
    assert_eq!(e.report.lines_folded, 1);
}

#[test]
fn inserted_headings_recognised_on_rerun() {
    let mut o = EnrichOutput::default();
    o.chapters = vec![
        Chapter {
            title: "Soil".into(),
            starts_at: "Host: Let's start".into(),
        },
        Chapter {
            title: "Pests".into(),
            starts_at: "And pests?".into(),
        },
    ];
    let once = enrich(NO_HEADINGS, &o, &[], opts());
    assert_eq!(once.report.headings_inserted, 2);
    assert!(
        once.text.contains("## Pests\n\nHost: And pests?"),
        "contains-fallback"
    );
    let twice = enrich(&once.text, &o, &[], opts());
    assert_eq!(twice.report.headings_inserted, 2);
    assert_eq!(once.text, twice.text);
    assert_eq!(twice.text.matches("## Soil").count(), 1);
}

#[test]
fn existing_headings_drive_toc_and_chapters_are_ignored() {
    let body = "# Title\n\nIntro text here.\n\n## Part [one]\n\nMore text.\n";
    let mut o = EnrichOutput::default();
    o.chapters = vec![Chapter {
        title: "X".into(),
        starts_at: "More text".into(),
    }];
    let e = enrich(body, &o, &[], opts());
    assert!(e.text.contains("> - [[#Title]]\n> - [[#Part one]]"));
    assert_eq!(e.report.headings_inserted, 0);
    assert!(!e.text.contains("## X"));
}

#[test]
fn headings_in_code_fences_do_not_count() {
    let body = "Intro line.\n```\n# not a heading\n```\nSecond part starts.\nMore.\n";
    let mut o = EnrichOutput::default();
    o.chapters = vec![Chapter {
        title: "Two".into(),
        starts_at: "Second part".into(),
    }];
    let e = enrich(body, &o, &[], opts());
    assert_eq!(e.report.headings_inserted, 1);
}

#[test]
fn unresolved_anchors_dropped_and_counted() {
    let mut o = full();
    o.spans.push(Span {
        kind: SpanKind::Chrome,
        first_line: "This line does not exist".into(),
        last_line: "Nor this one".into(),
        confidence: 1.0,
    });
    o.chapters.push(Chapter {
        title: "Ghost".into(),
        starts_at: "Nowhere to be found".into(),
    });
    o.key_claims.push(Claim {
        claim: "Fake.".into(),
        quote: "never said this".into(),
    });
    o.content_ends_after = Some("Missing ending".into());
    let e = enrich(&note(), &o, &[], opts());
    assert_eq!(e.report.anchors_dropped, 4);
    assert!(!e.text.contains("Ghost"));
    assert!(!e.text.contains("Fake."));
    assert!(
        e.text
            .contains("Subscribe now to get every issue in your inbox.\nShare")
    );
}

#[test]
fn last_line_must_follow_first_line() {
    let body = "End marker\nalpha\nbeta\ngamma\ndelta\nepsilon\nStart marker\nzeta\n";
    let mut o = EnrichOutput::default();
    o.spans = vec![Span {
        kind: SpanKind::Chrome,
        first_line: "Start marker".into(),
        last_line: "End marker".into(),
        confidence: 1.0,
    }];
    let e = enrich(body, &o, &[], opts());
    assert_eq!(e.report.anchors_dropped, 1);
    assert_eq!(e.text, body);
}

#[test]
fn overlapping_spans_merge() {
    let mut o = EnrichOutput::default();
    let sp = |a: &str, b: &str| Span {
        kind: SpanKind::Sponsor,
        first_line: a.into(),
        last_line: b.into(),
        confidence: 1.0,
    };
    o.spans = vec![
        sp("## Sponsor", "This issue is brought to you by Acme Gizmos."),
        sp(
            "This issue is brought to you by Acme Gizmos.",
            "Use code WIDGET for ten percent off.",
        ),
    ];
    let e = enrich(BODY, &o, &[], opts());
    assert_eq!(e.report.spans_folded, 1);
    assert_eq!(e.report.lines_folded, 3);
    assert_eq!(e.text.matches("> This issue").count(), 1);
}

#[test]
fn repeated_chrome_resolves_to_successive_occurrences() {
    let body = "Error banner.\nReload page.\nPart one text.\nPart one more.\nPart one end.\nError banner.\nReload page.\nPart two text.\nPart two more.\nPart two end.\n";
    let sp = Span {
        kind: SpanKind::Chrome,
        first_line: "Error banner.".into(),
        last_line: "Reload page.".into(),
        confidence: 1.0,
    };
    let mut o = EnrichOutput::default();
    o.spans = vec![sp.clone(), sp];
    let e = enrich(body, &o, &[], opts());
    assert_eq!(e.report.spans_folded, 2);
    assert!(!e.text[..e.text.find("[!removed]").unwrap()].contains("Error banner."));
}

#[test]
fn half_body_cap_refuses_fold_but_keeps_top_block() {
    let mut o = full();
    o.spans = vec![Span {
        kind: SpanKind::Chrome,
        first_line: "Welcome to this week's letter about widgets.".into(),
        last_line: "Others argue demand will keep rising with new uses.".into(),
        confidence: 1.0,
    }];
    let e = enrich(&note(), &o, &[], opts());
    assert!(e.report.fold_refused_over_half);
    assert_eq!(e.report.lines_folded, 0);
    assert!(!e.text.contains("[!removed]"));
    assert!(e.text.contains("[!abstract]"));
    assert!(e.text.contains("Subscribe now"));
}

#[test]
fn empty_output_leaves_body_unchanged() {
    let n = note();
    let e = enrich(&n, &EnrichOutput::default(), &[], opts());
    assert_eq!(e.text, n);
    assert_eq!(e.report, EnrichReport::default());
}

#[test]
fn span_matching_generated_block_text_is_ignored() {
    let once = enrich(&note(), &full(), &[], opts()).text;
    let mut o = full();
    o.spans = vec![Span {
        kind: SpanKind::Chrome,
        first_line: "> [!abstract]-".into(),
        last_line: "%% /cv-enrich %%".into(),
        confidence: 1.0,
    }];
    let e = enrich(&once, &o, &[], opts());
    assert_eq!(e.report.lines_folded, 0);
    assert!(e.report.anchors_dropped >= 1);
    assert_eq!(e.text.matches("[!abstract]-").count(), 1);
}

#[test]
fn banners_fold_into_same_callout() {
    let body = "![](https://cdn.example/banner.png)\n\nReal text one.\nReal text two.\nReal text three.\nReal text four.\n\n![[header-art.png]]\n";
    let banners = [
        BannerRef("![](https://cdn.example/banner.png)".into()),
        BannerRef("![[header-art.png]]".into()),
        BannerRef("![[absent.png]]".into()),
    ];
    let e = enrich(body, &EnrichOutput::default(), &banners, opts());
    assert_eq!(e.report.banners_folded, 2);
    assert_eq!(e.text.matches("[!removed]-").count(), 1);
    assert!(e.text.starts_with("Real text one."));
    assert!(
        e.text
            .contains("> ![](https://cdn.example/banner.png)\n>\n> ![[header-art.png]]")
    );
}

#[test]
fn fold_only_mode_skips_top_block_and_headings() {
    let e = enrich(&note(), &full(), &[], EnrichOpts { top_block: false });
    assert!(!e.text.contains("[!abstract]"));
    assert!(!e.text.contains("## Intro"));
    assert!(e.text.contains("[!removed]-"));
    assert!(!e.report.top_block);
}

/// Every non-blank original line survives in the body or as `> line` in the fold.
fn assert_no_loss(original: &str, out: &str) {
    for l in original.lines().filter(|l| !l.trim().is_empty()) {
        let folded = format!("> {l}");
        assert!(
            out.lines().any(|o| o == l || o == folded),
            "line lost: {l:?}\n--- out:\n{out}"
        );
    }
}

#[test]
fn unterminated_generated_block_keeps_every_line() {
    let once = enrich(&note(), &full(), &[], opts()).text;
    let broken = once.replacen("%% /cv-enrich %%\n", "", 1);
    let e = enrich(&broken, &full(), &[], opts());
    assert_no_loss(&broken, &e.text);
}

#[test]
fn heading_marker_without_heading_keeps_paragraph() {
    let body = "Intro line one.\n%% cv-enrich heading %%\nA real paragraph line.\nMore text.\n";
    let e = enrich(body, &EnrichOutput::default(), &[], opts());
    assert!(e.text.contains("A real paragraph line."));
    assert!(e.text.contains("More text."));
}

#[test]
fn no_line_lost_over_fixtures() {
    let n = note();
    for o in [full(), EnrichOutput::default()] {
        for top in [true, false] {
            let once = enrich(&n, &o, &[], EnrichOpts { top_block: top }).text;
            assert_no_loss(&n, &once);
            let twice = enrich(&once, &o, &[], EnrichOpts { top_block: top }).text;
            assert_no_loss(&n, &twice);
        }
    }
    let e = enrich(NO_HEADINGS, &full(), &[], opts());
    assert_no_loss(NO_HEADINGS, &e.text);
}

#[test]
fn rerun_with_empty_output_keeps_fold_and_drops_top() {
    let once = enrich(&note(), &full(), &[], opts()).text;
    let e = enrich(&once, &EnrichOutput::default(), &[], opts());
    assert!(!e.text.contains("[!abstract]"));
    assert!(e.text.contains("> ## Sponsor"));
    assert!(e.text.starts_with("Welcome"));
}

/// A close marker right after a `> ` line is a lazy continuation of the callout and renders as
/// text inside it; every close marker follows a blank line instead.
#[test]
fn close_marker_follows_blank_line() {
    let t = enrich(&note(), &full(), &[], opts()).text;
    let lines: Vec<&str> = t.lines().collect();
    let closes: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i] == MARK_CLOSE)
        .collect();
    assert_eq!(closes.len(), 2);
    for i in closes {
        assert_eq!(lines[i - 1], "", "line before close marker {i}");
    }
}

/// Notes enriched before the blank-line fix (close marker glued to the callout) re-run to the
/// fixed layout with the same fold content.
#[test]
fn legacy_glued_close_marker_reruns_to_fixed_layout() {
    let fixed = enrich(&note(), &full(), &[], opts()).text;
    let legacy = fixed.replace(&format!("\n\n{MARK_CLOSE}"), &format!("\n{MARK_CLOSE}"));
    assert_ne!(legacy, fixed);
    assert_eq!(enrich(&legacy, &full(), &[], opts()).text, fixed);
}

/// Plan tn-review-2026-11-04-remediation WP-R2: a ```` fence holding a ``` line stays open (its
/// `#` line is no heading), and a lowercase `## transcript` heading is an existing heading.
#[test]
fn nested_fence_and_lowercase_transcript_heading() {
    let body =
        "Intro line.\n````\n```\n# not a heading\n````\n## transcript\nSecond part starts.\n";
    let mut o = EnrichOutput::default();
    o.summary = Some("S.".into());
    let e = enrich(body, &o, &[], opts());
    assert!(e.text.contains("> - [[#transcript]]"), "{}", e.text);
    assert!(!e.text.contains("[[#not a heading]]"), "{}", e.text);
}

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
    enrich(body, &o, &[], EnrichOpts { top_block: false }).text
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

/// WP-5: `fold_ranges` writes byte-identical groups to `enrich`, appends to an existing fold, and
/// round-trips through `fold_groups` + `resolve_groups` (Keep restores at the anchor, Delete drops).
#[test]
fn fold_ranges_matches_enrich_and_round_trips() {
    // (body, ranges) mirroring round_trip_cases 0, 1, 2, 4.
    type Case = (&'static str, Vec<(usize, usize, SpanKind)>, Vec<Span>);
    let cases: Vec<Case> = vec![
        (
            round_trip_cases()[0].0,
            vec![(0, 0, SpanKind::Sponsor)],
            round_trip_cases()[0].1.clone(),
        ),
        (
            round_trip_cases()[1].0,
            vec![(3, 3, SpanKind::Chrome)],
            round_trip_cases()[1].1.clone(),
        ),
        (
            round_trip_cases()[2].0,
            vec![(5, 6, SpanKind::Cta)],
            round_trip_cases()[2].1.clone(),
        ),
        (
            round_trip_cases()[4].0,
            vec![(3, 4, SpanKind::Sponsor)],
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
    let both = fold_ranges(&first, &[(a, a + 1, SpanKind::Footer)]).unwrap();
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
        fold_ranges(&both, &[(f, f, SpanKind::Footer)]),
        Err(FoldError::Generated)
    );
}

/// `strip_structure` removes the top block and inserted headings, keeps the fold and the text,
/// and leaves a body without generated structure byte-identical.
#[test]
fn strip_structure_drops_the_top_block_and_inserted_headings_only() {
    let plain = "First paragraph.\n\n## Own heading\n\nSecond paragraph.\n";
    assert_eq!(strip_structure(plain), plain);
    let fold = "%% cv-enrich %%\n> [!removed]-\n> Buy now.\n%% /cv-enrich %%\n";
    let body = format!(
        "%% cv-enrich %%\n> [!abstract]-\n> Blurb.\n%% /cv-enrich %%\n\nIntro line.\n\n%% cv-enrich heading %%\n## Chapter one\n\nText one.\n\n%% cv-enrich heading %%\n## Chapter two\n\nText two.\n\n{fold}"
    );
    assert_eq!(
        strip_structure(&body),
        format!("Intro line.\n\nText one.\n\nText two.\n\n{fold}")
    );
}
