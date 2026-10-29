//! Ported vectors of Crucible `tests/localizeAttachments.edge.test.mjs` (names kept).
use super::*;

fn r(from: &str, to: &str) -> Replacement {
    Replacement {
        from: from.into(),
        to: to.into(),
    }
}
fn resolve(link: &str, exp: &str, paths: &[&str]) -> Result<String, RepairFailure> {
    resolve_local_attachment_repair(link, exp, &AttachmentPathIndex::build(paths))
}
fn plan(link: &str, exp: &str, paths: &[&str]) -> Option<String> {
    resolve(link, exp, paths).ok()
}
fn m(syntax: Syntax, is_embed: bool, display: Option<&str>) -> AttachmentMatch {
    AttachmentMatch {
        original: String::new(),
        link: String::new(),
        syntax,
        is_remote: false,
        is_embed,
        display_text: display.map(str::to_owned),
    }
}

#[test]
fn rewrite_only_replaces_full_markdown_image_ranges() {
    let content = "Numeric alt stays intact: ![2024-01-15](photos/2024-01-15.png)\nNumeric filename localizes: ![1](photos/1.png)\nPlain numeric filename text stays intact: photos/1.png";
    let res = rewrite_localized_attachment_refs(
        content,
        &[
            r(
                "![1](photos/1.png)",
                "![](Notes/_attachments/post/abc123_MD5.png)",
            ),
            r("photos/1.png", "BROKEN"),
        ],
    );
    assert_eq!(
        res.content,
        "Numeric alt stays intact: ![2024-01-15](photos/2024-01-15.png)\nNumeric filename localizes: ![](Notes/_attachments/post/abc123_MD5.png)\nPlain numeric filename text stays intact: photos/1.png"
    );
    assert_eq!(res.applied_from, ["![1](photos/1.png)"]);
}

#[test]
fn rewrite_keeps_numeric_heavy_sibling_filenames_separate() {
    let content = "![shot 1](assets/2024-01-15-1.png)\n![shot 10](assets/2024-01-15-10.png)";
    let res = rewrite_localized_attachment_refs(
        content,
        &[r(
            "![shot 1](assets/2024-01-15-1.png)",
            "![](assets/hash1_MD5.png)",
        )],
    );
    assert_eq!(
        res.content,
        "![](assets/hash1_MD5.png)\n![shot 10](assets/2024-01-15-10.png)"
    );
}

#[test]
fn strip_removes_only_image_data_placeholders() {
    let (c, n) = strip_data_uri_image_placeholders(
        "A ![](data:image/gif;base64,R0lGODlhAQABAAAAACw=) ![](real.png)",
    );
    assert_eq!(n, 1);
    assert_eq!(c, "A ![](real.png)");
}

#[test]
fn clamp_image_quality_handles_nan_undefined_and_bounds() {
    assert!((clamp_image_quality(Some(f64::NAN)) - 0.85).abs() < 1e-9);
    assert!((clamp_image_quality(None) - 0.85).abs() < 1e-9);
    assert!((clamp_image_quality(Some(10.0)) - 0.3).abs() < 1e-9);
    assert!((clamp_image_quality(Some(120.0)) - 1.0).abs() < 1e-9);
    assert!((clamp_image_quality(Some(72.0)) - 0.72).abs() < 1e-9);
}

#[test]
fn repoint_rewrites_moved_embed_prefixes_md_wiki_encoded_raw() {
    let content = "![](_resources/Clippings/example-person/3ff_MD5.webp)\n![[_resources/Clippings/example-person/abc_MD5.png]]\nProse mentioning _resources/Clippings/example-person/ stays untouched.";
    assert_eq!(
        repoint_attachment_folder_prefix(
            content,
            "_resources/Clippings/example-person",
            "_resources/daily/day/2026-06-13/example-person"
        ),
        "![](_resources/daily/day/2026-06-13/example-person/3ff_MD5.webp)\n![[_resources/daily/day/2026-06-13/example-person/abc_MD5.png]]\nProse mentioning _resources/Clippings/example-person/ stays untouched."
    );
}

#[test]
fn repoint_handles_pct20_prefixes_and_is_idempotent() {
    let (o, n) = ("_resources/My Clips/post", "_resources/daily/post");
    let md = "![](_resources/My%20Clips/post/x_MD5.webp)";
    let once = repoint_attachment_folder_prefix(md, o, n);
    assert_eq!(once, "![](_resources/daily/post/x_MD5.webp)");
    assert_eq!(repoint_attachment_folder_prefix(&once, o, n), once);
    assert_eq!(repoint_attachment_folder_prefix(md, o, o), md);
}

