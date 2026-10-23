//! Synthetic fixtures (invented text; the link/chrome structure mirrors public newsletter mail).

use super::*;

const POST_PAYLOAD: &str = "eyJlIjoiaHR0cHM6XC9cL3dyaXRlci5leGFtcGxlXC9wXC9maXJzdC1wb3N0P3V0bV9zb3VyY2U9ZW1haWwmcj1hYmMma2VlcD0xIiwicCI6MX0";
const SUB_PAYLOAD: &str = "eyJlIjoiaHR0cHM6XC9cL3dyaXRlci5leGFtcGxlXC9zdWJzY3JpYmU_dXRtX2NhbXBhaWduPXgmbmV4dD0xIiwicyI6Mn0";
const ICON: &str = "https://substackcdn.com/image/fetch/$s_!aB!,w_36/https%3A%2F%2Fsubstack.com%2Ficon%2FLucideHeart%3Fv%3D4";
const CONTENT_IMG: &str =
    "https://substackcdn.com/image/fetch/$s_!cD!,w_1100/https%3A%2F%2Fbucket.example%2Fchart.png";

const PROSE_1: &str = "The first paragraph argues that *tariffs* are [a tax](https://econ.example/tax?utm_x=1) on importers.  ";
const PROSE_2: &str = "A second paragraph, with `code`, a footnote[^1] and an image:";

fn substack_fixture() -> String {
    format!(
        "Forwarded this email? [Subscribe here](https://substack.com/redirect/2/{SUB_PAYLOAD}.sig) for more\n\n\
         [View in browser](https://substack.com/redirect/2/{POST_PAYLOAD}.sig)\n\n\
         # [A Title](https://substack.com/redirect/2/{POST_PAYLOAD}.sig)\n\n\
         [![]({ICON})](https://substack.com/app-link/post?publication_id=1&post_id=2&utm_source=substack&token=abc)\n\n\
         [![]({ICON})Like](https://substack.com/app-link/post?publication_id=1&submitLike=true)\n\
         [![]({ICON})Comment](https://substack.com/app-link/post?comments=true)\n\
         [READ IN APP![]({ICON})](https://open.substack.com/pub/w/p/first?utm_source=email&r=abc&triedRedirect=true)\n\n\
         {PROSE_1}\n\n{PROSE_2}\n\n\
         [![]({CONTENT_IMG})](https://substack.com/redirect/2/{POST_PAYLOAD}.sig)\n\n\
         See [the thread](https://substack.com/app-link/post?post_id=2&redirect=https%3A%2F%2Fwriter.example%2Fp%2Fa%3Futm_medium%3Dx) and [the app](https://substack.com/app-link/post?publication_id=1&post_id=2).\n\n\
         [Upgrade to paid](https://substack.com/redirect/2/{SUB_PAYLOAD}.sig)\n\n\
         **[Subscribe now](https://writer.example/subscribe?utm_source=x)**\n\n\
         [![]({ICON})Share](https://substack.com/app-link/post?action=share)\n\n\
         Thanks for reading.\n\n\
         © 2026 A Writer  \n1 Example Street  \n[Unsubscribe](https://substack.com/redirect/2/{SUB_PAYLOAD}.sig)\n\n\
         [![Start your Substack]({CONTENT_IMG})](https://substack.com/signup)\n\n\
         ![](https://eotrx.substackcdn.com/open?token=a)![](https://email.mg1.substack.com/o/abc)\n"
    )
}

#[test]
fn substack_redirects_unwrap_and_chrome_goes() {
    let out = clean_body(&substack_fixture());
    for gone in [
        "substack.com/redirect",
        "app-link/post?publication_id",
        "utm_",
        "Forwarded",
        "View in browser",
        "READ IN APP",
        "Upgrade to paid",
        "Subscribe now",
        "Unsubscribe",
        "© 2026",
        "eotrx",
        "Start your Substack",
        "Share",
        "Like",
    ] {
        assert!(!out.contains(gone), "{gone:?} survived:\n{out}");
    }
    // The title link is unwrapped to its target, utm stripped (`r`/`keep` stay off Substack hosts).
    assert!(
        out.contains("# [A Title](https://writer.example/p/first-post?r=abc&keep=1)"),
        "{out}"
    );
    // A content image wrapped in a redirect keeps the image and the unwrapped link.
    assert!(
        out.contains(&format!(
            "[![]({CONTENT_IMG})](https://writer.example/p/first-post?r=abc&keep=1)"
        )),
        "{out}"
    );
    // `app-link` with a derivable post URL is canonicalized; without one it is the anchor text.
    assert!(
        out.contains("[the thread](https://writer.example/p/a)") && out.contains("and the app."),
        "{out}"
    );
    assert!(out.ends_with("Thanks for reading."), "{out}");
}

