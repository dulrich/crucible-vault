//! Synthetic `.eml` fixtures (invented senders and text; CRLF line endings).

use super::*;

fn crlf(s: &str) -> Vec<u8> {
    s.replace('\n', "\r\n").into_bytes()
}

const QP: &str = "From: \"Ann Writer\" <Ann@news.example>
To: reader@mail.example
Subject: Hello =?UTF-8?Q?caf=C3=A9?=
Date: Tue, 14 Oct 2026 10:00:00 +0000
Message-ID: <abc123@news.example>
List-Id: Ann's Letter <ann.news.example>
MIME-Version: 1.0
Content-Type: multipart/alternative; boundary=\"b1\"

--b1
Content-Type: text/plain; charset=utf-8
Content-Transfer-Encoding: quoted-printable

Plain caf=C3=A9 body
--b1
Content-Type: text/html; charset=utf-8
Content-Transfer-Encoding: quoted-printable

<p>Html caf=C3=A9 body</p><a href=3D\"https://ann.example/p/first-post?utm=3D1\">Read</a>
--b1--
";

#[test]
fn multipart_alternative_quoted_printable() {
    let m = parse_eml(&crlf(QP)).unwrap();
    assert_eq!(m.message_id, "abc123@news.example");
    assert_eq!(m.from_addr, "ann@news.example");
    assert_eq!(m.from_name, "Ann Writer");
    assert_eq!(m.list_id.as_deref(), Some("ann.news.example"));
    assert_eq!(m.subject, "Hello café");
    assert!(
        m.date
            .as_deref()
            .unwrap()
            .starts_with("2026-10-14T10:00:00")
    );
    assert!(m.html.as_deref().unwrap().contains("Html café body"));
    assert!(m.text.as_deref().unwrap().contains("Plain café body"));
    assert_eq!(m.urls, vec!["https://ann.example/p/first-post"]);
}

#[test]
fn mixed_related_base64_html() {
    // base64 of `<p>Sealed body text</p><a href="https://open.substack.com/pub/zed/p/slug-one">x</a>`
    let html =
        "<p>Sealed body text</p><a href=\"https://open.substack.com/pub/zed/p/slug-one\">x</a>";
    let b64 = base64_of(html.as_bytes());
    let eml = format!(
        "From: Zed <zed@pub.example>
Subject: Nested
Message-ID: <nest@pub.example>
MIME-Version: 1.0
Content-Type: multipart/mixed; boundary=\"outer\"

--outer
Content-Type: multipart/related; boundary=\"inner\"

--inner
Content-Type: text/html; charset=utf-8
Content-Transfer-Encoding: base64

{b64}
--inner--
--outer--
"
    );
    let m = parse_eml(&crlf(&eml)).unwrap();
    assert!(m.html.as_deref().unwrap().contains("Sealed body text"));
    assert_eq!(m.urls, vec!["https://zed.substack.com/p/slug-one"]);
}