#[test]
fn plan_prefers_expected_folder_then_unique_name() {
    let exp = "_resources/daily/day/2026-06-13/example-person";
    let paths = [
        "_resources/daily/day/2026-06-13/example-person/3ff_MD5.webp",
        "_resources/other/elsewhere/3ff_MD5.webp",
    ];
    assert_eq!(
        plan("_resources/Clippings/example-person/3ff_MD5.webp", exp, &paths).as_deref(),
        Some("_resources/daily/day/2026-06-13/example-person/3ff_MD5.webp")
    );
    assert_eq!(
        plan(
            "_resources/Clippings/x/uniq_MD5.png",
            exp,
            &["_resources/wherever/uniq_MD5.png"]
        )
        .as_deref(),
        Some("_resources/wherever/uniq_MD5.png")
    );
    assert_eq!(
        plan(
            "_resources/Clippings/x/dup.png",
            exp,
            &["a/dup.png", "b/dup.png"]
        ),
        None
    );
    assert_eq!(
        plan("_resources/Clippings/x/gone_MD5.png", exp, &paths),
        None
    );
    assert_eq!(
        plan(
            "_resources/Clippings/x/a%20b_MD5.png",
            exp,
            &["_resources/keep/a b_MD5.png"]
        )
        .as_deref(),
        Some("_resources/keep/a b_MD5.png")
    );
}

fn links(pairs: &[(&str, &[(&str, u32)])]) -> HashMap<String, HashMap<String, u32>> {
    pairs
        .iter()
        .map(|(s, t)| {
            (
                (*s).to_owned(),
                t.iter().map(|(p, n)| ((*p).to_owned(), *n)).collect(),
            )
        })
        .collect()
}

#[test]
fn has_other_referrer_detects_a_second_note() {
    let l = links(&[
        (
            "daily/day/2026-05-23/Sync.md",
            &[("_resources/_blog_metadata/k/300b_MD5.png", 1)],
        ),
        (
            "_blog_metadata/k/2022-06-05-sync.md",
            &[("_resources/_blog_metadata/k/300b_MD5.png", 1)],
        ),
    ]);
    assert!(has_other_attachment_referrer(
        &l,
        "_resources/_blog_metadata/k/300b_MD5.png",
        "daily/day/2026-05-23/Sync.md"
    ));
}

#[test]
fn has_other_referrer_false_when_only_excluded_note() {
    let l = links(&[
        (
            "daily/day/2026-05-23/Sync.md",
            &[("_resources/old/300b_MD5.png", 2)],
        ),
        ("unrelated/note.md", &[("_resources/other/aaa_MD5.png", 1)]),
    ]);
    assert!(!has_other_attachment_referrer(
        &l,
        "_resources/old/300b_MD5.png",
        "daily/day/2026-05-23/Sync.md"
    ));
}

#[test]
fn has_other_referrer_ignores_zero_counts_and_empty_maps() {
    let l = links(&[("a.md", &[("_resources/x_MD5.png", 0)]), ("b.md", &[])]);
    assert!(!has_other_attachment_referrer(
        &l,
        "_resources/x_MD5.png",
        "c.md"
    ));
    assert!(!has_other_attachment_referrer(
        &HashMap::new(),
        "_resources/x_MD5.png",
        "c.md"
    ));
}

#[test]
fn prefix_repair_min_stem_length_is_8() {
    assert_eq!(PREFIX_REPAIR_MIN_STEM_LENGTH, 8);
}

const EXP: &str = "_resources/notes/post";
const TRUNC: &str = "_resources/Clippings/x/abcdef12_MD5.pn";

#[test]
fn unique_prefix_hit_recovers_truncated_basename() {
    let paths = ["_resources/elsewhere/abcdef1234567890_MD5.png"];
    assert_eq!(resolve(TRUNC, EXP, &paths).as_deref(), Ok(paths[0]));
    assert_eq!(plan(TRUNC, EXP, &paths).as_deref(), Some(paths[0]));
}

#[test]
fn prefix_ambiguity_reports_ambiguous() {
    let paths = [
        "_resources/elsewhere/abcdef1234567890_MD5.png",
        "_resources/other/abcdef12ffffffffff_MD5.png",
    ];
    assert_eq!(resolve(TRUNC, EXP, &paths), Err(RepairFailure::Ambiguous));
}

