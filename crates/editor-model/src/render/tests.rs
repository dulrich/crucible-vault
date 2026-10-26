//! Render-model lowering vectors on synthetic OFM (one per assessment §2 syntax row).

use super::*;
use crate::edits::{frontmatter_bool_edit, task_set_status, task_toggle};
use crate::props::PropValue;

fn t(s: &str) -> Inline {
    Inline::Text {
        text: s.into(),
        style: Style::default(),
    }
}

fn styled(s: &str, style: Style) -> Inline {
    Inline::Text {
        text: s.into(),
        style,
    }
}

fn one(src: &str) -> Block {
    let m = lower(src);
    assert_eq!(m.blocks.len(), 1, "{:#?}", m.blocks);
    m.blocks.into_iter().next().expect("one block")
}

#[test]
fn headings_and_paragraphs() {
    let m = lower("---\ntitle: x\n---\n# Top\n\n### Three\n\nplain text\n");
    assert_eq!(
        m.blocks,
        vec![
            Block::Heading {
                level: 1,
                inlines: vec![t("Top")]
            },
            Block::Heading {
                level: 3,
                inlines: vec![t("Three")]
            },
            Block::Paragraph(vec![t("plain text")]),
        ]
    );
}

#[test]
fn inline_styles() {
    let b = one("**b** *i* ~~s~~ ==hi== `c`\n");
    let strong = Style {
        strong: true,
        ..Style::default()
    };
    let em = Style {
        em: true,
        ..Style::default()
    };
    let strike = Style {
        strike: true,
        ..Style::default()
    };
    let hi = Style {
        highlight: true,
        ..Style::default()
    };
    assert_eq!(
        b,
        Block::Paragraph(vec![
            styled("b", strong),
            t(" "),
            styled("i", em),
            t(" "),
            styled("s", strike),
            t(" "),
            styled("hi", hi),
            t(" "),
            Inline::Code("c".into()),
        ])
    );
}

#[test]
fn highlight_spans_across_emphasis() {
    let Block::Paragraph(v) = one("a ==x **y** z== b\n") else {
        panic!("paragraph")
    };
    assert!(v.iter().any(|i| matches!(i, Inline::Text { text, style } if text == "y" && style.strong && style.highlight)));
    assert_eq!(v.last(), Some(&t(" b")));
}

#[test]
fn lists_and_nonstandard_tasks() {
    let b = one("- [>] fwd\n- [ ] open\n- [x] done\n- [-] cancelled\n- [/] half\n- plain\n");
    let Block::List { ordered, items, .. } = b else {
        panic!("list")
    };
    assert!(!ordered);
    let tasks: Vec<_> = items.iter().map(|i| i.task).collect();
    assert_eq!(
        tasks,
        vec![Some('>'), Some(' '), Some('x'), Some('-'), Some('/'), None]
    );
    assert_eq!(items[0].children, vec![Block::Paragraph(vec![t("fwd")])]);
    let Block::List { ordered, start, .. } = one("3. a\n4. b\n") else {
        panic!("ordered")
    };
    assert!(ordered);
    assert_eq!(start, 3);
}

#[test]
fn table_with_alignments() {
    let b = one("| a | b |\n|:-|-:|\n| 1 | **2** |\n");
    let Block::Table {
        header,
        rows,
        alignments,
    } = b
    else {
        panic!("table")
    };
    assert_eq!(alignments, vec![Align::Left, Align::Right]);
    assert_eq!(header, vec![vec![t("a")], vec![t("b")]]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], vec![t("1")]);
}

