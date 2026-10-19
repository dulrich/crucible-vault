//! Chunker regression goldens plus the vectors of Crucible's `tests/searchChunker.test.mjs`.
//!
//! `fixtures/golden.json` holds synthetic inputs (carried over from the TS-generated fixture of
//! `docs/spikes/wp13-golden/golden.ts`, lone surrogates replaced by U+FFFD) and the native
//! chunker's output for them. Since WP-R7 (plan tn-review-baseline-remediation) the chunker is
//! native `str`, so the expectations are this crate's own output, regenerated with
//! `CV_REGEN_GOLDEN=1 cargo test -p cv-search --test golden golden_fixture_is_current` and
//! reviewed by diff.

use cv_search::chunker::{
    BuildChunksInput, ImageDescriptionChunkInput, LinkedDocumentChunkInput, build_search_chunks,
    hash_search_content, hash_string, parse_search_document, strip_frontmatter_block, take_chars,
};
use cv_search::prepare::{LINKED_DOCUMENT_MAX_CHARS, combined_description_hash};
use serde_json::{Value, json};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/golden.json");

fn st(v: &Value) -> String {
    v.as_str().unwrap().to_owned()
}

fn list(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(st).collect()
}

fn input_of(i: &Value) -> BuildChunksInput {
    BuildChunksInput {
        vault_id: st(&i["vaultId"]),
        path: st(&i["path"]),
        basename: st(&i["basename"]),
        mtime: i["mtime"].as_i64().unwrap(),
        content: st(&i["content"]),
        content_hash: None,
        max_chars: usize::try_from(i["maxChars"].as_u64().unwrap()).unwrap(),
        overlap_chars: usize::try_from(i["overlapChars"].as_u64().unwrap()).unwrap(),
        image_descriptions: i["imageDescriptions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| ImageDescriptionChunkInput {
                filename: st(&d["filename"]),
                narrative: st(&d["narrative"]),
                extraction: st(&d["extraction"]),
            })
            .collect(),
        linked_documents: i["linkedDocuments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| LinkedDocumentChunkInput {
                path: st(&d["path"]),
                title: st(&d["title"]),
                text: st(&d["text"]),
            })
            .collect(),
        extra_hash_facets: list(&i["extraHashFacets"]),
    }
}