#[test]
fn below_min_stem_no_prefix_recovery() {
    let paths = ["_resources/elsewhere/abcdef1234567890_MD5.png"];
    assert_eq!(
        resolve("_resources/Clippings/x/abc_MD5.pn", EXP, &paths),
        Err(RepairFailure::Missing)
    );
}

#[test]
fn exact_basename_wins_over_prefix() {
    let paths = [
        "_resources/exact/abcdef12_MD5.png",
        "_resources/elsewhere/abcdef1234567890_MD5.png",
    ];
    assert_eq!(
        resolve("_resources/Clippings/x/abcdef12_MD5.png", EXP, &paths).as_deref(),
        Ok(paths[0])
    );
}

#[test]
fn prefix_ignores_non_managed_candidate() {
    let paths = ["_resources/elsewhere/abcdef1234567890.png"];
    assert_eq!(resolve(TRUNC, EXP, &paths), Err(RepairFailure::Missing));
}

#[test]
fn tier2_md5_ambiguity_prefers_expected_folder() {
    let paths = [
        "_resources/notes/post/sub/dup_MD5.png",
        "_resources/other/dup_MD5.png",
        "_resources/third/dup_MD5.png",
    ];
    assert_eq!(
        resolve("_resources/Clippings/x/dup_MD5.png", EXP, &paths).as_deref(),
        Ok(paths[0])
    );
}

#[test]
fn tier2_md5_ambiguity_shortest_path() {
    let paths = [
        "_resources/a/much/deeper/nested/folder/dup_MD5.png",
        "_resources/b/dup_MD5.png",
    ];
    assert_eq!(
        resolve("_resources/Clippings/x/dup_MD5.png", EXP, &paths).as_deref(),
        Ok("_resources/b/dup_MD5.png")
    );
}

#[test]
fn tier2_md5_ambiguity_equal_length_lexicographic() {
    let paths = ["_resources/bbb/dup_MD5.png", "_resources/aaa/dup_MD5.png"];
    assert_eq!(
        resolve("_resources/Clippings/x/dup_MD5.png", EXP, &paths).as_deref(),
        Ok("_resources/aaa/dup_MD5.png")
    );
}

#[test]
fn tier2_non_md5_duplicates_bail_ambiguous() {
    assert_eq!(
        resolve(
            "_resources/Clippings/x/dup.png",
            EXP,
            &["a/dup.png", "b/dup.png"]
        ),
        Err(RepairFailure::Ambiguous)
    );
}

#[test]
fn tier3_identical_full_basenames_auto_resolve() {
    let paths = [
        "_resources/notes/post/abcdef1234567890_MD5.png",
        "_resources/elsewhere/abcdef1234567890_MD5.png",
    ];
    assert_eq!(resolve(TRUNC, EXP, &paths).as_deref(), Ok(paths[0]));
}

#[test]
fn tier3_differing_basenames_bail_ambiguous() {
    let paths = [
        "_resources/elsewhere/abcdef1234567890_MD5.png",
        "_resources/other/abcdef12ffffffffff_MD5.png",
    ];
    assert_eq!(resolve(TRUNC, EXP, &paths), Err(RepairFailure::Ambiguous));
}

#[test]
fn plain_missing_ref_is_missing() {
    assert_eq!(
        resolve("_resources/x/nothing_MD5.png", EXP, &["a/b.png"]),
        Err(RepairFailure::Missing)
    );
}

#[test]
fn format_embed_empty_alt() {
    assert_eq!(
        format_embed(Syntax::Wiki, "folder/x_MD5.png"),
        "![[folder/x_MD5.png]]"
    );
    assert_eq!(
        format_embed(Syntax::Md, "folder/x_MD5.png"),
        "![](folder/x_MD5.png)"
    );
    assert_eq!(
        format_embed(Syntax::Md, "a folder/x_MD5.png"),
        "![](a%20folder/x_MD5.png)"
    );
}

#[test]
fn format_link_preserves_display_text() {
    assert_eq!(
        format_link(Syntax::Wiki, "folder/x_MD5.pdf", None),
        "[[folder/x_MD5.pdf]]"
    );
    assert_eq!(
        format_link(Syntax::Wiki, "folder/x_MD5.pdf", Some("the source PDF")),
        "[[folder/x_MD5.pdf|the source PDF]]"
    );
    assert_eq!(
        format_link(Syntax::Md, "folder/x_MD5.pdf", Some("the source PDF")),
        "[the source PDF](folder/x_MD5.pdf)"
    );
    assert_eq!(
        format_link(Syntax::Md, "a folder/x_MD5.pdf", Some("doc")),
        "[doc](a%20folder/x_MD5.pdf)"
    );
}

