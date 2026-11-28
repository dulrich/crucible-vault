//! Compatibility baseline (plan `intake-foundations` WP-2): old match results against the new
//! match keys over a fixed synthetic corpus, equal except for [`ALLOWLIST`].
//!
//! The old derivation is kept here as a **test-only copy** (`old_*`): the pre-WP-2
//! `identity_keys` / `identity_keys_in`, run over `old_canonical` (today's `canonicalize_url`
//! with the pre-WP-2 arXiv rule: a trailing slash defeated the rewrite), plus the two site
//! rules that did not use them — `dupes-scan` (equality of the configured post-id of `source`,
//! else the stored `post-id`) and `intake_finalize` (exact row URL against the note's values and
//! their canonical forms).

use std::collections::BTreeMap;

use cv_text::url_canon::CanonMethod;

use super::*;

// ---- the old derivation (test-only copy) -------------------------------------------------------

/// Pre-WP-2 canonical form: an arXiv URL whose path ends in `/` was not rewritten.
fn old_canonical(raw: &str) -> Option<String> {
    let t = raw.trim();
    for h in ["https://arxiv.org/", "https://www.arxiv.org/"] {
        if let Some(rest) = t.strip_prefix(h) {
            let path = rest.split(['?', '#']).next().unwrap_or(rest);
            if path.ends_with('/') {
                let c = canonicalize_url(&format!("https://old-arxiv.invalid/{rest}"))?;
                return Some(
                    c.canonical
                        .replacen("old-arxiv.invalid", &h[8..h.len() - 1], 1),
                );
            }
        }
    }
    canonicalize_url(t).map(|c| c.canonical)
}

fn old_identity_keys(url: &str) -> Vec<String> {
    let raw = url.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let Some(c) = old_canonical(raw) else {
        out.push(raw.to_owned());
        return out;
    };
    let mut forms = vec![c.clone()];
    if let Some(m) = open_substack(&c) {
        forms.push(m);
    }
    let opts = PostIdOptions::default();
    for f in forms {
        out.push(post_id_from_url(&f, &opts));
        out.push(f);
    }
    out.push(post_id_from_url(raw, &opts));
    out.sort();
    out.dedup();
    out
}

fn old_identity_keys_in(url: &str, pubs: &PublicationHosts) -> Vec<String> {
    let mut out = old_identity_keys(url);
    if pubs.is_empty() {
        return out;
    }
    let Some(c) = old_canonical(url.trim()) else {
        return out;
    };
    let mut forms = vec![c.clone()];
    if let Some(m) = open_substack(&c) {
        forms.push(m);
    }
    for f in forms {
        if let (Some(h), Some(p)) = (http_host(&f), canonical_path(&f))
            && let Some(name) = pubs.publication_of(&h)
        {
            out.push(format!("{PUBLICATION_KEY_PREFIX}{name}:{p}"));
        }
    }
    out.sort();
    out.dedup();
    out
}

fn old_supersede(a: &str, b: &str, pubs: &PublicationHosts) -> bool {
    let kb = old_identity_keys_in(b, pubs);
    old_identity_keys_in(a, pubs).iter().any(|k| kb.contains(k))
}

/// Old `dupes-scan` identity of a note with `source` = `v` (a URL) or `post-id` = `v`.
fn old_dupes_id(v: &str, opts: &PostIdOptions) -> Option<String> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    if v.contains("://") {
        Some(post_id_from_url(v, opts)).filter(|s| !s.is_empty())
    } else {
        Some(v.to_owned())
    }
}

/// Old `intake_finalize` URL rule: the row's stored `url` equals a note value or its canonical.
fn old_finalize(note: &str, row_url: &str) -> bool {
    row_url == note || old_canonical(note).as_deref() == Some(row_url)
}

// ---- corpus ------------------------------------------------------------------------------------

