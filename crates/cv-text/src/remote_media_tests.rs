use super::*;
use crate::attachments::is_embed_page;

fn yt(id: &str) -> RemoteKind {
    RemoteKind::VideoPage {
        provider: VideoProvider::YouTube,
        id: id.into(),
    }
}

#[test]
fn classify_youtube_shapes() {
    let id = "dQw4w9WgXcQ";
    for u in [
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10",
        "https://youtube.com/shorts/dQw4w9WgXcQ",
        "https://www.youtube.com/embed/dQw4w9WgXcQ?si=x",
        "https://youtube.com/live/dQw4w9WgXcQ",
        "https://youtu.be/dQw4w9WgXcQ?t=3",
        "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ",
        "https://m.youtube.com/watch?v=dQw4w9WgXcQ",
        "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
    ] {
        assert_eq!(classify_remote(u), yt(id), "{u}");
    }
    assert_eq!(
        classify_remote("https://youtube.com/watch?v=short"),
        RemoteKind::Unknown
    );
    assert_eq!(
        classify_remote("https://youtube.com/@channel"),
        RemoteKind::Unknown
    );
}

#[test]
fn classify_vimeo_shapes() {
    let v = |id: &str| RemoteKind::VideoPage {
        provider: VideoProvider::Vimeo,
        id: id.into(),
    };
    assert_eq!(classify_remote("https://vimeo.com/123456"), v("123456"));
    assert_eq!(classify_remote("https://www.vimeo.com/42?x=1"), v("42"));
    assert_eq!(
        classify_remote("https://player.vimeo.com/video/987"),
        v("987")
    );
    assert_eq!(
        classify_remote("https://vimeo.com/channels/staff"),
        RemoteKind::Unknown
    );
}

#[test]
fn classify_posts() {
    for u in [
        "https://x.com/someone/status/123",
        "https://twitter.com/someone/status/123",
        "https://mobile.twitter.com/someone/status/123",
        "https://x.com/i/web/status/123",
        "https://x.com/i/article/55",
    ] {
        assert_eq!(classify_remote(u), RemoteKind::Post, "{u}");
    }
    assert_eq!(
        classify_remote("https://x.com/someone"),
        RemoteKind::Unknown
    );
}

#[test]
fn classify_by_extension() {
    let table = [
        ("https://a.test/p/x.PNG?w=1", RemoteKind::Image),
        ("https://a.test/x.jpeg", RemoteKind::Image),
        ("https://a.test/x.svg#f", RemoteKind::Image),
        ("https://a.test/x.mp4", RemoteKind::VideoFile),
        ("https://a.test/x.webm", RemoteKind::VideoFile),
        ("https://a.test/live/x.m3u8", RemoteKind::VideoFile),
        ("https://a.test/x.mov", RemoteKind::VideoFile),
        ("https://a.test/x.mp3", RemoteKind::AudioFile),
        ("https://a.test/x.opus", RemoteKind::AudioFile),
        ("https://a.test/x.aac", RemoteKind::AudioFile),
        ("https://a.test/x.flac", RemoteKind::AudioFile),
        ("https://a.test/doc.pdf", RemoteKind::Pdf),
        ("https://a.test/page.html", RemoteKind::Unknown),
        ("https://a.test/image", RemoteKind::Unknown),
        ("https://a.test/", RemoteKind::Unknown),
        ("not a url", RemoteKind::Unknown),
        // A YouTube host path that is not a video page falls through to the extension rule.
        ("https://youtube.com/img/x.png", RemoteKind::Image),
    ];
    for (u, k) in table {
        assert_eq!(classify_remote(u), k, "{u}");
    }
}

#[test]
fn effective_kind_rules() {
    use EffectiveKind::{IdlessVideoPage, Kind};
    let page = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
    let post = "https://x.com/a/status/1";
    // A video page or post wins over every tag.
    for tag in [
        None,
        Some(HtmlMediaTag::Img),
        Some(HtmlMediaTag::Video),
        Some(HtmlMediaTag::Audio),
        Some(HtmlMediaTag::Iframe),
    ] {
        assert_eq!(
            effective_kind(page, tag),
            Kind(yt("dQw4w9WgXcQ")),
            "{tag:?}"
        );
        assert_eq!(effective_kind(post, tag), Kind(RemoteKind::Post), "{tag:?}");
    }
    // Otherwise the tag decides.
    let u = "https://a.test/x";
    assert_eq!(
        effective_kind(u, Some(HtmlMediaTag::Iframe)),
        Kind(RemoteKind::Embed)
    );
    assert_eq!(
        effective_kind(u, Some(HtmlMediaTag::Video)),
        Kind(RemoteKind::VideoFile)
    );
    assert_eq!(
        effective_kind(u, Some(HtmlMediaTag::Audio)),
        Kind(RemoteKind::AudioFile)
    );
    assert_eq!(
        effective_kind("https://a.test/x.mp4", Some(HtmlMediaTag::Img)),
        Kind(RemoteKind::Image)
    );
    // No tag: the classifier's kind, except an id-less embed page.
    assert_eq!(
        effective_kind("https://a.test/x.pdf", None),
        Kind(RemoteKind::Pdf)
    );
    assert_eq!(effective_kind(u, None), Kind(RemoteKind::Unknown));
    assert_eq!(
        effective_kind("https://youtu.be/abc", None),
        IdlessVideoPage
    );
    // The id-less page is only reached without a tag.
    assert_eq!(
        effective_kind("https://youtu.be/abc", Some(HtmlMediaTag::Img)),
        Kind(RemoteKind::Image)
    );
}

