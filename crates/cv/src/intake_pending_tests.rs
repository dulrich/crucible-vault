use std::collections::BTreeMap;
use std::path::Path;

use cv_state::{QueueConfig, StageOutcomeRow, StateDb};

use super::*;

/// A synthetic vault: an enriched blog post, a mail note with an enrich miss, an unfiled clip.
fn vault(root: &Path) -> Sources {
    let v = root.join("vault");
    let w = |p: &str, s: &str| {
        let f = v.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, s).unwrap();
    };
    w(
        "Blog/a.md",
        "---\npost-id: https://blog.example/a\nenriched: 2026-11-01\n---\nA post body.\n",
    );
    w(
        "Mail/m.md",
        "---\nmessage-id: <m@example>\nenrich-miss: 1\n---\nA mail body.\n",
    );
    w(
        "Clippings/c.md",
        "---\nsource: https://example.org/x\n---\n![](https://img.example/p.png)\n",
    );
    w("Notes/plain.md", "no frontmatter\n");
    drop(cv_index::Index::create(&v, &root.join("index.db")).unwrap());
    Sources {
        vault: v,
        index_db: root.join("index.db"),
        state_db: root.join("state.db"),
        search_db: root.join("search.db"),
    }
}

fn snapshot(root: &Path) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(root).unwrap() {
        let p = e.unwrap().path();
        if p.is_file() {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let digest = std::fs::read(&p)
                .unwrap()
                .iter()
                .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
                    (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
                });
            out.insert(name, digest);
        }
    }
    out
}

/// The stage row of `stage` in the rendered table: the six counts.
/// Every file of `before` is byte-identical in `after`; a file new in `after` can only be an SQLite
/// `-shm`/`-wal` sidecar a read-only WAL open creates (the database bytes never change).
fn assert_unchanged(before: &BTreeMap<String, u64>, after: &BTreeMap<String, u64>) {
    for (k, v) in before {
        assert_eq!(after.get(k), Some(v), "{k} changed");
    }
    for k in after.keys().filter(|k| !before.contains_key(*k)) {
        assert!(k.ends_with("-shm") || k.ends_with("-wal"), "{k} created");
    }
}

fn row(text: &str, stage: &str) -> Vec<usize> {
    let line = text
        .lines()
        .find(|l| l.split_whitespace().next() == Some(stage))
        .unwrap_or_else(|| panic!("no {stage} row in\n{text}"));
    line.split_whitespace()
        .skip(1)
        .map(|n| n.parse().unwrap())
        .collect()
}

#[test]
fn report_with_and_without_the_outcome_table_and_leaves_bytes_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let src = vault(dir.path());
    let now = crate::now_ms() + 1000;

    // No state.db at all: no outcomes, no image records; nothing is created.
    let text = report(&src, now, 30, true).unwrap();
    assert!(!src.state_db.exists() && !src.search_db.exists());
    assert!(
        text.starts_with("intake notes (last 30 days): 3 blog=1 mail=1 youtube=0 clip=1\n"),
        "{text}"
    );
    //                         done pending skipped unfinished failed stuck
    assert_eq!(row(&text, "enrich"), [1, 0, 0, 1, 0, 0]);
    assert_eq!(row(&text, "auto_file"), [0, 1, 0, 0, 0, 0]);
    assert_eq!(row(&text, "localize"), [2, 1, 0, 0, 0, 0]);
    assert_eq!(row(&text, "image_describe"), [0, 0, 3, 0, 0, 0]);
    assert_eq!(row(&text, "finalize"), [0, 0, 3, 0, 0, 0]);
    assert_eq!(row(&text, "index"), [0, 3, 0, 0, 0, 0]);
    assert!(
        text.contains("mail Mail/m.md: enrich=unfinished index=pending\n"),
        "{text}"
    );
    assert!(!text.contains("Notes/plain.md"));

    // A v21 database (before the stage table): no recorded outcomes, same counts.
    {
        cv_state::migrate::create_at_version(&src.state_db, 21).unwrap();
    }
    let before = snapshot(dir.path());
    assert_eq!(report(&src, now, 30, false).unwrap(), {
        let mut t = text.clone();
        t.truncate(t.find("blog Blog/a.md").unwrap());
        t
    });
    assert_unchanged(&before, &snapshot(dir.path()));
    std::fs::remove_file(&src.state_db).unwrap();

    // The current schema with a recorded index failure for the mail note.
    {
        let mut s = StateDb::open(&src.state_db, 1, QueueConfig::default()).unwrap();
        let id = i64::try_from(s.note_id_bind("Mail/m.md").unwrap().0).unwrap();
        s.upsert_stage_outcome(&StageOutcomeRow {
            note_id: id,
            stage: "index".into(),
            outcome: "failed".into(),
            reason: "embedder down".into(),
            attempts: 2,
            fingerprint: String::new(),
            first_ms: 1,
            updated_ms: 1,
        })
        .unwrap();
    }
    let before = snapshot(dir.path());
    let text = report(&src, now, 30, true).unwrap();
    assert_unchanged(&before, &snapshot(dir.path()));
    assert_eq!(row(&text, "index"), [0, 2, 0, 0, 1, 0]);
    assert!(
        text.contains("mail Mail/m.md: enrich=unfinished index=failed(2)\n"),
        "{text}"
    );

    // The window: a clock 31 days on sees nothing.
    let later = report(&src, now + 31 * DAY_MS, 30, false).unwrap();
    assert!(
        later.starts_with("intake notes (last 30 days): 0 "),
        "{later}"
    );
}