/// Synthetic values: note `source` / `post-id` frontmatter shapes, feed links and GUIDs
/// (URL-shaped and plain), URL-derived tracker ids, Substack subdomain / open / custom-domain
/// forms, YouTube and X forms, arXiv forms. Invented hosts and ids only.
const CORPUS: &[&str] = &[
    "https://alpha.substack.com/p/first-post",
    "https://alpha.substack.com/p/first-post/?utm_source=substack&utm_medium=email",
    "https://open.substack.com/pub/alpha/p/first-post?r=abc",
    "https://www.alpha-custom.example/p/first-post",
    "https://alpha-custom.example/p/first-post?utm_campaign=x",
    "https://beta.substack.com/p/first-post",
    "https://blog.example.org/2025/01/02/a-title/",
    "https://blog.example.org/2025/01/02/a-title",
    "https://blog.example.org/?p=123",
    "https://blog.example.org/?p=123&utm_source=rss",
    "tag:blog.example.org,2025:post-123",
    "post-123",
    "first-post",
    "https://www.youtube.com/watch?v=AbCdEfGhIjK",
    "https://youtu.be/AbCdEfGhIjK?t=30",
    "https://m.youtube.com/shorts/AbCdEfGhIjK",
    "https://twitter.com/someone/status/1234567890123?s=20",
    "https://x.com/someone/status/1234567890123",
    "https://arxiv.org/abs/2301.12345",
    "https://arxiv.org/pdf/2301.12345/",
    "https://arxiv.org/pdf/2301.12345.pdf",
    "https://forum.example.net/threads/topic.42/page-3#post-9876",
    "https://forum.example.net/posts/9876/",
];

fn pubs() -> PublicationHosts {
    let mut p = PublicationHosts::default();
    p.insert("alpha.substack.com", "Alpha");
    p.insert("www.alpha-custom.example", "Alpha");
    p.insert("beta.substack.com", "Beta");
    p
}

fn configured() -> PostIdOptions {
    let mut host_rules = BTreeMap::new();
    host_rules.insert("blog.example.org".to_owned(), CanonMethod::KeepParams);
    PostIdOptions {
        method: None,
        host_rules,
    }
}