#[test]
fn folded_callout_with_title() {
    let b = one("> [!warning]- Folded title\n> body line\n>\n> second\n");
    assert_eq!(
        b,
        Block::Callout {
            kind: "warning".into(),
            title: Some("Folded title".into()),
            fold: Some(Fold::Folded),
            children: vec![
                Block::Paragraph(vec![t("body line")]),
                Block::Paragraph(vec![t("second")]),
            ],
        }
    );
    let Block::Callout {
        kind, title, fold, ..
    } = one("> [!NOTE]+\n> x\n")
    else {
        panic!("callout")
    };
    assert_eq!(
        (kind.as_str(), title, fold),
        ("note", None, Some(Fold::Open))
    );
    assert!(matches!(one("> just a quote\n"), Block::BlockQuote(_)));
}

#[test]
fn footnote_def_and_ref() {
    let m = lower("see[^n]\n\n[^n]: the def\n");
    assert_eq!(
        m.blocks,
        vec![Block::Paragraph(vec![
            t("see"),
            Inline::FootnoteRef {
                name: "n".into(),
                ix: 1
            }
        ])]
    );
    assert_eq!(
        m.footnotes,
        vec![FootnoteDef {
            name: "n".into(),
            blocks: vec![Block::Paragraph(vec![t("the def")])],
        }]
    );
}

#[test]
fn code_and_display_math() {
    assert_eq!(
        one("```rust\nfn x() {}\n```\n"),
        Block::CodeBlock {
            lang: "rust".into(),
            text: "fn x() {}".into()
        }
    );
    assert_eq!(
        one("$$\nx^2 + y\n$$\n"),
        Block::CodeBlock {
            lang: "math".into(),
            text: "x^2 + y".into()
        }
    );
}

#[test]
fn quote_break() {
    let m = lower("> q\n\n---\n");
    assert_eq!(
        m.blocks,
        vec![
            Block::BlockQuote(vec![Block::Paragraph(vec![t("q")])]),
            Block::ThematicBreak
        ]
    );
}

#[test]
fn wikilinks_alias_fragment_md_links_tags() {
    let b = one("a [[dir/Note#Head|Ali]] [[Plain]] [l](https://x.y) #tag/sub #123 x\n");
    assert_eq!(
        b,
        Block::Paragraph(vec![
            t("a "),
            Inline::WikiLink {
                target: "dir/Note".into(),
                fragment: Some("Head".into()),
                alias: Some("Ali".into())
            },
            t(" "),
            Inline::WikiLink {
                target: "Plain".into(),
                fragment: None,
                alias: None
            },
            t(" "),
            Inline::Link {
                url: "https://x.y".into(),
                label: "l".into()
            },
            t(" "),
            Inline::Tag("tag/sub".into()),
            t(" #123 x"),
        ])
    );
}

#[test]
fn embed_vs_transclusion_and_images() {
    let m = lower(
        "a ![[pic.png|300]] b\n\n![[Other#Sec]]\n\n![alt|200](img/x.jpg) ![r](https://h/x.png)\n",
    );
    assert_eq!(
        m.blocks,
        vec![
            Block::Paragraph(vec![t("a ")]),
            Block::Image {
                src: "pic.png".into(),
                alt: "pic.png".into(),
                width: Some(300),
                remote: false,
                link: None
            },
            Block::Paragraph(vec![t(" b")]),
            Block::Transclusion {
                target: "Other".into(),
                fragment: Some("Sec".into()),
                content: None
            },
            Block::Image {
                src: "img/x.jpg".into(),
                alt: "alt".into(),
                width: Some(200),
                remote: false,
                link: None
            },
            Block::Image {
                src: "https://h/x.png".into(),
                alt: "r".into(),
                width: None,
                remote: true,
                link: None
            },
        ]
    );
    assert_eq!(
        images(&m),
        vec![
            ("pic.png".into(), false),
            ("img/x.jpg".into(), false),
            ("https://h/x.png".into(), true)
        ]
    );
}

#[test]
fn view_fence_placeholder() {
    assert_eq!(
        one("```crucible-view\n_custom/crucible-vault/views/tasks.toml#Overdue\n```\n"),
        Block::ViewFence {
            file: "_custom/crucible-vault/views/tasks.toml".into(),
            name: "Overdue".into()
        }
    );
}

