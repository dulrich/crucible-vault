//! `view.rs` unit tests (task marks, link/image click messages).

use super::*;

const LISTED: &str = " x><?/!\"-*liSIfkudwpcb";

#[test]
fn every_listed_char_has_a_private_use_glyph_except_empty() {
    for c in LISTED.chars() {
        let (g, _) = task_mark(c);
        if c == ' ' {
            assert_eq!(g, None);
        } else {
            let g = g.unwrap_or_else(|| panic!("no glyph for {c:?}"));
            assert!(('\u{e000}'..='\u{f8ff}').contains(&g.glyph()), "{c:?}");
        }
    }
}

#[test]
fn roles_follow_the_crucible_css() {
    assert_eq!(task_mark('x'), (Some(Icon::Check), TaskRole::Accent));
    assert_eq!(task_mark('!').1, TaskRole::Orange);
    assert_eq!(task_mark('w').1, TaskRole::Purple);
    assert_eq!(task_mark('i').1, TaskRole::Blue);
    assert_eq!(task_mark('-'), (Some(Icon::Minus), TaskRole::Faint));
    assert_eq!(task_mark('>').1, TaskRole::Faint);
}

/// Plan usability-sprint-captures-links WP-3: external links reach the opener.
#[test]
fn external_link_maps_to_open_url() {
    let f = link_msg("a.md");
    assert!(matches!(
        f(LinkTarget::External("https://x.y".into())),
        Message::OpenUrl(u) if u == "https://x.y"
    ));
    assert!(matches!(
        f(LinkTarget::Vault("b".into())),
        Message::Reading(ReadingMsg::OpenLink { .. })
    ));
}

/// Plan round-6 WP-3: a linked image clicks through its outer href.
#[test]
fn linked_image_click_opens_external_href() {
    assert!(matches!(
        linked_image_open("a.md", "https://x"),
        Message::OpenUrl(u) if u == "https://x"
    ));
}

/// Plan round-6 WP-3: a vault-path image inside a vault link opens the note link.
#[test]
fn vault_image_in_vault_link_opens_the_note() {
    let m = editor_model::render::lower("[![](img/a.png)](Other.md)\n");
    let [
        Block::Image {
            src,
            link: Some(href),
            remote: false,
            ..
        },
    ] = m.blocks.as_slice()
    else {
        panic!("expected one linked local image: {:?}", m.blocks);
    };
    assert_eq!((src.as_str(), href.as_str()), ("img/a.png", "Other.md"));
    let Message::Reading(ReadingMsg::OpenLink { src, target }) = linked_image_open("n.md", href)
    else {
        panic!("vault href must OpenLink");
    };
    assert_eq!((src.as_str(), target.as_str()), ("n.md", "Other.md"));
}

#[test]
fn unknown_char_falls_back_to_a_plain_box() {
    assert_eq!(task_mark('Z'), (None, TaskRole::Dim));
    assert_eq!(task_mark('9'), (None, TaskRole::Dim));
}

#[test]
fn only_cancelled_is_struck() {
    assert!(task_struck('-'));
    for c in LISTED.chars().filter(|c| *c != '-') {
        assert!(!task_struck(c), "{c:?}");
    }
}

#[test]
fn host_tasks_toggle_and_transcluded_do_not() {
    let Some(Message::Reading(ReadingMsg::ToggleTask { path, line })) =
        task_press("a.md", Some(' '), Some(3))
    else {
        panic!("host task must toggle");
    };
    assert_eq!((path.as_str(), line), ("a.md", 3));
    assert!(task_press("a.md", Some('x'), None).is_none());
    assert!(task_press("a.md", None, Some(3)).is_none());
}

/// Plan remote-media-sprint WP-5: the ▶ badge rule.
#[test]
fn video_hrefs_get_the_badge() {
    use embeds::is_video_href;
    assert!(is_video_href("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
    assert!(is_video_href("https://youtu.be/dQw4w9WgXcQ"));
    assert!(is_video_href("https://a.example/v.mp4"));
    assert!(!is_video_href("https://a.example/page"));
    assert!(!is_video_href("https://a.example/p.png"));
    assert!(!is_video_href("Other.md"));
}

/// Plan remote-media-sprint WP-5: card labels by kind; image / unknown keep the placeholder.
#[test]
fn remote_card_labels() {
    use embeds::remote_card_label as l;
    assert_eq!(
        l("https://www.youtube.com/watch?v=dQw4w9WgXcQ").as_deref(),
        Some("▶ video · youtube.com")
    );
    assert_eq!(
        l("https://cdn.example/v.mp4").as_deref(),
        Some("▶ video · cdn.example")
    );
    assert_eq!(l("https://x.com/u/status/1").as_deref(), Some("X post"));
    assert_eq!(
        l("https://www.a.example/s.mp3?x=1").as_deref(),
        Some("♪ audio · a.example")
    );
    assert_eq!(
        l("https://a.example/d.pdf").as_deref(),
        Some("pdf · a.example")
    );
    assert_eq!(l("https://a.example/p.png"), None);
    assert_eq!(l("https://a.example/page"), None);
}

#[test]
fn embed_card_label_uses_the_host() {
    use editor_model::render::RemoteCardKind;
    assert_eq!(
        embeds::remote_card_text(RemoteCardKind::Embed, "https://www.dw.example/c/1"),
        "embed · dw.example"
    );
}

#[test]
fn media_glyph_by_extension() {
    use embeds::media_glyph as g;
    assert_eq!(g("a/b.mp3"), Some("♪"));
    assert_eq!(g("b.FLAC"), Some("♪"));
    assert_eq!(g("b.mp4"), Some("▶"));
    assert_eq!(g("b.webm"), Some("▶"));
    assert_eq!(g("b.pdf"), None);
    assert_eq!(g("b"), None);
}

/// A markdown `![](local.mp4)` lowers to a local image block (the media card in the view).
#[test]
fn markdown_local_video_is_a_local_image_block() {
    let m = editor_model::render::lower("![](clips/a.mp4)\n");
    assert!(matches!(
        m.blocks.as_slice(),
        [Block::Image { remote: false, src, .. }] if src == "clips/a.mp4"
    ));
}