/// `(site, a, b, reason)`: the only pairs whose match result changed. Every entry is named.
const ALLOWLIST: &[(&str, &str, &str, &str)] = &[
    // arXiv trailing slash: canonicalize_url now trims it before the pdf → abs rewrite.
    (
        "supersede",
        "https://arxiv.org/abs/2301.12345",
        "https://arxiv.org/pdf/2301.12345/",
        "arXiv trailing slash",
    ),
    (
        "supersede",
        "https://arxiv.org/pdf/2301.12345/",
        "https://arxiv.org/pdf/2301.12345.pdf",
        "arXiv trailing slash",
    ),
    (
        "dupes",
        "https://arxiv.org/abs/2301.12345",
        "https://arxiv.org/pdf/2301.12345/",
        "arXiv trailing slash",
    ),
    (
        "dupes",
        "https://arxiv.org/pdf/2301.12345/",
        "https://arxiv.org/pdf/2301.12345.pdf",
        "arXiv trailing slash",
    ),
    // The rest are widenings of the two site rules that did not use the shared keys: `dupes-scan`
    // compared the raw post-id (no canonical form, no Substack/publication alias) and
    // `intake_finalize` the stored row URL by exact string. No match is lost (asserted).
    (
        "dupes",
        "https://alpha-custom.example/p/first-post?utm_campaign=x",
        "https://alpha.substack.com/p/first-post",
        "Substack/publication alias",
    ),
    (
        "dupes",
        "https://alpha-custom.example/p/first-post?utm_campaign=x",
        "https://alpha.substack.com/p/first-post/?utm_source=substack&utm_medium=email",
        "Substack/publication alias",
    ),
    (
        "dupes",
        "https://alpha-custom.example/p/first-post?utm_campaign=x",
        "https://open.substack.com/pub/alpha/p/first-post?r=abc",
        "Substack/publication alias",
    ),
    (
        "dupes",
        "https://alpha-custom.example/p/first-post?utm_campaign=x",
        "https://www.alpha-custom.example/p/first-post",
        "Substack/publication alias",
    ),
    (
        "dupes",
        "https://alpha.substack.com/p/first-post",
        "https://open.substack.com/pub/alpha/p/first-post?r=abc",
        "Substack/publication alias",
    ),
    (
        "dupes",
        "https://alpha.substack.com/p/first-post",
        "https://www.alpha-custom.example/p/first-post",
        "Substack/publication alias",
    ),
    (
        "dupes",
        "https://alpha.substack.com/p/first-post/?utm_source=substack&utm_medium=email",
        "https://open.substack.com/pub/alpha/p/first-post?r=abc",
        "Substack/publication alias",
    ),
    (
        "dupes",
        "https://alpha.substack.com/p/first-post/?utm_source=substack&utm_medium=email",
        "https://www.alpha-custom.example/p/first-post",
        "Substack/publication alias",
    ),
    (
        "dupes",
        "https://arxiv.org/abs/2301.12345",
        "https://arxiv.org/pdf/2301.12345.pdf",
        "canonical form",
    ),
    (
        "dupes",
        "https://blog.example.org/?p=123",
        "https://blog.example.org/?p=123&utm_source=rss",
        "canonical form",
    ),
    (
        "dupes",
        "https://m.youtube.com/shorts/AbCdEfGhIjK",
        "https://www.youtube.com/watch?v=AbCdEfGhIjK",
        "canonical form",
    ),
    (
        "dupes",
        "https://m.youtube.com/shorts/AbCdEfGhIjK",
        "https://youtu.be/AbCdEfGhIjK?t=30",
        "canonical form",
    ),
    (
        "dupes",
        "https://open.substack.com/pub/alpha/p/first-post?r=abc",
        "https://www.alpha-custom.example/p/first-post",
        "Substack/publication alias",
    ),
    (
        "dupes",
        "https://twitter.com/someone/status/1234567890123?s=20",
        "https://x.com/someone/status/1234567890123",
        "canonical form",
    ),
    (
        "dupes",
        "https://www.youtube.com/watch?v=AbCdEfGhIjK",
        "https://youtu.be/AbCdEfGhIjK?t=30",
        "canonical form",
    ),
    (
        "finalize",
        "https://alpha-custom.example/p/first-post?utm_campaign=x",
        "https://alpha.substack.com/p/first-post",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://alpha-custom.example/p/first-post?utm_campaign=x",
        "https://alpha.substack.com/p/first-post/?utm_source=substack&utm_medium=email",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://alpha-custom.example/p/first-post?utm_campaign=x",
        "https://open.substack.com/pub/alpha/p/first-post?r=abc",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://alpha-custom.example/p/first-post?utm_campaign=x",
        "https://www.alpha-custom.example/p/first-post",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://alpha.substack.com/p/first-post",
        "https://alpha-custom.example/p/first-post?utm_campaign=x",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://alpha.substack.com/p/first-post",
        "https://alpha.substack.com/p/first-post/?utm_source=substack&utm_medium=email",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://alpha.substack.com/p/first-post",
        "https://open.substack.com/pub/alpha/p/first-post?r=abc",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://alpha.substack.com/p/first-post",
        "https://www.alpha-custom.example/p/first-post",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://alpha.substack.com/p/first-post/?utm_source=substack&utm_medium=email",
        "https://alpha-custom.example/p/first-post?utm_campaign=x",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://alpha.substack.com/p/first-post/?utm_source=substack&utm_medium=email",
        "https://open.substack.com/pub/alpha/p/first-post?r=abc",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://alpha.substack.com/p/first-post/?utm_source=substack&utm_medium=email",
        "https://www.alpha-custom.example/p/first-post",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://arxiv.org/abs/2301.12345",
        "https://arxiv.org/pdf/2301.12345.pdf",
        "canonical form",
    ),
    (
        "finalize",
        "https://arxiv.org/abs/2301.12345",
        "https://arxiv.org/pdf/2301.12345/",
        "arXiv trailing slash",
    ),
    (
        "finalize",
        "https://arxiv.org/pdf/2301.12345.pdf",
        "https://arxiv.org/pdf/2301.12345/",
        "arXiv trailing slash",
    ),
    (
        "finalize",
        "https://arxiv.org/pdf/2301.12345/",
        "https://arxiv.org/abs/2301.12345",
        "arXiv trailing slash",
    ),
    (
        "finalize",
        "https://arxiv.org/pdf/2301.12345/",
        "https://arxiv.org/pdf/2301.12345.pdf",
        "arXiv trailing slash",
    ),
    (
        "finalize",
        "https://blog.example.org/2025/01/02/a-title",
        "https://blog.example.org/2025/01/02/a-title/",
        "canonical form",
    ),
    (
        "finalize",
        "https://blog.example.org/?p=123",
        "https://blog.example.org/?p=123&utm_source=rss",
        "canonical form",
    ),
    (
        "finalize",
        "https://forum.example.net/posts/9876/",
        "https://forum.example.net/threads/topic.42/page-3#post-9876",
        "canonical form",
    ),
    (
        "finalize",
        "https://forum.example.net/threads/topic.42/page-3#post-9876",
        "https://forum.example.net/posts/9876/",
        "canonical form",
    ),
    (
        "finalize",
        "https://m.youtube.com/shorts/AbCdEfGhIjK",
        "https://youtu.be/AbCdEfGhIjK?t=30",
        "canonical form",
    ),
    (
        "finalize",
        "https://open.substack.com/pub/alpha/p/first-post?r=abc",
        "https://alpha-custom.example/p/first-post?utm_campaign=x",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://open.substack.com/pub/alpha/p/first-post?r=abc",
        "https://alpha.substack.com/p/first-post",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://open.substack.com/pub/alpha/p/first-post?r=abc",
        "https://alpha.substack.com/p/first-post/?utm_source=substack&utm_medium=email",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://open.substack.com/pub/alpha/p/first-post?r=abc",
        "https://www.alpha-custom.example/p/first-post",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://www.alpha-custom.example/p/first-post",
        "https://alpha-custom.example/p/first-post?utm_campaign=x",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://www.alpha-custom.example/p/first-post",
        "https://alpha.substack.com/p/first-post",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://www.alpha-custom.example/p/first-post",
        "https://alpha.substack.com/p/first-post/?utm_source=substack&utm_medium=email",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://www.alpha-custom.example/p/first-post",
        "https://open.substack.com/pub/alpha/p/first-post?r=abc",
        "Substack/publication alias",
    ),
    (
        "finalize",
        "https://www.youtube.com/watch?v=AbCdEfGhIjK",
        "https://m.youtube.com/shorts/AbCdEfGhIjK",
        "canonical form",
    ),
    (
        "finalize",
        "https://www.youtube.com/watch?v=AbCdEfGhIjK",
        "https://youtu.be/AbCdEfGhIjK?t=30",
        "canonical form",
    ),
    (
        "finalize",
        "https://x.com/someone/status/1234567890123",
        "https://twitter.com/someone/status/1234567890123?s=20",
        "canonical form",
    ),
    (
        "finalize",
        "https://youtu.be/AbCdEfGhIjK?t=30",
        "https://m.youtube.com/shorts/AbCdEfGhIjK",
        "canonical form",
    ),
];