/// The fixture with every expectation recomputed from its inputs.
fn regenerate(f: &Value) -> Value {
    let cases: Vec<Value> = f["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| {
            let input = input_of(&case["input"]);
            let (_, meta) = parse_search_document(&input.content, &input.basename);
            let chunks: Vec<Value> = build_search_chunks(&input)
                .iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "contentHash": c.content_hash,
                        "heading": c.heading,
                        "text": c.text,
                        "ordinal": c.ordinal,
                        "entities": c.entities.iter().map(|e| e.text.clone()).collect::<Vec<_>>(),
                    })
                })
                .collect();
            json!({
                "name": case["name"],
                "input": case["input"],
                "title": meta.title,
                "tags": meta.tags,
                "hash": hash_search_content(&input.content, &input.extra_hash_facets),
                "chunks": chunks,
            })
        })
        .collect();
    let hashes: Vec<Value> = f["hashes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| json!({"input": h["input"], "hash": hash_string(h["input"].as_str().unwrap())}))
        .collect();
    let raw = st(&f["linked"]["raw"]);
    let text = take_chars(
        strip_frontmatter_block(&raw).trim(),
        LINKED_DOCUMENT_MAX_CHARS,
    );
    let pairs: Vec<(String, String)> = f["combined"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .map(|l| {
            let (m, h) = l.as_str().unwrap().split_once(':').unwrap();
            (m.to_owned(), h.to_owned())
        })
        .collect();
    json!({
        "generator": f["generator"],
        "cases": cases,
        "hashes": hashes,
        "linked": {"raw": raw, "text": text, "facetHash": hash_search_content(text, &[])},
        "combined": {"lines": f["combined"]["lines"], "hash": combined_description_hash(&pairs)},
    })
}

#[test]
fn golden_fixture_is_current() {
    let f: Value = serde_json::from_str(&std::fs::read_to_string(FIXTURE).unwrap()).unwrap();
    assert!(f["cases"].as_array().unwrap().len() >= 20);
    let fresh = regenerate(&f);
    if std::env::var_os("CV_REGEN_GOLDEN").is_some() {
        let mut out = serde_json::to_string_pretty(&fresh).unwrap();
        out.push('\n');
        std::fs::write(FIXTURE, out).unwrap();
        return;
    }
    for (got, want) in fresh["cases"]
        .as_array()
        .unwrap()
        .iter()
        .zip(f["cases"].as_array().unwrap())
    {
        assert_eq!(got, want, "case {}", want["name"]);
    }
    assert_eq!(fresh, f);
    assert_eq!(combined_description_hash(&[]), "");
}

#[test]
fn astral_text_slices_on_char_boundaries() {
    // Hand spot-check: 500 emoji (4 UTF-8 bytes each) with max 400 chars, overlap 50.
    let content = "\u{1F600}".repeat(500);
    let chunks = build_search_chunks(&BuildChunksInput {
        path: "e.md".into(),
        content: content.clone(),
        max_chars: 400,
        overlap_chars: 50,
        ..BuildChunksInput::default()
    });
    let lens: Vec<usize> = chunks.iter().map(|c| c.text.chars().count()).collect();
    assert_eq!(lens, [400, 150]);
    assert!(
        chunks
            .iter()
            .all(|c| c.text.chars().all(|ch| ch == '\u{1F600}'))
    );
    // Mixed widths (1-4 bytes) never panic at any max/overlap.
    let mixed = "a\u{e9}\u{4e2d}\u{1F600}\u{301}".repeat(300);
    for (max, overlap) in [(400, 0), (401, 133), (999, 7)] {
        for c in build_search_chunks(&BuildChunksInput {
            content: mixed.clone(),
            max_chars: max,
            overlap_chars: overlap,
            ..BuildChunksInput::default()
        }) {
            assert!(c.text.chars().count() <= max);
        }
    }
}

#[test]
fn facet_order_does_not_move_the_hash() {
    let a = "linked:\u{E000}:1".to_owned();
    let b = "linked:\u{1F600}:1".to_owned();
    assert_eq!(
        hash_search_content("sorted facets", &[a.clone(), b.clone()]),
        hash_search_content("sorted facets", &[b, a])
    );
}

// ── Ported from https://github.com/dulrich/obsidian-crucible/blob/master/tests/searchChunker.test.mjs ────────────────────────────

const PROSE: &str =
    "---\ntitle: Quarterly Report\nauthor: Ada Lovelace\n---\n# Findings\nRevenue rose.";

fn chunk_input() -> BuildChunksInput {
    BuildChunksInput {
        vault_id: "test-vault".into(),
        path: "notes/report.md".into(),
        basename: "report".into(),
        mtime: 7,
        content: PROSE.into(),
        max_chars: 400,
        overlap_chars: 0,
        ..BuildChunksInput::default()
    }
}

fn img(f: &str, n: &str, e: &str) -> ImageDescriptionChunkInput {
    ImageDescriptionChunkInput {
        filename: f.into(),
        narrative: n.into(),
        extraction: e.into(),
    }
}

#[test]
fn indexable_extensions() {
    use cv_search::is_search_indexable_path as ok;
    assert!(ok("daily/note.md") && ok("research/report.qmd") && ok("inbox/raw.txt"));
    assert!(!ok("assets/image.png") && !ok("dir/.md") && !ok("dir/x."));
    assert!(ok("UP/CASE.MD"));
}

#[test]
fn frontmatter_metadata_and_heading_fallback() {
    let c = String::from(
        "---\ntitle: Example Search Title\nsource: https://example.com/source\ntags:\n  - search\n  - agents\n---\n# Ignored fallback\nBody text",
    );
    let (_, m) = parse_search_document(&c, "fallback");
    assert_eq!(m.title, "Example Search Title");
    assert_eq!(m.source.as_deref().unwrap(), "https://example.com/source");
    assert_eq!(m.tags, ["search", "agents"]);
}

#[test]
fn chunks_are_stable_and_retain_headings() {
    let content = format!(
        "---\ntitle: Example\n---\n# First\n{}\n\n## Second\n{}",
        "Alpha ".repeat(20),
        "Beta ".repeat(20)
    );
    let input = BuildChunksInput {
        vault_id: "test".into(),
        path: "daily/example.md".into(),
        basename: "example".into(),
        mtime: 1,
        content: content.clone(),
        max_chars: 80,
        overlap_chars: 10,
        ..BuildChunksInput::default()
    };
    let first = build_search_chunks(&input);
    let second = build_search_chunks(&input);
    assert!(first.len() > 1);
    assert_eq!(
        first.iter().map(|c| &c.id).collect::<Vec<_>>(),
        second.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
    assert!(first.iter().any(|c| c.heading == "First"));
    assert!(first.iter().any(|c| c.heading == "Second"));
    assert_eq!(first[0].vault_id, "test");
    assert_eq!(first[0].content_hash, hash_search_content(&content, &[]));
    assert_ne!(
        first[0].content_hash,
        hash_search_content(&format!("{content}\nChanged"), &[])
    );
}

#[test]
fn no_images_is_byte_identical_to_pre_image_chunker() {
    let p = PROSE;
    assert_eq!(hash_search_content(p, &[]), "f84cbaae");
    let chunks = build_search_chunks(&chunk_input());
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].id, "notes/report.md#0:347085ed");
    assert_eq!(chunks[0].content_hash, "f84cbaae");
    assert_eq!(chunks[0].ordinal, 0);
    assert_eq!(chunks[0].heading, "Findings");
    assert_eq!(
        hash_search_content(p, &["".to_owned(), "   ".to_owned()]),
        "f84cbaae"
    );
}

