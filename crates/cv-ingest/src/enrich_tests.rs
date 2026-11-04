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
