use super::*;

fn kinds(line: &str) -> Vec<(&str, Kind)> {
    let (spans, _) = scan_line(
        LineState {
            block: Block::Body,
            in_callout: false,
        },
        line,
    );
    spans.into_iter().map(|(r, k)| (&line[r], k)).collect()
}

#[test]
fn every_inline_kind_has_a_vector() {
    assert_eq!(
        kinds("## Title here"),
        vec![("## Title here", Kind::Heading(2))]
    );
    assert_eq!(
        kinds("see [[Note|alias]]"),
        vec![("[[Note|alias]]", Kind::Wikilink)]
    );
    assert_eq!(
        kinds("![[embed.png]]"),
        vec![("![[embed.png]]", Kind::Embed)]
    );
    assert_eq!(
        kinds("a [t](http://x.y/#z)"),
        vec![("[t](http://x.y/#z)", Kind::MdLink)]
    );
    assert_eq!(kinds("x #tag/sub y"), vec![("#tag/sub", Kind::Tag)]);
    assert_eq!(
        kinds("*em* and _em_"),
        vec![("*em*", Kind::Emphasis), ("_em_", Kind::Emphasis)]
    );
    assert_eq!(
        kinds("**bold** __b__"),
        vec![("**bold**", Kind::Strong), ("__b__", Kind::Strong)]
    );
    assert_eq!(kinds("say ==hi=="), vec![("==hi==", Kind::Highlight)]);
    assert_eq!(kinds("- [>] moved"), vec![("[>]", Kind::TaskMarker('>'))]);
    assert_eq!(
        kinds("  * [x] done #t"),
        vec![("[x]", Kind::TaskMarker('x')), ("#t", Kind::Tag)]
    );
    assert_eq!(kinds("1. [ ] open"), vec![("[ ]", Kind::TaskMarker(' '))]);
    assert_eq!(
        kinds("> [!note] Title"),
        vec![("[!note] Title", Kind::CalloutHeader)]
    );
    assert_eq!(kinds("text[^1] more"), vec![("[^1]", Kind::Footnote)]);
    assert_eq!(kinds("[^1]: def"), vec![("[^1]:", Kind::Footnote)]);
    assert_eq!(kinds("use `code #x` ok"), vec![("`code #x`", Kind::Code)]);
}

#[test]
fn tags_not_in_code_urls_words_or_numbers() {
    assert!(kinds("`#notatag`").iter().all(|(_, k)| *k != Kind::Tag));
    assert!(kinds("https://example.com/#frag").is_empty());
    assert!(kinds("a#b").is_empty());
    assert!(kinds("issue #123").is_empty());
    assert_eq!(kinds("#ünï"), vec![("#ünï", Kind::Tag)]);
    assert_eq!(kinds("snake_case_word"), vec![]);
}

#[test]
fn frontmatter_state_carries() {
    let doc = "---\ntitle: Hi #x\ntags:\n  - a\n---\n# H\ntitle: no";
    let lines: Vec<&str> = doc.split('\n').collect();
    let spans = scan_document(doc);
    let k = |i: usize| -> Vec<(&str, Kind)> {
        spans[i]
            .iter()
            .map(|(r, k)| (&lines[i][r.clone()], *k))
            .collect()
    };
    assert_eq!(k(0), vec![("---", Kind::FenceMarker)]);
    assert_eq!(
        k(1),
        vec![
            ("title", Kind::FrontmatterKey),
            ("Hi #x", Kind::FrontmatterValue)
        ]
    );
    assert_eq!(k(2), vec![("tags", Kind::FrontmatterKey)]);
    assert_eq!(k(3), vec![("a", Kind::FrontmatterValue)]);
    assert_eq!(k(4), vec![("---", Kind::FenceMarker)]);
    assert_eq!(k(5), vec![("# H", Kind::Heading(1))]);
    assert!(k(6).is_empty(), "body `key: v` is not frontmatter");
    // `---` later in the body is not frontmatter.
    assert!(scan_document("x\n---\na: b")[2].is_empty());
}

#[test]
fn fences_carry_state_backtick_and_tilde() {
    let doc = "```rust\n# not heading\n~~~\n```\n~~~~\n```\n#tag in fence\n~~~~\n#real";
    let s = scan_document(doc);
    assert_eq!(s[0], vec![(0..7, Kind::FenceMarker)]);
    assert_eq!(s[1], vec![(0..13, Kind::Code)]);
    assert_eq!(s[2], vec![(0..3, Kind::Code)], "~~~ does not close ```");
    assert_eq!(s[3], vec![(0..3, Kind::FenceMarker)]);
    assert_eq!(s[4], vec![(0..4, Kind::FenceMarker)]);
    assert_eq!(s[5], vec![(0..3, Kind::Code)]);
    assert_eq!(s[6], vec![(0..13, Kind::Code)]);
    assert_eq!(s[7], vec![(0..4, Kind::FenceMarker)]);
    assert_eq!(s[8], vec![(0..5, Kind::Tag)]);
}

#[test]
fn callout_state_tracks_continuation() {
    let (_, st) = scan_line(LineState::default(), "> [!warning] Careful");
    assert!(st.in_callout);
    let (sp, st2) = scan_line(st, "> body [[link]]");
    assert!(st2.in_callout);
    assert_eq!(sp, vec![(7..15, Kind::Wikilink)]);
    let (_, st3) = scan_line(st2, "plain");
    assert!(!st3.in_callout);
}

/// The iced adapter's contract: per-line states cached, `change_line(n)` resumes at line n from
/// the saved state. Simulated here: re-scan from a mid-fence line using the cached state equals
/// a full re-scan of the edited document.
#[test]
fn change_line_reentry_mid_fence_matches_full_rescan() {
    let doc = "intro\n```\ncode #a\nmore\n```\n#after";
    let lines: Vec<&str> = doc.split('\n').collect();
    let mut states = vec![LineState::default()];
    for l in &lines {
        let (_, n) = scan_line(*states.last().expect("state"), l);
        states.push(n);
    }
    // Edit line 3 (inside the fence) and re-enter from there with the cached state.
    let mut edited = lines.clone();
    edited[3] = "changed **x**";
    let mut st = states[3];
    let mut tail = Vec::new();
    for l in &edited[3..] {
        let (s, n) = scan_line(st, l);
        tail.push(s);
        st = n;
    }
    let full = scan_document(&edited.join("\n"));
    assert_eq!(&full[3..], tail.as_slice());
    assert_eq!(tail[0], vec![(0..13, Kind::Code)]);

    // Editing the opening fence away changes everything below — re-entry at line 1 agrees.
    let mut opened = lines.clone();
    opened[1] = "no fence";
    let mut st = states[1];
    let mut tail = Vec::new();
    for l in &opened[1..] {
        let (s, n) = scan_line(st, l);
        tail.push(s);
        st = n;
    }
    assert_eq!(&scan_document(&opened.join("\n"))[1..], tail.as_slice());
}

#[test]
fn crlf_is_ignored() {
    let s = scan_document("---\r\na: b\r\n---\r\n# h\r");
    assert_eq!(
        s[1],
        vec![(0..1, Kind::FrontmatterKey), (3..4, Kind::FrontmatterValue)]
    );
    assert_eq!(s[3], vec![(0..3, Kind::Heading(1))]);
}