#[test]
fn transclusion_depth_one() {
    let mut m = lower("# A\n\n![[B]]\n");
    let mut fetch = |t: &str| match t {
        "B" => Some("b text\n\n![[C]]\n".to_owned()),
        "C" => Some("c text\n".to_owned()),
        _ => None,
    };
    transclude(&mut m, &mut fetch);
    assert_eq!(
        m.blocks[1],
        Block::Transclusion {
            target: "B".into(),
            fragment: None,
            content: Some(vec![
                Block::Paragraph(vec![t("b text")]),
                Block::Paragraph(vec![Inline::WikiLink {
                    target: "C".into(),
                    fragment: None,
                    alias: None
                }]),
            ]),
        }
    );
}

#[test]
fn transclusion_heading_section_and_missing() {
    let mut m = lower("![[B#Two]]\n\n![[Missing]]\n");
    let mut fetch = |t: &str| {
        (t == "B").then(|| "# One\n\na\n\n# Two\n\nb\n\n## Sub\n\nc\n\n# Three\n\nd\n".to_owned())
    };
    transclude(&mut m, &mut fetch);
    let Block::Transclusion {
        content: Some(c), ..
    } = &m.blocks[0]
    else {
        panic!("filled")
    };
    assert_eq!(c.len(), 4);
    assert!(matches!(
        m.blocks[1],
        Block::Transclusion { content: None, .. }
    ));
}

#[test]
fn remote_detection() {
    assert!(is_remote("https://x"));
    assert!(is_remote("data:image/png;base64,xx"));
    assert!(!is_remote("img/x.png"));
    assert!(!is_remote("C:x"));
    assert!(is_image_target("a/b.PNG"));
    assert!(!is_image_target("note"));
}

// ---- editor-usability-round WP-2: frontmatter properties + task source lines ----

#[test]
fn frontmatter_becomes_properties_and_body_is_unchanged() {
    let src = "---\ntitle: Hello\ncount: 3\ntags:\n  - a\n  - b\nup: \"[[Parent|P]]\"\nempty:\n---\npara\n";
    let m = lower(src);
    assert_eq!(m.blocks, lower("para\n").blocks);
    let keys: Vec<_> = m.properties.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(keys, ["title", "count", "tags", "up", "empty"]);
    assert_eq!(m.properties[0].values, [PropValue::Text("Hello".into())]);
    assert_eq!(m.properties[1].values, [PropValue::Text("3".into())]);
    assert_eq!(
        m.properties[2].values,
        [PropValue::Text("a".into()), PropValue::Text("b".into())]
    );
    assert_eq!(
        m.properties[3].values,
        [PropValue::Link {
            target: "Parent".into(),
            fragment: None,
            alias: Some("P".into())
        }]
    );
    assert!(m.properties[4].values.is_empty());
    assert!(lower("no frontmatter\n").properties.is_empty());
    assert!(lower("---\na: [unclosed\n---\nx\n").properties.is_empty());
}

#[test]
fn task_items_carry_full_source_lines() {
    let src = "---\na: 1\n---\n# H\n\n- [ ] one\n- plain\n  - [x] nested\n\n> [!note]\n> - [/] in callout\n";
    let m = lower(src);
    assert_eq!(task_lines(&m), vec![(5, ' '), (7, 'x'), (10, '/')]);
    for (line, _) in task_lines(&m) {
        assert!(task_toggle(src, line).is_some(), "line {line} has a box");
    }
    let Block::List { items, .. } = &m.blocks[1] else {
        panic!("list")
    };
    assert_eq!(items[1].line, None, "non-task items carry no line");
}

