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
