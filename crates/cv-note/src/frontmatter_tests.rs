use super::*;

fn s(v: &str) -> JsValue {
    JsValue::Str(v.into())
}

const DOC: &str = "---\ntitle: 'Quoted Title'   # trailing\n# own-line comment\n\ntags:\n  - a\n  - b\nn: 0x1F\n---\n\nBody text.\n";

#[test]
fn get_str_is_string_only_and_untrimmed() {
    let d = FrontmatterDoc::new("---\na: '  x  '\nb: 5\nc: true\nd: ''\n---\n");
    assert_eq!(d.get_str("a").as_deref(), Some("  x  "));
    assert_eq!(d.get_str("b"), None);
    assert_eq!(d.get_str("c"), None);
    assert_eq!(d.get_str("d").as_deref(), Some(""));
    assert_eq!(d.get_str("missing"), None);
    assert_eq!(FrontmatterDoc::new("---\n- a\n---\n").get_str("a"), None);
}

#[test]
fn get_str_trimmed_drops_empty() {
    let d = FrontmatterDoc::new("---\na: '  x  '\nb: '   '\nc: 5\n---\n");
    assert_eq!(d.get_str_trimmed("a").as_deref(), Some("x"));
    assert_eq!(d.get_str_trimmed("b"), None);
    assert_eq!(d.get_str_trimmed("c"), None);
    assert_eq!(d.get_str_trimmed("missing"), None);
}

#[test]
fn get_bool_is_bool_only() {
    let d = FrontmatterDoc::new("---\na: true\nb: false\nc: 'true'\nd: 1\n---\n");
    assert_eq!(d.get_bool("a"), Some(true));
    assert_eq!(d.get_bool("b"), Some(false));
    assert_eq!(d.get_bool("c"), None);
    assert_eq!(d.get_bool("d"), None);
    assert_eq!(d.get_bool("missing"), None);
}

#[test]
fn get_strings_keeps_strings_only() {
    let d = FrontmatterDoc::new("---\na: [x, 1, true, y]\nb: solo\nc: 5\nd: []\ne:\n---\n");
    assert_eq!(d.get_strings("a"), vec!["x", "y"]);
    assert_eq!(d.get_strings("b"), vec!["solo"]);
    assert!(d.get_strings("c").is_empty());
    assert!(d.get_strings("d").is_empty());
    assert!(d.get_strings("e").is_empty());
    assert!(d.get_strings("missing").is_empty());
}

#[test]
fn get_texts_stringifies_every_item() {
    let d = FrontmatterDoc::new(
        "---\na: [x, 1, true, [p, q]]\nb: solo\nc: 5\nd: false\ne:\nf: []\n---\n",
    );
    assert_eq!(d.get_texts("a"), vec!["x", "1", "true", "p,q"]);
    assert_eq!(d.get_texts("b"), vec!["solo"]);
    assert_eq!(d.get_texts("c"), vec!["5"]);
    assert_eq!(d.get_texts("d"), vec!["false"]);
    assert!(d.get_texts("e").is_empty());
    assert!(d.get_texts("f").is_empty());
    assert!(d.get_texts("missing").is_empty());
}

#[test]
fn set_existing_touches_only_that_entry() {
    let mut d = FrontmatterDoc::new(DOC);
    assert!(d.set("n", &JsValue::Num(5.0)).unwrap());
    assert_eq!(d.text(), DOC.replace("n: 0x1F\n", "n: 5\n"));
}

#[test]
fn set_equal_value_is_byte_identical() {
    let mut d = FrontmatterDoc::new(DOC);
    assert!(!d.set("title", &s("Quoted Title")).unwrap());
    assert!(!d.set("n", &JsValue::Num(31.0)).unwrap());
    assert_eq!(d.text(), DOC);
}

#[test]
fn set_new_key_appends_before_fence() {
    let mut d = FrontmatterDoc::new(DOC);
    assert!(d.set("word-count", &JsValue::Num(2.0)).unwrap());
    assert_eq!(
        d.text(),
        DOC.replace("n: 0x1F\n", "n: 0x1F\nword-count: 2\n")
    );
}

#[test]
fn insert_after_places_new_key_after_anchor() {
    let link = JsValue::Arr(vec![s("[[m/a]]")]);
    let mut d = FrontmatterDoc::new("---\na: 1\nyt-video-id: x\nz: 2\n---\nB\n");
    assert!(d.insert_after("yt-video-id", "yt-metadata", &link).unwrap());
    assert_eq!(
        d.text(),
        "---\na: 1\nyt-video-id: x\nyt-metadata:\n  - \"[[m/a]]\"\nz: 2\n---\nB\n"
    );
    // Anchor absent → end of block.
    let mut d = FrontmatterDoc::new("---\na: 1\n---\n");
    assert!(d.insert_after("nope", "k", &s("v")).unwrap());
    assert_eq!(d.text(), "---\na: 1\nk: v\n---\n");
    // Existing key → set in place.
    let mut d = FrontmatterDoc::new("---\nk: v\nyt-video-id: x\n---\n");
    assert!(!d.insert_after("yt-video-id", "k", &s("v")).unwrap());
}