#[test]
fn transcluded_tasks_carry_no_source_line() {
    let mut m = lower("- [ ] host\n\n![[B]]\n");
    let mut fetch = |t: &str| (t == "B").then(|| "- [ ] embedded\n- [x] two\n".to_owned());
    transclude(&mut m, &mut fetch);
    assert_eq!(task_lines(&m), vec![(0, ' ')]);
    let Block::Transclusion {
        content: Some(c), ..
    } = &m.blocks[1]
    else {
        panic!("filled")
    };
    let Block::List { items, .. } = &c[0] else {
        panic!("list")
    };
    assert!(items.iter().all(|i| i.task.is_some() && i.line.is_none()));
}

#[test]
fn task_toggle_targets_the_status_char() {
    let src = "- [ ] a\n  * [x] b\n1. [/] c\n> - [-] d\nnot [ ] a task\n";
    assert_eq!(task_toggle(src, 0), Some((3..4, 'x')));
    assert_eq!(task_toggle(src, 1), Some((5..6, ' ')));
    assert_eq!(task_toggle(src, 2), Some((4..5, 'x')));
    assert_eq!(task_toggle(src, 3), Some((5..6, 'x')));
    assert_eq!(task_toggle(src, 4), None);
    assert_eq!(task_toggle(src, 9), None);
}

#[test]
fn task_set_status_targets_the_status_char() {
    let src = "- [ ] a\n  * [x] b\nnot [ ] a task\n";
    assert_eq!(task_set_status(src, 0, '!'), Some((3..4, '!')));
    assert_eq!(task_set_status(src, 1, '>'), Some((5..6, '>')));
    assert_eq!(task_set_status(src, 1, 'x'), None, "already that status");
    assert_eq!(task_set_status(src, 2, 'x'), None, "not a task");
    assert_eq!(task_set_status(src, 9, 'x'), None);
}

/// plan editor-ui-round-3 WP-5: block source provenance (full-note lines, frontmatter included).
#[test]
fn blocks_carry_their_source_line_ranges() {
    let m = lower("---\na: 1\n---\n# H\n\npara one\npara two\n\n- x\n- y\n");
    assert_eq!(m.blocks.len(), 3);
    assert_eq!(m.block_lines, vec![3..4, 5..7, 8..10]);
    assert_eq!(m.block_at_line(6), Some(1));
    assert_eq!(m.block_at_line(4), None);
}

/// plan editor-ui-round-3 WP-6: place-keeping maps line → block → first line, both directions.
#[test]
fn place_keeping_maps_lines_to_blocks_and_back() {
    let src = "---\na: 1\n---\n# H\n\npara\n\n- x\n- y\n\n```\ncode\nmore\n```\n";
    let m = lower(src);
    assert_eq!(m.block_lines, vec![3..4, 5..6, 7..9, 10..14]);
    // Frontmatter and blank lines map to the next block; inner lines to their block.
    assert_eq!(m.block_for_line(0), Some(0));
    assert_eq!(m.block_for_line(4), Some(1));
    assert_eq!(m.block_for_line(8), Some(2));
    assert_eq!(m.block_for_line(12), Some(3));
    assert_eq!(m.block_for_line(99), Some(3), "past the end = last block");
    // Back: a block's first line maps to itself.
    for (i, r) in m.block_lines.iter().enumerate() {
        assert_eq!(m.block_for_line(r.start), Some(i));
    }
    assert_eq!(lower("").block_for_line(0), None);
}

// ---- plan usability-sprint-captures-links WP-4: interactive frontmatter ----

#[test]
fn bool_toggle_rewrites_only_the_scalar() {
    let src = "---\ntitle: x\nread: false\nn: 1\n---\nbody read: false\n";
    assert_eq!(
        apply_bool_set(src, "read", None).as_deref(),
        Some("---\ntitle: x\nread: true\nn: 1\n---\nbody read: false\n")
    );
    let src = "---\nread:   true   # done?\n---\n";
    assert_eq!(
        apply_bool_set(src, "read", None).as_deref(),
        Some("---\nread:   false   # done?\n---\n")
    );
}