fn base64_of(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(char::from(T[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[test]
fn iso_8859_1_body_is_decoded() {
    let mut eml = crlf(
        "From: Old <old@legacy.example>
Subject: Latin
Message-ID: <l1@legacy.example>
Content-Type: text/html; charset=ISO-8859-1
Content-Transfer-Encoding: 8bit

<p>Se",
    );
    eml.push(0xF1); // n-tilde in ISO-8859-1
    eml.extend_from_slice(b"or</p>\r\n");
    let m = parse_eml(&eml).unwrap();
    assert!(m.html.unwrap().contains("Señor"));
}

#[test]
fn missing_message_id_gets_a_stable_synthetic_id() {
    let eml = crlf("From: a@x.example\nSubject: S\n\n<p>body</p>\n");
    let a = parse_eml(&eml).unwrap();
    let b = parse_eml(&eml).unwrap();
    assert!(a.message_id.starts_with("sha:"));
    assert_eq!(a.message_id, b.message_id);
    let other = parse_eml(&crlf("From: a@x.example\nSubject: T\n\n<p>body</p>\n")).unwrap();
    assert_ne!(a.message_id, other.message_id);
}

#[test]
fn redirects_are_unwrapped_only_with_an_inline_target() {
    let html = r#"
<a href="https://track.esp.example/r?url=https%3A%2F%2Fann.example%2Fp%2Fwrapped&amp;sig=1">go</a>
<a href="https://track.esp.example/r/ZXhhbXBsZQ">opaque</a>
<a href="https://track.esp.example/r/opaque2">View in browser</a>
<a href="https://track.esp.example/c?u=not-a-url">read</a>"#;
    let urls = extract_post_urls(html);
    assert_eq!(urls, vec!["https://ann.example/p/wrapped"]);
}

#[test]
fn from_matcher_is_exact_or_domain_suffix() {
    let post = parse_eml(&crlf(QP)).unwrap();
    let yes = |from: &[&str], lid: Option<&str>| {
        let f: Vec<String> = from.iter().map(|s| (*s).to_owned()).collect();
        row_matches(&post, &f, lid)
    };
    assert!(yes(&["ANN@news.example"], None));
    assert!(yes(&["@news.example"], None));
    assert!(!yes(&["@example"], None));
    assert!(!yes(&["ann@other.example"], None));
    assert!(!yes(&["news.example"], None));
    assert!(yes(&[], Some("<ANN.news.example>")));
    assert!(!yes(&[], Some("other.example")));
    assert!(!yes(&[], None));
    let rows = [
        (vec!["x@y.example".to_owned()], None::<String>),
        (vec![], Some("ann.news.example".to_owned())),
    ];
    let hit = match_newsletter(&post, &rows, |r| (r.0.as_slice(), r.1.as_deref()));
    assert_eq!(hit, Some(&rows[1]));
}

fn bare(from: &str, list_id: Option<&str>) -> MailPost {
    MailPost {
        message_id: "m".into(),
        from_addr: from.into(),
        from_name: String::new(),
        list_id: list_id.map(str::to_owned),
        subject: String::new(),
        date: None,
        html: None,
        text: None,
        urls: vec![],
    }
}

#[test]
fn substack_host_from_list_id_or_sender() {
    let h = |f, l| substack_host(&bare(f, l));
    let want = Some("examplepub.substack.com".to_owned());
    assert_eq!(h("x@y.example", Some("examplepub.substack.com")), want);
    assert_eq!(h("examplepub@substack.com", None), want);
    assert_eq!(h("ExamplePub@Substack.com", None), want);
    // List-Id wins over the sender.
    assert_eq!(
        h("other@substack.com", Some("examplepub.substack.com")),
        want
    );
}

#[test]
fn substack_host_rejects_non_substack_and_malformed() {
    let h = |f, l| substack_host(&bare(f, l));
    assert_eq!(h("ann@news.example", Some("ann.news.example")), None);
    assert_eq!(h("ann@news.example", None), None);
    assert_eq!(h("x@y.example", Some("substack.com")), None);
    assert_eq!(h("x@y.example", Some(".substack.com")), None);
    assert_eq!(h("x@y.example", Some("a.b.substack.com")), None);
    assert_eq!(h("@substack.com", None), None);
    assert_eq!(h("a.b@substack.com", None), None);
}

#[test]
fn url_host_is_lowercased_and_bare() {
    assert_eq!(
        url_host("https://User@ExamplePub.Substack.com:8080/feed?x=1#f"),
        Some("examplepub.substack.com".to_owned())
    );
    assert_eq!(url_host("ftp://a.example/"), None);
    assert_eq!(url_host("https:///x"), None);
}

// --- plan intake-round-2-clean-dedupe-reading WP-2: canonical post URL ---

const JWT: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJyZWFkZXIifQ.c2lnbmF0dXJlX29mX2p3dA";

/// A Substack-sender post with `html` as its body (urls extracted like `parse_eml`).
fn sub_post(pubname: &str, subject: &str, html: &str) -> MailPost {
    MailPost {
        message_id: "m@substack.com".into(),
        from_addr: format!("{pubname}@substack.com"),
        from_name: "Pub".into(),
        list_id: Some(format!("{pubname}.substack.com")),
        subject: subject.into(),
        date: None,
        html: Some(html.into()),
        text: None,
        urls: extract_post_urls_for(html, subject),
    }
}

fn b64url(s: &str) -> String {
    base64_of(s.as_bytes())
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_")
}

fn variant_bodies() -> Vec<String> {
    let redirect = b64url(r#"{"e":"https://zed.substack.com/p/slug-one?r=abc&utm_source=x"}"#);
    vec![
        format!(
            "<a href=\"https://open.substack.com/pub/zed/p/slug-one?action=restack-comment&amp;r=abc&amp;token={JWT}\">Restack</a>"
        ),
        format!("<a href=\"https://substack.com/redirect/2/{redirect}.sig\">Read</a>"),
        "<a href=\"https://track.esp.example/r?url=https%3A%2F%2Fzed.substack.com%2Fp%2Fslug-one%3Fr%3Dabc\">Go</a>".into(),
        "<a href=\"https://ZED.substack.com/p/slug-one/?utm_source=email#top\">Read</a>".into(),
    ]
}

#[test]
fn every_link_variant_converges_on_one_canonical_url() {
    for body in variant_bodies() {
        let p = sub_post("zed", "S", &body);
        assert_eq!(
            canonical_post_url(&p, None).as_deref(),
            Some("https://zed.substack.com/p/slug-one"),
            "{body}"
        );
    }
}

#[test]
fn opaque_tracker_only_mail_has_no_identity() {
    let body = "<a href=\"https://link.mail.beehiiv.com/ss/c/u001.opaque/4f2/h1/abc\">View in browser</a>\
                <a href=\"https://zed.substack.com/redirect/2/bm90LWpzb24\">Read</a>\
                <a href=\"https://substack.com/redirect/2/bm90LWpzb24\">x</a>\
                <a href=\"https://esp.example/v2/c/abc\">y</a>";
    let p = sub_post("zed", "S", body);
    assert!(p.urls.is_empty(), "{:?}", p.urls);
    assert_eq!(canonical_post_url(&p, None), None);
}

#[test]
fn a_link_to_another_publication_is_never_the_identity() {
    let body = "<a href=\"https://other.substack.com/p/their-post\">A post I liked</a>\
                <a href=\"https://open.substack.com/pub/other/p/x?r=1\">restack</a>\
                <a href=\"https://blog.other.example/p/y\">y</a>";
    let p = sub_post("zed", "S", body);
    assert!(!p.urls.is_empty());
    assert_eq!(canonical_post_url(&p, None), None);
    assert_eq!(
        canonical_post_url(&p, Some("https://custom.example/feed")),
        None,
        "an unproven link is never rebased"
    );
}

#[test]
fn a_proven_candidate_rebases_onto_the_custom_domain_blog() {
    let body = format!(
        "<a href=\"https://open.substack.com/pub/zed/p/slug-one?action=restack-comment&amp;r=abc&amp;token={JWT}\">Restack</a>"
    );
    let p = sub_post("zed", "S", &body);
    assert_eq!(
        canonical_post_url(&p, Some("https://www.custom.example/feed")).as_deref(),
        Some("https://www.custom.example/p/slug-one")
    );
    // Evidence by the blog host alone (a custom-domain sender without a Substack host).
    let mut q = sub_post(
        "zed",
        "S",
        "<a href=\"https://custom.example/p/two?token=1\">x</a>",
    );
    q.from_addr = "news@custom.example".into();
    q.list_id = None;
    assert_eq!(
        canonical_post_url(&q, Some("https://custom.example/feed")).as_deref(),
        Some("https://custom.example/p/two")
    );
}

#[test]
fn the_messages_own_post_comes_first() {
    let body = "<a href=\"https://zed.substack.com/p/older\">Older post</a>\
                <a href=\"https://zed.substack.com/p/today\">Today Title</a>";
    let p = sub_post("zed", "Today Title", body);
    assert_eq!(p.urls[0], "https://zed.substack.com/p/today");
    assert_eq!(
        canonical_post_url(&p, None).as_deref(),
        Some("https://zed.substack.com/p/today")
    );
}

#[test]
fn odd_hrefs_never_panic_and_never_leak_tokens() {
    let body = "<a href=\"\">e</a><a href=\"mailto:x@y.example\">m</a><a href=\"/p/rel\">r</a>\
                <a href=\"https://zed.substack.com/p/%E0%A4\">bad pct</a><a href=\"https://%zz/p/q\">h</a>\
                <a href=\"https://track.esp.example/r?url=%\">w</a><a href=https://zed.substack.com/p/>no slug</a>";
    let p = sub_post("zed", "S", body);
    let _ = canonical_post_url(&p, Some("not a url"));
    let mut all: Vec<String> = variant_bodies()
        .iter()
        .flat_map(|b| {
            let p = sub_post("zed", "S", b);
            let mut v = p.urls.clone();
            v.extend(canonical_post_url(&p, Some("https://custom.example/")));
            v
        })
        .collect();
    all.extend(p.urls);
    for u in &all {
        assert!(
            !u.contains("token=") && !u.contains("r=") && !u.contains('?'),
            "{u}"
        );
    }
}

#[test]
fn the_identity_guard_rejects_tokens_jwts_and_trackers() {
    assert!(is_clean_identity("https://zed.substack.com/p/slug-one"));
    for bad in [
        "https://zed.substack.com/p/x?token=abc",
        "https://zed.substack.com/p/x?a=1&r=2",
        &format!("https://zed.substack.com/p/{JWT}"),
        "https://substack.com/redirect/2/abc",
        "https://link.mail.beehiiv.com/ss/c/abc",
        "https://esp.example/v2/c/abc",
        "https://esp.example/view/abc",
        "https://t.hubspotlinks.com/p/x",
        "mailto:x@y.example",
    ] {
        assert!(!is_clean_identity(bad), "{bad}");
    }
}