#[test]
fn set_creates_block_when_absent() {
    let mut d = FrontmatterDoc::new("Just body.\n");
    assert!(d.set("title", &s("note")).unwrap());
    assert_eq!(d.text(), "---\ntitle: note\n---\nJust body.\n");
}

#[test]
fn set_into_empty_block() {
    let mut d = FrontmatterDoc::new("---\n---\nBody\n");
    assert!(d.set("a", &s("b")).unwrap());
    assert_eq!(d.text(), "---\na: b\n---\nBody\n");
}

#[test]
fn set_quotes_ambiguous_strings() {
    let mut d = FrontmatterDoc::new("---\na: 1\n---\n");
    d.set("b", &s("true")).unwrap();
    d.set("c", &s("x: y")).unwrap();
    d.set("d", &s("2026-07-29")).unwrap();
    assert_eq!(
        d.text(),
        "---\na: 1\nb: \"true\"\nc: \"x: y\"\nd: 2026-07-29\n---\n"
    );
    assert_eq!(d.get("b").unwrap(), Some(s("true")));
    assert_eq!(d.get("c").unwrap(), Some(s("x: y")));
}

#[test]
fn remove_keeps_neighbours_and_comments() {
    let mut d = FrontmatterDoc::new(DOC);
    assert!(d.remove("tags").unwrap());
    assert_eq!(d.text(), DOC.replace("tags:\n  - a\n  - b\n", ""));
    assert!(!d.remove("absent").unwrap());
}

#[test]
fn remove_first_key_keeps_block_key_first() {
    let src = "---\ntitle: T\n# note about tags\ntags:\n  - a\nkind: x\n---\nbody\n";
    let mut d = FrontmatterDoc::new(src);
    assert!(d.remove("title").unwrap());
    assert_eq!(
        d.text(),
        "---\ntags:\n  - a\n# note about tags\nkind: x\n---\nbody\n"
    );
    assert!(split_frontmatter(d.text()).0.is_some());
    assert_eq!(
        d.keys().unwrap(),
        vec!["tags".to_owned(), "kind".to_owned()]
    );
    assert_eq!(d.body(), "body\n");
}

#[test]
fn remove_only_key_with_comment_is_refused() {
    let src = "---\ntitle: T\n# keep me\n---\nbody\n";
    let mut d = FrontmatterDoc::new(src);
    assert!(matches!(
        d.remove("title"),
        Err(FmError::UnsupportedLayout(_))
    ));
    assert_eq!(d.text(), src);
}

#[test]
fn remove_only_key_leaves_empty_block() {
    let mut d = FrontmatterDoc::new("---\ntitle: T\n\n---\nbody\n");
    assert!(d.remove("title").unwrap());
    assert_eq!(d.text(), "---\n---\nbody\n");
    assert!(d.set("a", &s("x")).unwrap());
    assert_eq!(d.text(), "---\na: x\n---\nbody\n");
}

#[test]
fn append_to_block_list_adds_one_line() {
    let mut d = FrontmatterDoc::new(DOC);
    assert!(d.append_to_list("tags", &s("c")).unwrap());
    assert_eq!(d.text(), DOC.replace("  - b\n", "  - b\n  - c\n"));
    assert!(!d.append_to_list("tags", &s("a")).unwrap());
}

#[test]
fn append_to_flow_list_and_absent() {
    let mut d = FrontmatterDoc::new("---\nx: 1\ntags: [a]\n---\n");
    assert!(d.append_to_list("tags", &s("b")).unwrap());
    assert_eq!(d.text(), "---\nx: 1\ntags:\n  - a\n  - b\n---\n");
    assert!(d.append_to_list("new", &s("z")).unwrap());
    assert_eq!(d.get("new").unwrap(), Some(JsValue::Arr(vec![s("z")])));
}

#[test]
fn sort_moves_whole_chunks() {
    let mut d = FrontmatterDoc::new("---\nb: 1 # c1\nc:\n  - x\na: 'q'\n---\nB\n");
    assert!(d.sort_keys(&["a".into(), "c".into()]).unwrap());
    assert_eq!(d.text(), "---\na: 'q'\nc:\n  - x\nb: 1 # c1\n---\nB\n");
    assert!(!d.sort_keys(&["a".into(), "c".into()]).unwrap());
}

#[test]
fn crlf_preserved() {
    let mut d = FrontmatterDoc::new("---\r\na: 1\r\n---\r\nB\r\n");
    d.set("b", &JsValue::Num(2.0)).unwrap();
    assert_eq!(d.text(), "---\r\na: 1\r\nb: 2\r\n---\r\nB\r\n");
}