#[test]
fn is_embed_page_truth_table_unchanged() {
    for u in [
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        "https://youtube.com/watch",
        "https://youtube.com/shorts/x",
        "https://youtu.be/abc",
        "https://www.youtube-nocookie.com/embed/x",
        "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
        "https://x.com/a/status/1",
        "https://mobile.twitter.com/a/status/1",
    ] {
        assert!(is_embed_page(u), "{u}");
    }
    for u in [
        "https://vimeo.com/123",
        "https://player.vimeo.com/video/1",
        "https://x.com/i/article/55",
        "https://x.com/a",
        "https://youtu.be/",
        "https://youtube.com/@c",
        "https://a.test/x.png",
        "garbage",
    ] {
        assert!(!is_embed_page(u), "{u}");
    }
}

#[test]
fn html_img_void_and_attrs() {
    let s = r#"pre <IMG alt="a > b" SRC='https://a.test/x.png' width=10> post"#;
    let v = scan_html_media(s);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].tag, HtmlMediaTag::Img);
    assert_eq!(v[0].src, "https://a.test/x.png");
    assert_eq!(&s[v[0].span.clone()], &s[4..s.len() - 5]);
}

#[test]
fn html_img_unquoted_and_self_closing() {
    let s = "<img src=https://a.test/x.png/>";
    let v = scan_html_media("<img src=https://a.test/y.png />");
    assert_eq!(v[0].src, "https://a.test/y.png");
    let v = scan_html_media(s);
    assert_eq!(v.len(), 1);
}

#[test]
fn html_local_and_srcless_skipped() {
    assert!(scan_html_media(r#"<img src="local.png"><img alt="x"><video></video>"#).is_empty());
}

#[test]
fn html_video_with_source_and_poster() {
    let s = "<video controls poster=\"https://a.test/p.jpg\">\n<source src=\"local.mp4\">\n<source src=\"https://a.test/v.mp4\" type=video/mp4>\n</VIDEO >tail";
    let v = scan_html_media(s);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].tag, HtmlMediaTag::Video);
    assert_eq!(v[0].src, "https://a.test/v.mp4");
    assert_eq!(v[0].poster.as_deref(), Some("https://a.test/p.jpg"));
    assert_eq!(v[0].span, 0..s.len() - 4);
}

#[test]
fn html_audio_src_attr_and_self_closing() {
    let v = scan_html_media(
        r#"<audio src="https://a.test/a.mp3" /> <audio src="https://a.test/b.ogg"></audio>"#,
    );
    assert_eq!(v.len(), 2);
    assert!(v.iter().all(|m| m.tag == HtmlMediaTag::Audio));
    assert_eq!(v[1].src, "https://a.test/b.ogg");
}

#[test]
fn html_iframe_multiline_attrs() {
    let s = "x\n<iframe\n  width=\"560\" height=\"315\"\n  SRC=\"https://www.youtube.com/embed/dQw4w9WgXcQ\"\n  allowfullscreen\n  title='t'></iframe>\ny";
    let v = scan_html_media(s);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].tag, HtmlMediaTag::Iframe);
    assert_eq!(classify_remote(&v[0].src), yt("dQw4w9WgXcQ"));
    assert!(s[v[0].span.clone()].ends_with("</iframe>"));
}

#[test]
fn html_fenced_and_unclosed_skipped() {
    let s = "```\n<img src=\"https://a.test/f.png\">\n```\n<video src=\"https://a.test/v.mp4\">\nno close\n<img src=\"https://a.test/ok.png\">";
    let v = scan_html_media(s);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].src, "https://a.test/ok.png");
}

#[test]
fn html_nested_same_name_skipped() {
    let s = r#"<video src="https://a.test/1.mp4"><video src="https://a.test/2.mp4"></video></video><img src="https://a.test/i.png">"#;
    let v = scan_html_media(s);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].tag, HtmlMediaTag::Img);
}

#[test]
fn html_unterminated_quote_skipped() {
    assert!(scan_html_media("<img src=\"https://a.test/x.png>").is_empty());
}
