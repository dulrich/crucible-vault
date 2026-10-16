//! Byte-compatibility against Crucible's own TypeScript chunker.
//!
//! `fixtures/golden.json` was produced by running `https://github.com/dulrich/obsidian-crucible/blob/master/src/search/chunker.ts`
//! on synthetic inputs (`docs/spikes/wp13-golden/golden.ts`, see the report for the command).
//! Strings are UTF-16 code-unit arrays so lone surrogates (E4) survive. Plus the vectors of
//! Crucible's `tests/searchChunker.test.mjs`, ported.

use cv_search::chunker::{
    BuildChunksInput, ImageDescriptionChunkInput, LinkedDocumentChunkInput, build_search_chunks,
    hash_search_content, hash_string, parse_search_document, strip_frontmatter_block,
};
use cv_search::js::{JsStr, js, lossy, trim};
use cv_search::prepare::{LINKED_DOCUMENT_MAX_CHARS, combined_description_hash};
use serde_json::Value;

fn units(v: &Value) -> JsStr {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| u16::try_from(x.as_u64().unwrap()).unwrap())
        .collect()
}

fn list(v: &Value) -> Vec<JsStr> {
    v.as_array().unwrap().iter().map(units).collect()
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/golden.json")).unwrap()
}

fn input_of(i: &Value) -> BuildChunksInput {
    BuildChunksInput {
        vault_id: i["vaultId"].as_str().unwrap().into(),
        path: i["path"].as_str().unwrap().into(),
        basename: i["basename"].as_str().unwrap().into(),
        mtime: i["mtime"].as_i64().unwrap(),
        content: units(&i["content"]),
        content_hash: None,
        max_chars: usize::try_from(i["maxChars"].as_u64().unwrap()).unwrap(),
        overlap_chars: usize::try_from(i["overlapChars"].as_u64().unwrap()).unwrap(),
        image_descriptions: i["imageDescriptions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| ImageDescriptionChunkInput {
                filename: units(&d["filename"]),
                narrative: units(&d["narrative"]),
                extraction: units(&d["extraction"]),
            })
            .collect(),
        linked_documents: i["linkedDocuments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| LinkedDocumentChunkInput {
                path: units(&d["path"]),
                title: units(&d["title"]),
                text: units(&d["text"]),
            })
            .collect(),
        extra_hash_facets: list(&i["extraHashFacets"]),
    }
}

#[test]
fn golden_cases_match_crucible_exactly() {
    let f = fixture();
    let cases = f["cases"].as_array().unwrap();
    assert!(cases.len() >= 20);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let input = input_of(&case["input"]);
        assert_eq!(
            hash_search_content(&input.content, &input.extra_hash_facets),
            case["hash"].as_str().unwrap(),
            "{name}: hash"
        );
        let (_, meta) = parse_search_document(&input.content, &input.basename);
        assert_eq!(meta.title, units(&case["title"]), "{name}: title");
        assert_eq!(meta.tags, list(&case["tags"]), "{name}: tags");
        let chunks = build_search_chunks(&input);
        let want = case["chunks"].as_array().unwrap();
        assert_eq!(chunks.len(), want.len(), "{name}: chunk count");
        for (c, w) in chunks.iter().zip(want) {
            assert_eq!(c.id, w["id"].as_str().unwrap(), "{name}: id");
            assert_eq!(c.content_hash, w["contentHash"].as_str().unwrap(), "{name}");
            assert_eq!(c.heading, units(&w["heading"]), "{name}: heading");
            assert_eq!(c.text, units(&w["text"]), "{name}: text #{}", c.ordinal);
            assert_eq!(c.ordinal as u64, w["ordinal"].as_u64().unwrap(), "{name}");
            let ents: Vec<JsStr> = c.entities.iter().map(|e| e.text.clone()).collect();
            assert_eq!(ents, list(&w["entities"]), "{name}: entities");
            // The payload serializes (lone surrogates escaped) and stays valid JSON.
            let json = c.to_json();
            let back: Value = serde_json::from_str(&json)
                .or_else(|_| serde_json::from_str(&json.replace("\\ud", "\\u00")))
                .unwrap();
            assert_eq!(back["id"], w["id"]);
        }
    }
}