#[test]
fn prose_paragraphs_are_byte_identical() {
    let out = clean_body(&substack_fixture());
    // PROSE_1 differs only in its `utm_` link target; the rest is byte-identical.
    assert!(
        out.contains(
            "The first paragraph argues that *tariffs* are [a tax](https://econ.example/tax) on importers.  \n"
        ),
        "{out}"
    );
    assert!(out.contains(PROSE_2), "{out}");
    let plain = format!(
        "{PROSE_2}\n\nOne more paragraph, no links.\n\n```\nShare\n[Unsubscribe](https://x.example)\n```\n\nEnd."
    );
    assert_eq!(
        clean_body(&plain),
        plain,
        "code fences and prose are untouched"
    );
}

#[test]
fn undecodable_redirect_keeps_text_and_drops_the_tracker() {
    let md = "See [the post](https://substack.com/redirect/2/!!notbase64!!.sig) and [other](https://substack.com/redirect/55adf37a-d2b6?j=eyJ1IjoiNTJrdHJmIn0.sig) and ![](https://x.example/p.png).";
    assert_eq!(
        clean_body(md),
        "See the post and other and ![](https://x.example/p.png)."
    );
}

#[test]
fn tracking_params_strip_only_where_specified() {
    let md = "[a](https://news.example/p?r=1&s=2&utm_source=x&id=7#frag) [b](https://x.substack.com/p/q?r=1&s=2&token=t&publication_id=9&post_id=3&isFreemail=true&triedRedirect=true&utm_medium=m&keep=1)";
    assert_eq!(
        clean_body(md),
        "[a](https://news.example/p?r=1&s=2&id=7#frag) [b](https://x.substack.com/p/q?keep=1)"
    );
}

#[test]
fn beehiiv_unwraps_only_a_literal_target_and_footer_goes() {
    let md = "Intro paragraph here.\n\n[Read it](https://link.mail.beehiiv.com/ss/c/x?url=https%3A%2F%2Fwriter.example%2Fp%2Fa%3Futm_source%3Dbeehiiv) or [opaque](https://link.mail.beehiiv.com/ss/c/OPAQUETOKEN)\n\n[View online](https://link.mail.beehiiv.com/ss/c/v)\n\nBye.\n\nYou can [unsubscribe](https://link.mail.beehiiv.com/ss/u/abc) at any time.  \nA Letter Inc, 1 Road\n\n![](https://link.mail.beehiiv.com/o/abc)";
    assert_eq!(
        clean_body(md),
        "Intro paragraph here.\n\n[Read it](https://writer.example/p/a) or [opaque](https://link.mail.beehiiv.com/ss/c/OPAQUETOKEN)\n\nBye."
    );
}

#[test]
fn hubspot_redirects_and_footer() {
    let md = "Body paragraph.\n\n[Report](https://abc.hubspotlinks.com/Ctc/xyz?u=https%3A%2F%2Fco.example%2Fr%3Futm_campaign%3Dq) and [tracked](https://abc.hubspotlinks.com/Ctc/opaque)\n\nYou can [unsubscribe from this newsletter](https://hs.s.hubspotemail.net/hs/preferences-center/en/page?data=W2n) or manage preferences.\n\n![](https://hs.s.hubspotemail.net/e2t/o/abc)";
    assert_eq!(
        clean_body(md),
        "Body paragraph.\n\n[Report](https://co.example/r) and [tracked](https://abc.hubspotlinks.com/Ctc/opaque)"
    );
}

#[test]
fn a_long_tail_after_an_unsubscribe_mention_is_not_a_footer() {
    let tail: String = (0..20)
        .map(|i| format!("\n\nTail paragraph {i}."))
        .collect();
    let md = format!(
        "Intro.\n\nTo stop these mails [unsubscribe here](https://x.example/u) any time.{tail}"
    );
    let out = clean_body(&md);
    assert!(
        out.contains("Tail paragraph 19.") && out.contains("[unsubscribe here]"),
        "{out}"
    );
}