#[test]
fn format_ref_dispatches_on_is_embed() {
    assert_eq!(
        format_ref(&m(Syntax::Wiki, true, None), "x_MD5.png"),
        "![[x_MD5.png]]"
    );
    assert_eq!(
        format_ref(&m(Syntax::Md, false, Some("the report")), "x_MD5.pdf"),
        "[the report](x_MD5.pdf)"
    );
}

#[test]
fn parse_collects_embeds_tagged_is_embed() {
    let ms = parse_attachment_refs("![[a_MD5.png]]");
    assert_eq!(ms.len(), 1);
    assert!(ms[0].is_embed);
    assert_eq!(ms[0].syntax, Syntax::Wiki);
}

#[test]
fn parse_collects_links_only_when_managed() {
    let ms = parse_attachment_refs("[[attachments/a_MD5.pdf|the report]] [[notes/other-note.md]]");
    assert_eq!(ms.len(), 1);
    assert!(!ms[0].is_embed);
    assert_eq!(ms[0].syntax, Syntax::Wiki);
    assert_eq!(ms[0].display_text.as_deref(), Some("the report"));
    assert_eq!(ms[0].link, "attachments/a_MD5.pdf");
}

#[test]
fn parse_markdown_managed_link_keeps_display_and_syntax() {
    let ms = parse_attachment_refs("[the report](attachments/a_MD5.pdf)");
    assert_eq!(ms.len(), 1);
    assert!(!ms[0].is_embed);
    assert_eq!(ms[0].syntax, Syntax::Md);
    assert_eq!(ms[0].display_text.as_deref(), Some("the report"));
}

#[test]
fn parse_dedupes_identical_original() {
    assert_eq!(
        parse_attachment_refs("![[a_MD5.png]] and ![[a_MD5.png]]").len(),
        1
    );
}

#[test]
fn parse_finds_remote_markdown_images() {
    let ms = parse_attachment_refs("![alt](https://example.com/pic.png)");
    assert_eq!(ms.len(), 1);
    assert!(ms[0].is_remote && ms[0].is_embed);
}

#[test]
fn parse_skips_fenced_code_and_data_uris() {
    let ms = parse_attachment_refs("```\n![[a_MD5.png]]\n```\n![](data:image/gif;base64,AA)");
    assert!(ms.is_empty());
}

#[test]
fn rewrite_wiki_link_to_managed_attachment_lands() {
    let res = rewrite_localized_attachment_refs(
        "See [[attachments/old/a_MD5.pdf|the report]] for details.",
        &[r(
            "[[attachments/old/a_MD5.pdf|the report]]",
            "[[attachments/new/a_MD5.pdf|the report]]",
        )],
    );
    assert_eq!(
        res.content,
        "See [[attachments/new/a_MD5.pdf|the report]] for details."
    );
    assert_eq!(
        res.applied_from,
        ["[[attachments/old/a_MD5.pdf|the report]]"]
    );
}

#[test]
fn rewrite_pct20_markdown_link_lands() {
    let res = rewrite_localized_attachment_refs(
        "The [source PDF](attachments/old%20folder/a_MD5.pdf) is here.",
        &[r(
            "[source PDF](attachments/old%20folder/a_MD5.pdf)",
            "[source PDF](attachments/new%20folder/a_MD5.pdf)",
        )],
    );
    assert_eq!(
        res.content,
        "The [source PDF](attachments/new%20folder/a_MD5.pdf) is here."
    );
    assert_eq!(
        res.applied_from,
        ["[source PDF](attachments/old%20folder/a_MD5.pdf)"]
    );
}