#[test]
fn described_image_appends_two_chunks() {
    let mut input = chunk_input();
    input.image_descriptions = vec![img(
        "a1b2c3d4e5f60718293a4b5c6d7e8f90_MD5.png",
        "A line chart of quarterly revenue climbing through Q4.",
        "Title: Quarterly revenue\nQ1 1.2M\nQ2 1.4M\nQ3 1.9M\nQ4 2.6M",
    )];
    let chunks = build_search_chunks(&input);
    assert_eq!(chunks.len(), 3);
    assert_eq!(
        chunks.iter().map(|c| c.ordinal).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(
        chunks[1].heading,
        "Image: a1b2c3d4e5f60718293a4b5c6d7e8f90_MD5.png"
    );
    assert_eq!(
        chunks[2].heading,
        "Image: a1b2c3d4e5f60718293a4b5c6d7e8f90_MD5.png (text)"
    );
    assert!(chunks[2].text.contains("Q4 2.6M"));
    for c in &chunks {
        assert_eq!(c.path, "notes/report.md");
        assert_eq!(c.title, "Quarterly Report");
    }
    assert_eq!(chunks[0].id, "notes/report.md#0:347085ed");
}

#[test]
fn svg_record_emits_one_chunk_and_blanks_nothing() {
    let mut input = chunk_input();
    input.image_descriptions = vec![img(
        "diagram_MD5.svg",
        "",
        "Pipeline\nintake -> enrich -> index",
    )];
    let chunks = build_search_chunks(&input);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[1].heading, "Image: diagram_MD5.svg (text)");
    input.image_descriptions = vec![
        img("blank_MD5.png", "   ", ""),
        img("  ", "orphaned", "orphaned"),
    ];
    assert_eq!(build_search_chunks(&input).len(), 1);
}

#[test]
fn long_extraction_packs_by_max_chars() {
    let mut input = chunk_input();
    let row = "Region North | Units 1284 | Revenue 91234 | Margin 0.42\n\n";
    input.image_descriptions = vec![img(
        "table_MD5.webp",
        "A wide sales table.",
        &row.repeat(40),
    )];
    let chunks = build_search_chunks(&input);
    let packed: Vec<_> = chunks
        .iter()
        .filter(|c| c.heading == "Image: table_MD5.webp (text)")
        .collect();
    assert!(packed.len() > 3);
    assert!(packed.iter().all(|c| c.text.len() <= 400));
    let ids: std::collections::HashSet<_> = packed.iter().map(|c| &c.id).collect();
    assert_eq!(ids.len(), packed.len());
    assert!(chunks.iter().enumerate().all(|(i, c)| c.ordinal == i));
}

#[test]
fn extra_facet_fold_is_order_independent_and_deduped() {
    let p = PROSE;
    let with = hash_search_content(p, &["image-desc:deadbeef".to_owned()]);
    assert_ne!(with, hash_search_content(p, &[]));
    assert_ne!(
        with,
        hash_search_content(p, &["image-desc:cafebabe".to_owned()])
    );
    assert_eq!(
        hash_search_content(p, &["image-desc:deadbeef".to_owned(), "other:1".to_owned()]),
        hash_search_content(
            p,
            &[
                "other:1".to_owned(),
                "image-desc:deadbeef".to_owned(),
                "other:1".to_owned()
            ]
        )
    );
    let mut input = chunk_input();
    input.extra_hash_facets = vec!["image-desc:deadbeef".to_owned()];
    assert_eq!(build_search_chunks(&input)[0].content_hash, with);
}
