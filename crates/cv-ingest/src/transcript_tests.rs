use super::*;

fn seg<'a>(body: &'a str, s: &Section) -> Vec<&'a str> {
    s.segments.iter().map(|r| &body[r.clone()]).collect()
}

#[test]
fn locates_the_section_between_headings() {
    let body =
        "intro\n\n## Transcript\n\n0:01 hello there\n\n0:09 more text\n\n## Notes\n\nafter\n";
    let s = locate(body).unwrap();
    assert_eq!(
        &body[s.body_start..s.end],
        "\n0:01 hello there\n\n0:09 more text\n\n"
    );
    assert_eq!(seg(body, &s), ["0:01 hello there\n\n0:09 more text"]);
}

#[test]
fn heading_match_is_case_insensitive_and_chapters_stay_inside() {
    let body = "## transcript\n\n### One\n\ntext\n\n### Two\n\nmore\n";
    let s = locate(body).unwrap();
    assert_eq!(s.end, body.len());
    assert_eq!(seg(body, &s), ["### One\n\ntext\n\n### Two\n\nmore"]);
    assert!(locate("## Transcripts\n\nx\n").is_none());
    assert!(locate("no heading\n").is_none());
    assert!(locate("```\n## Transcript\n```\n").is_none());
}

#[test]
fn a_level_one_heading_ends_the_section_and_empty_sections_have_no_segments() {
    let body = "## Transcript\n\n# Next\n\ntext\n";
    let s = locate(body).unwrap();
    assert_eq!(&body[s.end..], "# Next\n\ntext\n");
    assert!(s.segments.is_empty());
}

#[test]
fn managed_blocks_are_excluded_and_replace_keeps_them_byte_identical() {
    let top = "%% cv-enrich %%\n> [!abstract]-\n> ## not a heading\n\n%% /cv-enrich %%\n";
    let fold = "%% cv-enrich %%\n> [!removed]-\n> Subscribe now\n\n%% /cv-enrich %%\n";
    let body = format!("{top}\n## Transcript\n\n0:01 raw words here\n\n{fold}");
    let s = locate(&body).unwrap();
    assert_eq!(s.end, body.len(), "the fold is inside the last section");
    assert_eq!(seg(&body, &s), ["0:01 raw words here"]);
    let new = replace(&body, &s.segments[0], "### Intro\n\nRaw words here.\n");
    assert_eq!(
        new,
        format!("{top}\n## Transcript\n\n### Intro\n\nRaw words here.\n\n{fold}")
    );
    assert!(new.ends_with(fold) && new.starts_with(top));
}

#[test]
fn heading_markers_split_segments_and_unterminated_opens_are_text() {
    let body = "## Transcript\n\nalpha\n\n%% cv-enrich heading %%\n## Late\n\nbeta\n";
    let s = locate(body).unwrap();
    assert_eq!(seg(body, &s), ["alpha"]);
    let open = "## Transcript\n\nalpha\n\n%% cv-enrich %%\nbeta\n";
    let s = locate(open).unwrap();
    assert_eq!(seg(open, &s), ["alpha\n\n%% cv-enrich %%\nbeta"]);
    let two = "## Transcript\n\none\n\n%% cv-enrich %%\nx\n%% /cv-enrich %%\n\ntwo\n";
    assert_eq!(seg(two, &locate(two).unwrap()), ["one", "two"]);
}

#[test]
fn small_sections_are_one_chunk() {
    assert_eq!(chunks("a b\n\nc d\n"), ["a b\n\nc d"]);
}

#[test]
fn big_sections_split_at_paragraphs_into_balanced_ordered_chunks() {
    let para = |n: usize| format!("w{n} ").repeat(100).trim().to_owned();
    // 250 paragraphs x 100 words = 25,000 words -> 3 chunks of ~8,400 words.
    let text = (0..250).map(para).collect::<Vec<_>>().join("\n\n");
    let cs = chunks(&text);
    assert_eq!(cs.len(), 3);
    assert!(cs.iter().all(|c| words(c) <= MAX_CHUNK_WORDS));
    assert!(
        cs.iter().all(|c| words(c) >= 7_000),
        "{:?}",
        cs.iter().map(|c| words(c)).collect::<Vec<_>>()
    );
    assert_eq!(
        cs.join("\n\n"),
        text,
        "paragraph boundaries, in order, nothing lost"
    );
}

#[test]
fn one_giant_paragraph_splits_at_word_boundaries() {
    let text = "word ".repeat(30_000);
    let cs = chunks(&text);
    assert_eq!(cs.len(), 3);
    assert!(cs.iter().all(|c| words(c) <= MAX_CHUNK_WORDS));
    assert_eq!(cs.iter().map(|c| words(c)).sum::<usize>(), 30_000);
}

#[test]
fn retention_floor() {
    assert!(retention_ok(100, 60));
    assert!(retention_ok(100, 94));
    assert!(retention_ok(100, 130));
    assert!(!retention_ok(100, 59));
    assert!(!retention_ok(100, 0));
    assert!(!retention_ok(0, 0));
}

#[test]
fn counts_chapter_headings() {
    assert_eq!(chapter_headings("### A\n\nx\n### B\n\n## C\n#### D\n"), 2);
}