#[test]
fn rewrite_mixed_note_moves_only_managed_refs() {
    let content = "![[attachments/old/img_MD5.png]]\n[[attachments/old/doc_MD5.pdf|the doc]]\nSee also [[Some Other Note]] and [more](Other%20Note.md).";
    let res = rewrite_localized_attachment_refs(
        content,
        &[
            r(
                "![[attachments/old/img_MD5.png]]",
                "![[attachments/new/img_MD5.png]]",
            ),
            r(
                "[[attachments/old/doc_MD5.pdf|the doc]]",
                "[[attachments/new/doc_MD5.pdf|the doc]]",
            ),
        ],
    );
    assert_eq!(
        res.content,
        "![[attachments/new/img_MD5.png]]\n[[attachments/new/doc_MD5.pdf|the doc]]\nSee also [[Some Other Note]] and [more](Other%20Note.md)."
    );
    assert_eq!(res.applied_from.len(), 2);
}

#[test]
fn rewrite_never_touches_ordinary_note_links() {
    let content = "Check [[Some Other Note]] and [the text](Other%20Note.md) for more.";
    let res = rewrite_localized_attachment_refs(
        content,
        &[
            r("[[Some Other Note]]", "[[Renamed Note]]"),
            r("[the text](Other%20Note.md)", "[the text](Renamed.md)"),
        ],
    );
    assert_eq!(res.content, content);
    assert!(res.applied_from.is_empty());
}

#[test]
fn rewrite_is_idempotent() {
    let content = "See [[attachments/new/a_MD5.pdf|the report]] for details.";
    let res = rewrite_localized_attachment_refs(
        content,
        &[r(
            "[[attachments/old/a_MD5.pdf|the report]]",
            "[[attachments/new/a_MD5.pdf|the report]]",
        )],
    );
    assert_eq!(res.content, content);
    assert!(res.applied_from.is_empty());
}

#[test]
fn repoint_wiki_link_alongside_embeds() {
    let content = "![](_resources/Clippings/example-person/3ff_MD5.webp)\n[[_resources/Clippings/example-person/doc_MD5.pdf|Open: the source PDF]]";
    assert_eq!(
        repoint_attachment_folder_prefix(
            content,
            "_resources/Clippings/example-person",
            "_resources/daily/day/2026-06-13/example-person"
        ),
        "![](_resources/daily/day/2026-06-13/example-person/3ff_MD5.webp)\n[[_resources/daily/day/2026-06-13/example-person/doc_MD5.pdf|Open: the source PDF]]"
    );
}

#[test]
fn repoint_pct20_markdown_link_idempotent() {
    let (o, n) = ("_resources/My Clips/post", "_resources/daily/post");
    let md = "[the source](attachments/_resources/My%20Clips/post/x_MD5.pdf)";
    let once = repoint_attachment_folder_prefix(md, o, n);
    assert_eq!(
        once,
        "[the source](attachments/_resources/daily/post/x_MD5.pdf)"
    );
    assert_eq!(repoint_attachment_folder_prefix(&once, o, n), once);
}

#[test]
fn repoint_never_rewrites_ordinary_note_links() {
    let content = "[[_resources/Clippings/example-person/Some Note]]\n[a note](_resources/Clippings/example-person/Some%20Note.md)";
    assert_eq!(
        repoint_attachment_folder_prefix(
            content,
            "_resources/Clippings/example-person",
            "_resources/daily/day/2026-06-13/example-person"
        ),
        content
    );
}

#[test]
fn fragment_suffixed_managed_ref_resolves() {
    assert_eq!(
        resolve(
            "_resources/old/uniq_MD5.png#page=2",
            "attachments/x",
            &["_resources/keep/uniq_MD5.png"]
        )
        .as_deref(),
        Ok("_resources/keep/uniq_MD5.png")
    );
}

#[test]
fn alias_suffixed_managed_ref_resolves() {
    assert_eq!(
        resolve(
            "_resources/old/uniq_MD5.png|Open: pasted image",
            "attachments/x",
            &["_resources/keep/uniq_MD5.png"]
        )
        .as_deref(),
        Ok("_resources/keep/uniq_MD5.png")
    );
}

#[test]
fn fragment_suffixed_tier2_md5_duplicates_auto_resolve() {
    assert_eq!(
        resolve(
            "_resources/old/dup_MD5.png#frag",
            "attachments/x",
            &["b/dup_MD5.png", "a/dup_MD5.png"]
        )
        .as_deref(),
        Ok("a/dup_MD5.png")
    );
}

#[test]
fn rewrite_replaces_image_nested_in_markdown_link() {
    let img = "![](https://example.com/uploads/logo-1200x628-1.jpg)";
    let content = format!("Before\n[{img}](https://link.example.com/click/abc)\nAfter");
    let res =
        rewrite_localized_attachment_refs(&content, &[r(img, "![](_resources/n/dd8e_MD5.webp)")]);
    assert_eq!(res.applied_from, [img]);
    assert_eq!(
        res.content,
        "Before\n[![](_resources/n/dd8e_MD5.webp)](https://link.example.com/click/abc)\nAfter"
    );
}