#[test]
fn rename_in_place_keeps_value_bytes() {
    let mut d = FrontmatterDoc::new("---\nvideo_ids: ['x'] # keep\nz: 1\n---\n");
    assert!(d.rename_key("video_ids", "yt-video-ids").unwrap());
    assert_eq!(d.text(), "---\nyt-video-ids: ['x'] # keep\nz: 1\n---\n");
}

#[test]
fn unsupported_layout_refused() {
    let mut d = FrontmatterDoc::new("---\n{a: 1}\n---\n");
    assert!(d.set("b", &s("c")).is_err());
}

#[test]
fn blank_line_after_yaml() {
    let mut d = FrontmatterDoc::new("---\na: 1\n---\nBody\n");
    assert!(d.ensure_blank_line_after());
    assert_eq!(d.text(), "---\na: 1\n---\n\nBody\n");
    assert!(!d.ensure_blank_line_after());
}

// ---- the canonical span + set_edit (the cases every frontmatter detector in the workspace
// must agree on).

fn split(t: &str) -> (&str, &str) {
    t.split_at(span(t).map_or(0, |r| r.end))
}

#[test]
fn span_splits_block_from_body() {
    assert_eq!(
        split("---\na: 1\n---\nbody\n"),
        ("---\na: 1\n---\n", "body\n")
    );
    assert_eq!(split("no fm\n"), ("", "no fm\n"));
    assert_eq!(split("---\nunclosed\n"), ("", "---\nunclosed\n"));
    assert_eq!(split("---\na: 1\n---"), ("---\na: 1\n---", ""));
    assert_eq!(split("---\n---\nb\n"), ("---\n---\n", "b\n"));
}

#[test]
fn span_handles_crlf_bom_and_body_rules() {
    assert_eq!(span("---\r\na: 1\r\n---\r\nb\r\n"), Some(0..16));
    // BOM: the range starts at the fence; `..end` keeps the BOM with the block.
    let bom = "\u{feff}---\na: 1\n---\n\nbody\n";
    assert_eq!(span(bom), Some(3..16));
    assert_eq!(split(bom).1, "\nbody\n");
    // A later `---` (thematic break) closes nothing when the block was already closed,
    // and a body `---` never opens a block.
    assert_eq!(split("---\na: 1\n---\nx\n---\ny\n").1, "x\n---\ny\n");
    assert_eq!(span("text\n---\na: 1\n---\n"), None);
    // ofm's rule: the first line must look like a YAML key; fences are exact lines.
    assert_eq!(span("---\nsome prose\n---\n"), None);
    assert_eq!(span("--- \na: 1\n---\n"), None);
}

fn apply(t: &str, key: &str, v: &JsValue) -> Option<String> {
    let (r, with) = FrontmatterDoc::new(t).set_edit(key, v).unwrap()?;
    let mut out = t.to_owned();
    out.replace_range(r, &with);
    Some(out)
}

#[test]
fn set_edit_is_a_minimal_splice() {
    let t = JsValue::Bool(true);
    let f = JsValue::Bool(false);
    let doc = FrontmatterDoc::new("---\nread:   true   # done?\n---\n");
    assert_eq!(
        doc.set_edit("read", &f).unwrap(),
        Some((12..16, "false".into()))
    );
    assert_eq!(
        apply("---\nread: ~ # c\n---\n", "read", &t).as_deref(),
        Some("---\nread: true # c\n---\n")
    );
    assert_eq!(
        apply("---\nread:  # c\n---\n", "read", &t).as_deref(),
        Some("---\nread: true # c\n---\n")
    );
    assert_eq!(
        apply("---\nread:\n---\n", "read", &t).as_deref(),
        Some("---\nread: true\n---\n")
    );
    assert_eq!(
        apply("---\ntitle: x\n---\nb\n", "read", &t).as_deref(),
        Some("---\ntitle: x\nread: true\n---\nb\n")
    );
    assert_eq!(
        apply("body\n", "read", &t).as_deref(),
        Some("---\nread: true\n---\nbody\n")
    );
    assert_eq!(
        apply("---\r\nn: 1\r\n---\r\n", "read", &t).as_deref(),
        Some("---\r\nn: 1\r\nread: true\r\n---\r\n")
    );
    assert_eq!(
        apply("\u{feff}body\n", "read", &t).as_deref(),
        Some("\u{feff}---\nread: true\n---\nbody\n")
    );
    assert_eq!(
        apply("\u{feff}---\nread: false\n---\n", "read", &t).as_deref(),
        Some("\u{feff}---\nread: true\n---\n")
    );
    assert_eq!(
        FrontmatterDoc::new("---\nread: true\n---\n")
            .set_edit("read", &t)
            .unwrap(),
        None
    );
}

#[test]
fn set_edit_refuses_unterminated_block() {
    for t in ["---\nread: true\n", "---\nsome prose\n---\n"] {
        assert!(matches!(
            FrontmatterDoc::new(t).set_edit("read", &JsValue::Bool(true)),
            Err(FmError::UnsupportedLayout(_))
        ));
    }
}