#[test]
fn bool_toggle_refuses_non_boolean_shapes() {
    // Ported from the retired `frontmatter_bool_toggle`: non-boolean scalars are
    // refused; a nested or prefix-matching key is not `read`, so the setter upserts a new
    // top-level key instead of touching it.
    assert!(frontmatter_bool_edit("---\nread: \"true\"\n---\n", "read", None).is_none());
    assert!(frontmatter_bool_edit("---\nread: true yes\n---\n", "read", None).is_none());
    for src in [
        "---\nm:\n  read: true\n---\n",
        "---\nother: true\n---\n",
        "---\nreadx: true\n---\n",
    ] {
        let out = apply_bool_set(src, "read", None).expect("upsert");
        assert!(out.starts_with(&src[..src.len() - 4]), "{out:?}");
        assert!(out.ends_with("read: true\n---\n"), "{out:?}");
    }
}

#[test]
fn bool_toggle_handles_crlf() {
    let src = "---\r\na: 1\r\nread: true\r\n---\r\n";
    assert_eq!(
        apply_bool_set(src, "read", None).as_deref(),
        Some("---\r\na: 1\r\nread: false\r\n---\r\n")
    );
}

#[test]
fn frontmatter_bools_and_urls_become_typed_values() {
    let src = "---\nread: true\nsrc: https://ex.com/a\nmd: \"[Site](https://ex.com/b)\"\nrel: \"[x](notes/a.md)\"\nauthor: [\"[[A]]\", \"[[B]]\"]\n---\n";
    let m = lower(src);
    assert_eq!(m.properties[0].values, [PropValue::Bool(true)]);
    assert_eq!(
        m.properties[1].values,
        [PropValue::Url {
            url: "https://ex.com/a".into(),
            label: "https://ex.com/a".into()
        }]
    );
    assert_eq!(
        m.properties[2].values,
        [PropValue::Url {
            url: "https://ex.com/b".into(),
            label: "Site".into()
        }]
    );
    assert_eq!(
        m.properties[3].values,
        [PropValue::Text("[x](notes/a.md)".into())]
    );
    let link = |t: &str| PropValue::Link {
        target: t.into(),
        fragment: None,
        alias: None,
    };
    assert_eq!(m.properties[4].values, [link("A"), link("B")]);
}

#[test]
fn linked_image_renders_as_image_with_link() {
    let m = lower("[![](a.jpg)](https://x)\n");
    assert_eq!(
        m.blocks,
        vec![Block::Image {
            src: "a.jpg".into(),
            alt: String::new(),
            width: None,
            remote: false,
            link: Some("https://x".into()),
        }]
    );
}

#[test]
fn linked_image_with_alt_keeps_alt() {
    let m = lower("[![alt](a.jpg)](https://x)\n");
    assert_eq!(
        m.blocks,
        vec![Block::Image {
            src: "a.jpg".into(),
            alt: "alt".into(),
            width: None,
            remote: false,
            link: Some("https://x".into()),
        }]
    );
}

/// Plan round-6 WP-4: vault link targets (wiki + relative md + linked-image href), nested
/// included; remote URLs and `[[#h]]` excluded.
#[test]
fn link_targets_list_vault_links_only() {
    let m = lower(
        "see [[A]] and [[#h]] [x](b.md) [y](https://e.com)\n\n- [[C|alias]]\n\n> [[D#sec]]\n\n[![](i.png)](E.md)\n",
    );
    assert_eq!(link_targets(&m), ["A", "b.md", "C", "D", "E.md"]);
}

/// Plan round-6 WP-5: frontmatter wikilink values are listed (body first), URLs and plain text
/// are not.
#[test]
fn link_targets_cover_frontmatter_links() {
    let m = lower(
        "---\nup: \"[[Parent]]\"\nrel:\n  - \"[[X|x]]\"\n  - plain\nsrc: https://e.com\n---\n[[A]]\n",
    );
    assert_eq!(link_targets(&m), ["A", "Parent", "X"]);
}

