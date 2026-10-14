//! Render-model lowering vectors on synthetic OFM (one per assessment §2 syntax row).

use super::*;

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
                remote: false
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
                remote: false
            },
            Block::Image {
                src: "https://h/x.png".into(),
                alt: "r".into(),
                width: None,
                remote: true
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