fn sorted(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_owned(), b.to_owned())
    } else {
        (b.to_owned(), a.to_owned())
    }
}

fn keys(v: &str, opts: Option<&PostIdOptions>) -> MatchKeys {
    note_keys(
        &NoteFacts {
            sources: &[v.to_owned()],
            ..NoteFacts::default()
        },
        &pubs(),
        opts,
    )
}

#[test]
fn compatibility_baseline_equal_outside_the_allowlist() {
    let p = pubs();
    let opts = configured();
    let mut diffs = BTreeMap::new();
    for (i, a) in CORPUS.iter().enumerate() {
        for b in &CORPUS[i + 1..] {
            // Supersede engine, reconcile phases A and E (note ↔ note / incoming URL).
            let new = intersects(&keys(a, None), &keys(b, None));
            if old_supersede(a, b, &p) != new {
                diffs.insert(("supersede", sorted(a, b)), new);
            }
            // dupes-scan (configured post-id options; the stored id only without a source).
            let old = old_dupes_id(a, &opts).is_some()
                && old_dupes_id(a, &opts) == old_dupes_id(b, &opts);
            let new = intersects(&keys(a, Some(&opts)), &keys(b, Some(&opts)));
            if old != new {
                diffs.insert(("dupes", sorted(a, b)), new);
            }
        }
    }
    // intake_finalize: note value ↔ stored row url (blog rows, any direction).
    for note in CORPUS {
        for row in CORPUS.iter().filter(|r| r.contains("://")) {
            let new = intersects(&keys(note, None), &row_keys("blog", row, "x", &p));
            if old_finalize(note, row) != new {
                assert!(new, "finalize lost a match: {note} / {row}");
                diffs.insert(("finalize", (note.to_string(), row.to_string())), new);
            }
        }
    }
    let allowed: Vec<_> = ALLOWLIST
        .iter()
        .map(|(s, a, b, _)| {
            let pair = if *s == "finalize" {
                ((*a).to_owned(), (*b).to_owned())
            } else {
                sorted(a, b)
            };
            (*s, pair)
        })
        .collect();
    assert!(
        diffs.values().all(|now| *now),
        "a match was lost: {diffs:?}"
    );
    let unexpected: Vec<_> = diffs
        .iter()
        .filter(|((s, pair), _)| !allowed.contains(&(*s, pair.clone())))
        .map(|((s, (a, b)), new)| format!("{s} | {a} | {b} | now {new}"))
        .collect();
    assert!(
        unexpected.is_empty(),
        "unallowlisted changes:\n{}",
        unexpected.join("\n")
    );
    for a in &allowed {
        assert!(
            diffs.contains_key(a),
            "allowlist entry no longer differs: {a:?}"
        );
    }
}