// ---- plan round-8-intake-views-anchor WP-3: frontmatter boolean upsert ----

fn apply_bool_set(src: &str, key: &str, to: Option<bool>) -> Option<String> {
    let (range, with) = frontmatter_bool_edit(src, key, to)?;
    let mut out = src.to_owned();
    out.replace_range(range, &with);
    Some(out)
}

#[test]
fn bool_set_upserts_missing_null_and_blockless() {
    let set = |s: &str| apply_bool_set(s, "read", None);
    assert_eq!(
        set("---\ntitle: x\n---\nb\n").as_deref(),
        Some("---\ntitle: x\nread: true\n---\nb\n")
    );
    assert_eq!(
        set("---\nread:\n---\n").as_deref(),
        Some("---\nread: true\n---\n")
    );
    assert_eq!(
        set("---\nread: null\n---\n").as_deref(),
        Some("---\nread: true\n---\n")
    );
    assert_eq!(
        set("---\nread: ~ # c\n---\n").as_deref(),
        Some("---\nread: true # c\n---\n")
    );
    assert_eq!(
        set("---\nread:  # c\n---\n").as_deref(),
        Some("---\nread: true # c\n---\n")
    );
    assert_eq!(
        set("body\n").as_deref(),
        Some("---\nread: true\n---\nbody\n")
    );
    assert_eq!(
        set("---\r\nn: 1\r\n---\r\n").as_deref(),
        Some("---\r\nn: 1\r\nread: true\r\n---\r\n")
    );
    assert_eq!(
        set("---\nread: true\n---\n").as_deref(),
        Some("---\nread: false\n---\n")
    );
    // Explicit target: equal → no edit; non-boolean shapes refused.
    assert!(frontmatter_bool_edit("---\nread: true\n---\n", "read", Some(true)).is_none());
    assert_eq!(
        apply_bool_set("---\nread: true\n---\n", "read", Some(false)).as_deref(),
        Some("---\nread: false\n---\n")
    );
    assert!(frontmatter_bool_edit("---\nread: \"true\"\n---\n", "read", None).is_none());
    assert!(frontmatter_bool_edit("---\nread: yes\n---\n", "read", None).is_none());
    assert!(frontmatter_bool_edit("---\nread: true\n", "read", None).is_none());
}

/// Plan round-15 WP-4: a PDF embed is its own block carrying the 1-based page and the alias
/// width; other external kinds stay file cards.
#[test]
fn pdf_embeds_lower_to_pdf_embed_blocks() {
    let pdf = |target: &str, page: u32, width: Option<u32>| Block::PdfEmbed {
        target: target.into(),
        page,
        width,
    };
    assert_eq!(lower("![[x.pdf]]\n").blocks, vec![pdf("x.pdf", 1, None)]);
    assert_eq!(
        lower("![[x.pdf#page=3]]\n").blocks,
        vec![pdf("x.pdf", 3, None)]
    );
    assert_eq!(
        lower("![[x.pdf|300]]\n").blocks,
        vec![pdf("x.pdf", 1, Some(300))]
    );
    assert_eq!(
        lower("![[x.pdf#page=2|300]]\n").blocks,
        vec![pdf("x.pdf", 2, Some(300))]
    );
    assert_eq!(
        lower("![[x.pdf#page=0]]\n").blocks,
        vec![pdf("x.pdf", 1, None)]
    );
    assert_eq!(
        lower("![[x.pdf#page=abc]]\n").blocks,
        vec![pdf("x.pdf", 1, None)]
    );
    assert_eq!(
        lower("![[x.docx]]\n").blocks,
        vec![Block::FileEmbed {
            target: "x.docx".into()
        }]
    );
    assert_eq!(
        crate::render::pdf_embeds(&lower("![[a.pdf#page=2]]\n\n![[b.pdf]]\n")),
        vec![("a.pdf".to_owned(), 2), ("b.pdf".to_owned(), 1)]
    );
}