#[test]
fn clean_body_is_idempotent() {
    for md in [
        substack_fixture(),
        "[x](https://substack.com/redirect/2/!!bad) ![](https://eotrx.substackcdn.com/a)\n\n\n\n\n[Share](https://s.example)\n\nEnd  ".to_owned(),
        format!("a\n\n[![]({ICON})Like](https://substack.com/app-link/post)\n\n\n\nb"),
        String::new(),
    ] {
        let once = clean_body(&md);
        assert_eq!(clean_body(&once), once, "not idempotent for {md:?}");
    }
}

#[test]
fn b64url_and_json_helpers() {
    assert_eq!(b64url_decode("aGk_").as_deref(), Some(&b"hi?"[..]));
    assert_eq!(b64url_decode("aGk=").as_deref(), Some(&b"hi"[..]));
    assert_eq!(b64url_decode("a*b"), None);
    assert_eq!(
        json_e(r#"{"e":"https:\/\/a.example\/p?x=1&y=2","p":1}"#).as_deref(),
        Some("https://a.example/p?x=1&y=2")
    );
    assert_eq!(json_e(r#"{"p":"x"}"#), None);
}

// ---- Round 2 (plan intake-round-2-clean-dedupe-reading WP-1) ----

const AVATAR: &str = "https://substackcdn.com/image/fetch/$s_!eF!,w_40,c_scale,f_png,q_auto:good/https%3A%2F%2Fcdn.example%2Favatar.png";
const BIG_URL: &str = "https://substackcdn.com/image/fetch/$s_!gH!,w_1100,c_limit,f_auto/https%3A%2F%2Fbucket.example%2Fchart-1100x733.png";

fn html_fixture() -> String {
    format!(
        "<html><head><title>A Title</title><style>.x{{color:red;margin:0}}@media (max-width:600px){{.x{{width:1px}}}}</style></head>\
         <body><script>var a = 1;</script><div style=\"display:none\">&#847;&zwnj;&nbsp;&#847;&zwnj;&nbsp;&#847;&zwnj;&nbsp;</div>\
         <p>First paragraph with <b>bold</b> text.</p>\
         <a href=\"https://substack.com/@writer\"><img src=\"{AVATAR}\" width=\"32\" height=\"32\"></a>\
         <a href=\"https://writer.example/p/first\"><img src=\"https://t.example/px.gif\" width=\"1\" height=\"1\"></a>\
         <img src=\"https://cdn.example/logo.png\" width=\"183\" height=\"40\">\
         <img src=\"https://cdn.example/dot.png\" style=\"width: 18px;height: 18px\">\
         <img src=\"https://cdn.example/chart.png\" width=\"1100\" height=\"733\" alt=\"chart\">\
         <p>Upgrade to paid to keep reading.</p></body></html>"
    )
}

#[test]
fn html_pass_removes_style_head_script_spacers_and_small_images() {
    let out = strip_html_chrome(&html_fixture());
    for gone in [
        "<style",
        "<head",
        "<script",
        "color:red",
        "A Title",
        "zwnj",
        "avatar",
        "px.gif",
        "dot.png",
    ] {
        assert!(!out.contains(gone), "{gone} survived: {out}");
    }
    for kept in [
        "logo.png",
        "chart.png",
        "First paragraph with <b>bold</b> text.",
    ] {
        assert!(out.contains(kept), "{kept} lost: {out}");
    }
}

#[test]
fn html_pass_keeps_paywall_marker_and_is_idempotent() {
    let out = strip_html_chrome(&html_fixture());
    assert!(out.contains("Upgrade to paid to keep reading."));
    assert_eq!(strip_html_chrome(&out), out);
    assert!(!strip_html_chrome("<header>Top</header><p>x</p>").is_empty());
    assert_eq!(
        strip_html_chrome("<header>Top</header>"),
        "<header>Top</header>"
    );
}

#[test]
fn html_pass_drops_anchor_wrapping_only_a_pixel_but_keeps_text_anchors() {
    let h = "<a href=\"https://w.example/\"><img src=\"https://t.example/o/abc\"></a>\
             <a href=\"https://w.example/p\"><img src=\"https://cdn.example/i.png\" width=\"20\"> Read the post</a>\
             <a href=\"https://w.example/q\"><img src=\"https://cdn.example/big.png\" width=\"600\"></a>";
    let out = strip_html_chrome(h);
    assert!(!out.contains("t.example"));
    assert!(out.contains("<a href=\"https://w.example/p\"> Read the post</a>"));
    assert!(out.contains("big.png"));
    assert_eq!(out.matches("<a ").count(), 2);
}

#[test]
fn spacer_lines_are_dropped_at_the_markdown_stage() {
    let md =
        "Real intro.\n\n\u{34f} \u{ad}\u{34f} \u{ad}\u{200c}\n\n\u{a0}\u{a0}\n\nSecond paragraph.";
    assert_eq!(clean_body(md), "Real intro.\n\nSecond paragraph.");
}

#[test]
fn referral_block_goes_with_its_paragraph_and_buttons() {
    let md = format!(
        "Prose before.\n\n#### Invite your friends and earn rewards\n\n\
         If you enjoy Some Letter, share it with your friends and earn rewards when they subscribe.\n\n\
         [Invite Friends](https://writer.example/leaderboard?utm_source=x)\n\
         [![]({ICON})Subscribed](https://substack.com/redirect/2/{SUB_PAYLOAD}.sig)\n\n\
         Prose after, about inviting friends to dinner."
    );
    assert_eq!(
        clean_body(&md),
        "Prose before.\n\nProse after, about inviting friends to dinner."
    );
}

#[test]
fn css_shaped_lines_are_dropped_but_code_prose_is_not() {
    let css = "@media only screen and (max-width:639px){img.stretch{height:auto !important;width:100%}.cta{color:inherit;display:block}.hdr{margin:0;padding:0}.ftr{font-size:12px;color:#888}.btn{border:0;background:#fff;padding:4px 8px}}";
    let prose = "In Rust a struct is `struct A { x: u32 }` and the closure `|a| { a + 1 }` returns a value; see the book.";
    let md = format!("{css}\n\n{prose}");
    assert_eq!(clean_body(&md), prose);
}

#[test]
fn icon_shapes_are_widened() {
    for src in [
        "https://substackcdn.com/image/fetch/w_36,c_scale,f_png/https%3A%2F%2Fsubstack.com%2Ficon%2FLucideShare2%3Fv%3D4",
        "https://substackcdn.com/image/fetch/$s_!x!,w_40,c_scale,f_png/https%3A%2F%2Fcdn.example%2Fcheck.png",
        "https://hs.example/hubfs/Social%20Icons/light-facebook-icon.png",
        "https://cdn.example/icon/arrow.svg",
    ] {
        assert!(is_icon(src), "{src}");
        assert_eq!(
            clean_body(&format!("Text.\n\n![x]({src}) tail text")),
            "Text.\n\n tail text"
        );
    }
    assert!(!is_icon(CONTENT_IMG));
    assert!(!is_icon(BIG_URL));
}

#[test]
fn avatar_in_profile_link_and_social_icon_runs_are_dropped_content_survives() {
    let md = format!(
        "[![]({AVATAR})](https://substack.com/@writer)\n\n\
         [![By Writer]({AVATAR}) Writer](https://substack.com/@writer)\n\n\
         [![](https://cdn.example/a.png)](https://substack.com/@writer)\n\n\
         [![chart]({BIG_URL})](https://writer.example/p/first)\n\n\
         [![]({ICON})](https://x.example/a) [![]({ICON})](https://x.example/b)"
    );
    let out = clean_body(&md);
    assert_eq!(
        out,
        format!(
            "[ Writer](https://substack.com/@writer)\n\n[![chart]({BIG_URL})](https://writer.example/p/first)"
        )
    );
}

#[test]
fn dims_hook_drops_small_local_images_and_keeps_large() {
    let md = "Intro.\n\n![](_attachments/a1.png)\n\n[![](_attachments/b2.png)](https://w.example/p)\n\n![chart](_attachments/c3.png)\n\n![](_attachments/d4.png)\n\nOutro.";
    let dims = |u: &str| match u {
        "_attachments/a1.png" => Some((36, 36)),
        "_attachments/b2.png" => Some((1, 1)),
        "_attachments/c3.png" => Some((1100, 611)),
        _ => None,
    };
    let out = clean_body_with(md, &dims);
    assert_eq!(
        out,
        "Intro.\n\n![chart](_attachments/c3.png)\n\n![](_attachments/d4.png)\n\nOutro."
    );
    assert_eq!(clean_body_with(&out, &dims), out);
    assert_eq!(clean_body(md), clean_body_with(md, &|_| None));
}

#[test]
fn chrome_link_wrapping_localized_icon_is_removed_in_one_pass() {
    let md = "Intro.\n\n[![](attachments/b3f9.png) Read in app](https://x.example/app)\n\nOutro.";
    let dims = |u: &str| (u == "attachments/b3f9.png").then_some((24, 24));
    let once = clean_body_with(md, &dims);
    assert_eq!(once, "Intro.\n\nOutro.");
    assert_eq!(clean_body_with(&once, &dims), once);
    // A large localized image in a chrome link still reads as content.
    let big = |_: &str| Some((600, 400));
    assert!(clean_body_with(md, &big).contains("Read in app"));
    // Idempotence on an existing representative fixture.
    let fx = substack_fixture();
    let c = clean_body_with(&fx, &dims);
    assert_eq!(clean_body_with(&c, &dims), c);
}

#[test]
fn full_round_two_pipeline_is_clean_prose_identical_and_idempotent() {
    let md = crate_pipeline(&html_fixture());
    assert!(!md.contains("color:red") && !md.contains("avatar") && !md.contains("px.gif"));
    assert!(md.contains("First paragraph with **bold** text."));
    assert!(md.contains("chart.png"));
    assert!(md.contains("Upgrade to paid to keep reading."));
    assert_eq!(clean_body(&md), md);
}

fn crate_pipeline(html: &str) -> String {
    clean_body(&crate::html_to_markdown(&strip_html_chrome(html)))
}

#[test]
fn resolve_link_is_the_wp2_seam() {
    assert_eq!(
        resolve_link(&format!(
            "https://substack.com/redirect/2/{POST_PAYLOAD}.sig"
        ))
        .as_deref(),
        Some("https://writer.example/p/first-post?r=abc&keep=1")
    );
    assert_eq!(
        resolve_link("https://substack.com/redirect/2/!!bad.sig"),
        None
    );
    assert_eq!(
        resolve_link("https://writer.example/p/a?utm_source=x&k=1").as_deref(),
        Some("https://writer.example/p/a?k=1")
    );
}

#[test]
fn small_hints_and_dims_thresholds() {
    assert_eq!(
        resize_hints("https://h/image/fetch/$s_!a!,w_36,h_36,c_fill/https%3A%2F%2Fo%2Fw_999.png"),
        vec![36, 36]
    );
    assert!(is_icon(
        "https://h/image/fetch/w_64,c_fill/https%3A%2F%2Fo%2Fa.png"
    ));
    assert!(!is_icon(
        "https://h/image/fetch/w_65,c_fill/https%3A%2F%2Fo%2Fa.png"
    ));
    assert!(!is_icon(
        "https://h/image/fetch/w_1100,h_618,c_fill/https%3A%2F%2Fo%2Fa.png"
    ));
}

#[test]
fn prose_quoting_inline_braces_code_survives() {
    let prose = "The config example { a: 1; b: 2; } is shown inline in this long sentence that keeps going well past the old forty character floor, so it would once have matched, and it carries on with more ordinary words until it also clears the two hundred byte minimum.";
    assert_eq!(clean_body(prose), prose);
}

#[test]
fn semicolon_free_minified_css_is_dropped() {
    let css = "@media screen and (max-width:600px){.x .y a{font-size: 9px}.w{margin: 0 auto}.v .u{border: 1px solid red}}@media (min-width:700px){.x{padding: 9px 9 9 9}.z .q{color: #333}}@media print{.k{display: none}.j{width: 100%}.i{height: 50px}}";
    assert!(css.len() >= 200 && !css.contains(';'));
    let x = format!("First paragraph.\n\n{css}\n\nSecond paragraph.");
    let out = clean_body(&x);
    assert_eq!(out, "First paragraph.\n\nSecond paragraph.");
    assert_eq!(clean_body(&out), out);
}

#[test]
fn prose_with_one_brace_pair_and_colon_is_not_css() {
    let prose = "The snippet { total: 5 } appears inline in this long sentence of ordinary prose, and the note: it keeps going well past the minimum length so that only the structural rules can decide, with more plain words to clear two hundred bytes easily.";
    assert!(prose.len() >= 200);
    assert_eq!(clean_body(prose), prose);
}