#[test]
fn finalize_row_side_widenings_are_the_named_rules() {
    let p = pubs();
    let note = "https://alpha.substack.com/p/first-post";
    for row in [
        "https://alpha.substack.com/p/first-post/?utm_source=substack&utm_medium=email",
        "https://open.substack.com/pub/alpha/p/first-post?r=abc",
        "https://alpha-custom.example/p/first-post?utm_campaign=x",
    ] {
        assert!(!old_finalize(note, row));
        assert!(intersects(
            &keys(note, None),
            &row_keys("blog", row, "x", &p)
        ));
    }
    // A different publication's same slug stays distinct.
    assert!(!intersects(
        &keys(note, None),
        &row_keys("blog", "https://beta.substack.com/p/first-post", "x", &p)
    ));
}

#[test]
fn canonicalize_url_is_idempotent_over_the_corpus() {
    for u in CORPUS {
        if let Some(c) = canonicalize_url(u) {
            let again = canonicalize_url(&c.canonical).expect("canonical parses");
            assert_eq!(again.canonical, c.canonical, "{u}");
        }
    }
}

#[test]
fn item_type_key_forms() {
    let p = pubs();
    // A YouTube row keys by its video id as well as its URL.
    let row = row_keys("youtube", "", "AbCdEfGhIjK", &p);
    assert!(intersects(
        &row,
        &keys("https://youtu.be/AbCdEfGhIjK", None)
    ));
    // A plain GUID is not a match key; a URL-shaped one is.
    let f = feed_item_keys("https://blog.example.org/?p=123", Some("post-123"), &p);
    assert!(!f.contains("post-123"));
    let g = feed_item_keys(
        "https://b.example/x",
        Some("https://blog.example.org/?p=123"),
        &p,
    );
    assert!(intersects(
        &g,
        &keys("https://blog.example.org/?p=123&utm_source=rss", None)
    ));
    // Mail: Message-ID and post URL.
    let m = mail_keys(Some("<abc@mail.example>"), Some(CORPUS[2]), &p);
    assert!(m.contains("cv-message:abc@mail.example"));
    assert!(intersects(&m, &keys(CORPUS[0], None)));
    let n = note_keys(
        &NoteFacts {
            message_ids: &["abc@mail.example".to_owned()],
            ..NoteFacts::default()
        },
        &p,
        None,
    );
    assert!(intersects(&m, &n));
    // The probe is a substring of every stored form that can share a key.
    assert_eq!(url_probe(CORPUS[2]).as_deref(), Some("first-post"));
    assert_eq!(url_probe(CORPUS[14]).as_deref(), Some("AbCdEfGhIjK"));
    assert_eq!(url_probe(CORPUS[19]).as_deref(), Some("2301.12345"));
    assert_eq!(url_probe(CORPUS[16]).as_deref(), Some("1234567890123"));
}