#[test]
fn repoint_nested_embed_leaves_outer_href() {
    assert_eq!(
        repoint_attachment_folder_prefix(
            "[![](_resources/Clippings/note/dd8e_MD5.webp)](https://link.example.com/click/abc)",
            "_resources/Clippings/note",
            "_resources/daily/day/2026-10-13/note"
        ),
        "[![](_resources/daily/day/2026-10-13/note/dd8e_MD5.webp)](https://link.example.com/click/abc)"
    );
}

#[test]
fn templates_and_slug() {
    // The one cv-text `slugify` — ASCII is unchanged, non-ASCII letters now fold
    // (measured: no existing `_attachments/<slug>` folder changes name).
    assert_eq!(crate::slugify("My Post: 2024!"), "my-post-2024");
    assert_eq!(
        crate::slugify("My Post: Ünïcode  2024!"),
        "my-post-unicode-2024"
    );
    assert_eq!(
        normalize_path("/_attachments//my-note/"),
        "_attachments/my-note"
    );
    let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 19)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .unwrap();
    assert_eq!(
        resolve_attachment_folder(DEFAULT_FOLDER_TEMPLATE, "My Note.md", "pic", "png", now),
        "_attachments/my-note"
    );
    assert_eq!(
        resolve_attachment_path(
            DEFAULT_FOLDER_TEMPLATE,
            DEFAULT_NAME_TEMPLATE,
            "notes/My Note.md",
            "",
            "png",
            "abc",
            now
        ),
        "notes/_attachments/my-note/abc_MD5.png"
    );
    assert!(is_md5_name("abc_MD5.png") && !is_md5_name("abc_MD5.") && !is_md5_name("abc.png"));
}

#[test]
fn titled_remote_images_are_refs() {
    let src = "[![alt](https://cdn.example/a.png \"a title\")](https://cdn.example/a)\n![](https://cdn.example/b.png 't')\n![](https://cdn.example/c.png)\n";
    let refs = parse_attachment_refs(src);
    let links: Vec<&str> = refs.iter().map(|m| m.link.as_str()).collect();
    assert_eq!(
        links,
        [
            "https://cdn.example/a.png",
            "https://cdn.example/b.png",
            "https://cdn.example/c.png"
        ]
    );
    assert_eq!(
        refs[0].original,
        "![alt](https://cdn.example/a.png \"a title\")"
    );
    assert!(has_remote_image_refs(
        "![x](https://cdn.example/a.png \"t\")"
    ));
}

#[test]
fn escaped_paren_image_url_is_unescaped_and_fully_matched() {
    let src = "x ![](https://h.example/a\\(b\\).png) y\n![](https://h.example/plain.png)\n";
    let refs = parse_attachment_refs(src);
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].link, "https://h.example/a%28b%29.png");
    assert_eq!(refs[0].original, "![](https://h.example/a\\(b\\).png)");
    assert_eq!(refs[1].link, "https://h.example/plain.png");
    assert!(has_remote_image_refs(src));
    let out = rewrite_localized_attachment_refs(
        src,
        &[Replacement {
            from: refs[0].original.clone(),
            to: "![[local_MD5.png]]".into(),
        }],
    );
    assert_eq!(
        out.content,
        "x ![[local_MD5.png]] y\n![](https://h.example/plain.png)\n"
    );
}

// --- plan remote-media-sprint WP-1: widened remote-ref shapes and the rewrite round trip ---

const WIDE_SHAPES: &[(&str, &str)] = &[
    (
        r#"![a](https://a.test/x.png "say \"hi\" now")"#,
        "https://a.test/x.png",
    ),
    (
        r"![a](https://a.test/q.png 'it\'s')",
        "https://a.test/q.png",
    ),
    (
        "![a](https://a.test/p.png (paren title))",
        "https://a.test/p.png",
    ),
    ("![a [b] c](https://a.test/n.png)", "https://a.test/n.png"),
    (
        "![w](https://en.wikipedia.org/wiki/Foo_(bar).png)",
        "https://en.wikipedia.org/wiki/Foo_(bar).png",
    ),
    ("![ang](<https://a.test/a.png>)", "https://a.test/a.png"),
    (
        r#"![t](https://a.test/t.png "x ) y")"#,
        "https://a.test/t.png",
    ),
];