#[test]
fn golden_covers_lone_surrogates_at_chunk_boundaries() {
    let f = fixture();
    let case = f["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "surrogate-split")
        .unwrap()
        .clone();
    let chunks = build_search_chunks(&input_of(&case["input"]));
    // The TS slices code units: the first slice ends on a lone high surrogate.
    let last = *chunks[0].text.last().unwrap();
    assert!((0xD800..0xDC00).contains(&last));
    assert!(chunks[0].to_json().contains("\\ud83d\""));
}

#[test]
fn golden_hash_string_and_facet_helpers() {
    let f = fixture();
    for h in f["hashes"].as_array().unwrap() {
        assert_eq!(
            hash_string(&units(&h["input"])),
            h["hash"].as_str().unwrap()
        );
    }
    // Linked-document text: stripFrontmatterBlock + trim + slice(0, 4000) — ends mid-pair.
    let raw = units(&f["linked"]["raw"]);
    let text = trim(strip_frontmatter_block(&raw));
    let text = &text[..text.len().min(LINKED_DOCUMENT_MAX_CHARS)];
    assert_eq!(text, &units(&f["linked"]["text"])[..]);
    assert_eq!(
        hash_search_content(text, &[]),
        f["linked"]["facetHash"].as_str().unwrap()
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
    assert_eq!(
        combined_description_hash(&pairs),
        f["combined"]["hash"].as_str().unwrap()
    );
    assert_eq!(combined_description_hash(&[]), "");
}

#[test]
fn facet_sort_is_code_unit_order() {
    // U+1F600 is D83D DE00 in UTF-16, which sorts before U+E000 by code unit but after it by
    // code point. Rust `str` order is code-point order, so it must not be used.
    let a = js("linked:\u{E000}:1");
    let b = js("linked:\u{1F600}:1");
    assert!(b < a);
    assert!("linked:\u{1F600}:1" > "linked:\u{E000}:1");
    let content = js("sorted facets");
    assert_eq!(
        hash_search_content(&content, &[a.clone(), b.clone()]),
        hash_search_content(&content, &[b, a])
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
        content: js(PROSE),
        max_chars: 400,
        overlap_chars: 0,
        ..BuildChunksInput::default()
    }
}

fn img(f: &str, n: &str, e: &str) -> ImageDescriptionChunkInput {
    ImageDescriptionChunkInput {
        filename: js(f),
        narrative: js(n),
        extraction: js(e),
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
    let c = js(
        "---\ntitle: Example Search Title\nsource: https://example.com/source\ntags:\n  - search\n  - agents\n---\n# Ignored fallback\nBody text",
    );
    let (_, m) = parse_search_document(&c, "fallback");
    assert_eq!(lossy(&m.title), "Example Search Title");
    assert_eq!(
        lossy(m.source.as_deref().unwrap()),
        "https://example.com/source"
    );
    assert_eq!(
        m.tags.iter().map(|t| lossy(t)).collect::<Vec<_>>(),
        ["search", "agents"]
    );
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
        content: js(&content),
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
    assert!(first.iter().any(|c| lossy(&c.heading) == "First"));
    assert!(first.iter().any(|c| lossy(&c.heading) == "Second"));
    assert_eq!(first[0].vault_id, "test");
    assert_eq!(
        first[0].content_hash,
        hash_search_content(&js(&content), &[])
    );
    assert_ne!(
        first[0].content_hash,
        hash_search_content(&js(&format!("{content}\nChanged")), &[])
    );
}

#[test]
fn no_images_is_byte_identical_to_pre_image_chunker() {
    let p = js(PROSE);
    assert_eq!(hash_search_content(&p, &[]), "f84cbaae");
    let chunks = build_search_chunks(&chunk_input());
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].id, "notes/report.md#0:347085ed");
    assert_eq!(chunks[0].content_hash, "f84cbaae");
    assert_eq!(chunks[0].ordinal, 0);
    assert_eq!(lossy(&chunks[0].heading), "Findings");
    assert_eq!(hash_search_content(&p, &[js(""), js("   ")]), "f84cbaae");
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
        lossy(&chunks[1].heading),
        "Image: a1b2c3d4e5f60718293a4b5c6d7e8f90_MD5.png"
    );
    assert_eq!(
        lossy(&chunks[2].heading),
        "Image: a1b2c3d4e5f60718293a4b5c6d7e8f90_MD5.png (text)"
    );
    assert!(lossy(&chunks[2].text).contains("Q4 2.6M"));
    for c in &chunks {
        assert_eq!(c.path, "notes/report.md");
        assert_eq!(lossy(&c.title), "Quarterly Report");
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
    assert_eq!(lossy(&chunks[1].heading), "Image: diagram_MD5.svg (text)");
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
        .filter(|c| lossy(&c.heading) == "Image: table_MD5.webp (text)")
        .collect();
    assert!(packed.len() > 3);
    assert!(packed.iter().all(|c| c.text.len() <= 400));
    let ids: std::collections::HashSet<_> = packed.iter().map(|c| &c.id).collect();
    assert_eq!(ids.len(), packed.len());
    assert!(chunks.iter().enumerate().all(|(i, c)| c.ordinal == i));
}

#[test]
fn extra_facet_fold_is_order_independent_and_deduped() {
    let p = js(PROSE);
    let with = hash_search_content(&p, &[js("image-desc:deadbeef")]);
    assert_ne!(with, hash_search_content(&p, &[]));
    assert_ne!(with, hash_search_content(&p, &[js("image-desc:cafebabe")]));
    assert_eq!(
        hash_search_content(&p, &[js("image-desc:deadbeef"), js("other:1")]),
        hash_search_content(
            &p,
            &[js("other:1"), js("image-desc:deadbeef"), js("other:1")]
        )
    );
    let mut input = chunk_input();
    input.extra_hash_facets = vec![js("image-desc:deadbeef")];
    assert_eq!(build_search_chunks(&input)[0].content_hash, with);
}