#[test]
fn remote_shapes_detected_and_parsed() {
    for &(src, url) in WIDE_SHAPES {
        let body = format!("pre {src} post\n");
        assert!(has_remote_image_refs(&body), "{src}");
        let refs = parse_attachment_refs(&body);
        assert_eq!(refs.len(), 1, "{src}: {refs:?}");
        assert_eq!(refs[0].original, src);
        assert_eq!(refs[0].link, url);
        assert!(refs[0].is_remote && refs[0].is_embed);
    }
}

#[test]
fn remote_shapes_round_trip_through_rewrite() {
    for &(src, _) in WIDE_SHAPES {
        let body = format!("a {src} b {src}\n");
        let refs = parse_attachment_refs(&body);
        let res = rewrite_localized_attachment_refs(
            &body,
            &[r(&refs[0].original, "![](n/_attachments/x/abc_MD5.png)")],
        );
        assert_eq!(
            res.content,
            "a ![](n/_attachments/x/abc_MD5.png) b ![](n/_attachments/x/abc_MD5.png)\n",
            "{src}"
        );
    }
}

#[test]
fn linked_image_yields_inner_remote_image_only() {
    let src = r#"[![alt](https://img.test/i.png "t \"q\"")](https://outer.test/page)"#;
    assert!(has_remote_image_refs(src));
    let refs = parse_attachment_refs(src);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].link, "https://img.test/i.png");
    let res = rewrite_localized_attachment_refs(src, &[r(&refs[0].original, "![](l_MD5.png)")]);
    assert_eq!(res.content, "[![](l_MD5.png)](https://outer.test/page)");
}

#[test]
fn rewrite_skips_fenced_copy() {
    let src = "![x](https://a.test/f_(1).png)\n```\n![x](https://a.test/f_(1).png)\n```\n";
    let refs = parse_attachment_refs(src);
    assert_eq!(refs.len(), 1);
    let res = rewrite_localized_attachment_refs(src, &[r(&refs[0].original, "![](z_MD5.png)")]);
    assert_eq!(
        res.content,
        "![](z_MD5.png)\n```\n![x](https://a.test/f_(1).png)\n```\n"
    );
    assert_eq!(res.applied_from.len(), 1);
}

#[test]
fn fenced_wide_shapes_not_detected() {
    let src = "```\n![a [b]](https://a.test/x.png \"t\")\n```\n";
    assert!(!has_remote_image_refs(src));
    assert!(parse_attachment_refs(src).is_empty());
}

#[test]
fn remote_with_space_or_bad_title_not_an_image() {
    for src in [
        "![](https://a.test/a b.png)",
        "![](https://a.test/a.png junk)",
        "![](https://a.test/a.png \"open)",
    ] {
        assert!(!has_remote_image_refs(src), "{src}");
    }
}

#[test]
fn paren_dest_balanced_or_first_close_fallback() {
    let src = "![](foo(bar.png) tail)";
    assert_eq!(parse_attachment_refs(src)[0].original, src);
    let src = "![](a(b.png)\n";
    assert_eq!(parse_attachment_refs(src)[0].original, "![](a(b.png)");
}

#[test]
fn nested_alt_local_embed_and_raw_destination() {
    let src = "![see [1] here](img/a%20b.png \"t\")";
    let refs = parse_attachment_refs(src);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].link, "img/a b.png");
    assert_eq!(
        md_ref_raw_destination(src).as_deref(),
        Some("img/a%20b.png")
    );
    assert_eq!(
        md_ref_raw_destination("![w](https://w.test/F_(b).png)").as_deref(),
        Some("https://w.test/F_(b).png")
    );
}

#[test]
fn managed_link_with_paren_path_round_trips() {
    let src = "[doc](notes/_attachments/a_(1)/abc_MD5.pdf)";
    let refs = parse_attachment_refs(src);
    assert_eq!(refs.len(), 1);
    let res = rewrite_localized_attachment_refs(src, &[r(src, "[doc](n/abc_MD5.pdf)")]);
    assert_eq!(res.content, "[doc](n/abc_MD5.pdf)");
}

#[test]
fn embed_pages_still_excluded_from_detection() {
    assert!(!has_remote_image_refs(
        "![v](https://www.youtube.com/watch?v=dQw4w9WgXcQ \"t\")"
    ));
}
