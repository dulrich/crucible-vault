use super::*;

fn mail_row(key: &str, id: &str) -> TrackerItem {
    TrackerItem {
        source_kind: "mail".into(),
        source_key: key.into(),
        item_id: id.into(),
        title: "T".into(),
        url: "u".into(),
        first_seen_ms: 1,
        day: "2026-10-21".into(),
        outcome: TrackerOutcome::Note,
        score: None,
        full_body: None,
        note_path: None,
        source_name: Some("Unknown sender a@b.example".into()),
        duration_s: None,
        word_count: None,
        access: None,
        complete: None,
        full_word_count: None,
    }
}

/// Plan intake-followups-mail-youtube WP-5 (audit E2): a re-key without a row at the new key
/// updates it in place, keeping status, `note_path` and evidence; a second call is a no-op.
#[test]
fn rekey_moves_a_row_in_place_and_is_idempotent() {
    let (_d, mut s) = db();
    let mut it = mail_row("unknown-sender", "m1");
    it.note_path = Some("n/a.md".into());
    it.complete = Some(true);
    s.insert_tracker_item(&it).unwrap();
    s.set_tracker_item_status("mail", "unknown-sender", "m1", Some('x'))
        .unwrap();
    let r = s
        .rekey_tracker_item("mail", "unknown-sender", "m1", "Newsbit", "Newsbit")
        .unwrap();
    assert_eq!(r, Rekey::Moved);
    assert!(
        s.tracker_item("mail", "unknown-sender", "m1")
            .unwrap()
            .is_none()
    );
    let got = s.tracker_item("mail", "Newsbit", "m1").unwrap().unwrap();
    assert_eq!(
        (
            got.note_path.as_deref(),
            got.complete,
            got.source_name.as_deref()
        ),
        (Some("n/a.md"), Some(true), Some("Newsbit"))
    );
    assert_eq!(
        s.tracker_item_status("mail", "Newsbit", "m1").unwrap(),
        Some(Some('x'))
    );
    let again = s
        .rekey_tracker_item("mail", "unknown-sender", "m1", "Newsbit", "Newsbit")
        .unwrap();
    assert_eq!(again, Rekey::Absent);
    assert_eq!(
        s.tracker_rows_of_source("mail", "Newsbit").unwrap().len(),
        1
    );
}

/// A collision keeps the new-key row, fills its NULL fields (and a NULL status) from the old
/// row, never overwrites a set field, and deletes the old row.
#[test]
fn rekey_collision_merges_into_the_existing_row() {
    let (_d, mut s) = db();
    let mut old = mail_row("unknown-sender", "m1");
    old.note_path = Some("n/old.md".into());
    old.access = Some("free".into());
    old.complete = Some(true);
    old.full_word_count = Some(900);
    let mut new = mail_row("Newsbit", "m1");
    new.note_path = Some("n/new.md".into());
    new.complete = Some(false);
    s.insert_tracker_item(&old).unwrap();
    s.insert_tracker_item(&new).unwrap();
    s.set_tracker_item_status("mail", "unknown-sender", "m1", Some('x'))
        .unwrap();
    let r = s
        .rekey_tracker_item("mail", "unknown-sender", "m1", "Newsbit", "Newsbit")
        .unwrap();
    assert_eq!(r, Rekey::Merged);
    assert!(
        s.tracker_item("mail", "unknown-sender", "m1")
            .unwrap()
            .is_none()
    );
    let got = s.tracker_item("mail", "Newsbit", "m1").unwrap().unwrap();
    assert_eq!(got.note_path.as_deref(), Some("n/new.md"), "set field kept");
    assert_eq!(got.complete, Some(false), "set field kept");
    assert_eq!(got.access.as_deref(), Some("free"), "NULL filled");
    assert_eq!(got.full_word_count, Some(900), "NULL filled");
    assert_eq!(
        s.tracker_item_status("mail", "Newsbit", "m1").unwrap(),
        Some(Some('x')),
        "NULL status taken from the old row"
    );
}

#[test]
fn tracker_open_rows_and_rows_of_source_ignore_outcome() {
    let (_d, mut s) = db();
    for (id, outcome) in [
        ("a", TrackerOutcome::Line),
        ("b", TrackerOutcome::Note),
        ("c", TrackerOutcome::Skipped),
    ] {
        let mut it = mail_row("unknown-sender", id);
        it.source_kind = "youtube".into();
        it.outcome = outcome;
        s.insert_tracker_item(&it).unwrap();
    }
    s.set_tracker_item_status("youtube", "unknown-sender", "b", Some('x'))
        .unwrap();
    let open: Vec<String> = s
        .tracker_open_rows("youtube")
        .unwrap()
        .into_iter()
        .map(|r| r.item_id)
        .collect();
    assert_eq!(open, vec!["a", "c"]);
    assert_eq!(
        s.tracker_rows_of_source("youtube", "unknown-sender")
            .unwrap()
            .len(),
        3
    );
}

/// Plan intake-round-2-clean-dedupe-reading WP-6: the note-linked rows, and nothing else.
#[test]
fn tracker_items_with_note_lists_only_linked_rows() {
    let (_d, mut s) = db();
    for (id, path) in [("a", Some("n/a.md")), ("b", None), ("c", Some("n/c.md"))] {
        let mut it = mail_row("Newsbit", id);
        it.note_path = path.map(str::to_owned);
        s.insert_tracker_item(&it).unwrap();
    }
    let got: Vec<String> = s
        .tracker_items_with_note()
        .unwrap()
        .into_iter()
        .map(|r| r.item_id)
        .collect();
    assert_eq!(got, vec!["a", "c"]);
}

/// `dup_labels` round-trips a row.
#[test]
fn dup_labels_roundtrip() {
    let (_d, mut s) = db();
    let l = crate::DupLabel {
        keep_path: "a.md".into(),
        other_path: "b.md".into(),
        keep_identity: Some("id".into()),
        other_identity: None,
        containment_keep_in_other: 0.5,
        containment_other_in_keep: 0.75,
        identity_match: false,
        verdict: "keep".into(),
        at_ms: 7,
    };
    s.insert_dup_label(&l).unwrap();
    assert_eq!(s.dup_labels().unwrap(), vec![l]);
}

fn trash_entry(path: &str, reason: TrashReason, at: i64) -> NewTrashEntry {
    NewTrashEntry {
        location: TrashLocation::Vault,
        trash_path: format!(".trash/{path}"),
        original_path: path.into(),
        trashed_ms: at,
        bytes: 3,
        reason,
        mtime_ms: Some(10),
        inode: Some(42),
        info_name: None,
    }
}

/// A fresh database is created at the v15 baseline with the ledger and label tables present and empty.
#[test]
fn fresh_database_is_created_at_v15() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    let s = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    assert_eq!(migrate::BASELINE_VERSION, 15);
    assert_eq!(migrate::latest_version(), 15);
    assert_eq!(s.schema_version().unwrap(), 15);
    assert!(s.trash_entries().unwrap().is_empty());
    drop(s);
    let c = rusqlite::Connection::open(&p).unwrap();
    for t in [
        "mail_boilerplate",
        "mail_boilerplate_seen",
        "dup_labels",
        "trash_entries",
    ] {
        let n: i64 = c
            .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{t}");
    }
}

/// The ledger accessors round-trip, refuse unknown spellings, and every mutation signals.
#[test]
fn trash_entries_roundtrip_and_signal() {
    use std::sync::{Arc, Mutex};
    let (_d, mut s) = db();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sn = Arc::clone(&seen);
    s.set_change_hook(Box::new(move |t| sn.lock().unwrap().push(t)));
    let a = trash_entry("a.md", TrashReason::Deleted, 5);
    let mut b = trash_entry("b.png", TrashReason::NoteAttachment, 2);
    b.location = TrashLocation::System;
    b.trash_path = "b.png".into();
    b.info_name = Some("b.png.trashinfo".into());
    let ia = s.insert_trash_entry(&a).unwrap();
    let ib = s.insert_trash_entry(&b).unwrap();
    let all = s.trash_entries().unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!((all[0].id, &all[0].entry), (ib, &b), "oldest first");
    assert_eq!(
        s.trash_entry_by_path(TrashLocation::Vault, ".trash/a.md")
            .unwrap()
            .map(|e| e.id),
        Some(ia)
    );
    assert!(
        s.trash_entry_by_path(TrashLocation::System, ".trash/a.md")
            .unwrap()
            .is_none()
    );
    // The CHECKs still refuse unknown spellings (reachable by raw SQL only now).
    for (loc, reason) in [("vault", "lost"), ("bin", "deleted")] {
        assert!(
            s.conn()
                .execute(
                    "INSERT INTO trash_entries (location, trash_path, original_path, trashed_ms,
                         bytes, reason) VALUES (?1, 'x', 'x', 1, 1, ?2)",
                    rusqlite::params![loc, reason],
                )
                .is_err()
        );
    }
    assert!(s.delete_trash_entry(ia).unwrap());
    assert!(!s.delete_trash_entry(ia).unwrap());
    assert_eq!(s.trash_entries().unwrap().len(), 1);
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["trash_entries", "trash_entries", "trash_entries"]
    );
}

fn line_row(kind: &str, key: &str, id: &str, name: &str) -> TrackerItem {
    let mut it = mail_row(key, id);
    it.source_kind = kind.into();
    it.outcome = TrackerOutcome::Line;
    it.source_name = Some(name.into());
    it
}

/// Plan cancel-source-intake WP-1: only the open `line` rows of the exact source change; done,
/// cancelled, `note`, `skipped` rows and other sources are untouched; reasons are recorded.
#[test]
fn cancel_source_open_items_touches_only_open_rows_of_one_source() {
    let (_d, mut s) = db();
    for (key, id) in [("UCa", "o1"), ("UCa", "o2"), ("UCa", "d1"), ("UCa", "c1")] {
        s.insert_tracker_item(&line_row("youtube", key, id, "Chan A"))
            .unwrap();
    }
    s.set_tracker_item_status("youtube", "UCa", "d1", Some('x'))
        .unwrap();
    s.set_tracker_item_status("youtube", "UCa", "c1", Some('-'))
        .unwrap();
    for (id, outcome) in [
        ("n1", TrackerOutcome::Note),
        ("s1", TrackerOutcome::Skipped),
    ] {
        let mut it = line_row("youtube", "UCa", id, "Chan A");
        it.outcome = outcome;
        s.insert_tracker_item(&it).unwrap();
    }
    s.insert_tracker_item(&line_row("youtube", "UCb", "b1", "Chan B"))
        .unwrap();
    s.insert_tracker_item(&line_row("blog", "UCa", "k1", "Blog"))
        .unwrap();
    assert_eq!(
        s.tracker_open_sources("youtube").unwrap(),
        vec![
            ("UCa".to_owned(), Some("Chan A".to_owned())),
            ("UCb".to_owned(), Some("Chan B".to_owned()))
        ]
    );
    assert_eq!(
        s.cancel_source_open_items("youtube", "UCa", "source-removed", 5, usize::MAX)
            .unwrap(),
        2
    );
    for id in ["o1", "o2"] {
        let it = s.tracker_item("youtube", "UCa", id).unwrap().unwrap();
        assert_eq!(it.outcome, TrackerOutcome::Ignored);
        assert_eq!(
            s.tracker_item_status("youtube", "UCa", id).unwrap(),
            Some(Some('-'))
        );
        let reason: String = s
            .conn()
            .query_row(
                "SELECT reason FROM ignored_ids WHERE kind = 'youtube' AND id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reason, "source-removed");
    }
    let st = |k: &str, key: &str, id: &str| s.tracker_item_status(k, key, id).unwrap();
    assert_eq!(st("youtube", "UCa", "d1"), Some(Some('x')));
    assert_eq!(st("youtube", "UCa", "c1"), Some(Some('-')));
    assert_eq!(st("youtube", "UCa", "n1"), Some(None));
    assert_eq!(st("youtube", "UCa", "s1"), Some(None));
    assert_eq!(st("youtube", "UCb", "b1"), Some(None));
    assert_eq!(st("blog", "UCa", "k1"), Some(None));
    for id in ["d1", "c1", "n1", "s1", "b1"] {
        assert!(!s.is_ignored("youtube", id).unwrap(), "{id}");
    }
    // Idempotent, and the limit bounds a run.
    assert_eq!(
        s.cancel_source_open_items("youtube", "UCa", "source-removed", 6, usize::MAX)
            .unwrap(),
        0
    );
    s.insert_tracker_item(&line_row("youtube", "UCc", "z1", "C"))
        .unwrap();
    s.insert_tracker_item(&line_row("youtube", "UCc", "z2", "C"))
        .unwrap();
    assert_eq!(
        s.cancel_source_open_items("youtube", "UCc", "source-cancelled", 7, 1)
            .unwrap(),
        1
    );
}

/// Plan round-17 WP-1: prune deletes only rows whose file is gone (both locations), keeps present
/// files, unsafe paths and a missing system root, and signals once.
#[test]
fn prune_missing_trash_tracks_the_disk() {
    use std::sync::{Arc, Mutex};
    let (_d, mut s) = db();
    let vault = tempfile::tempdir().unwrap();
    let sys = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join(".trash")).unwrap();
    std::fs::create_dir_all(sys.path().join("files")).unwrap();
    std::fs::write(vault.path().join(".trash/present.md"), "x").unwrap();
    std::fs::write(sys.path().join("files/present.png"), "x").unwrap();
    let sys_row = |name: &str| NewTrashEntry {
        location: TrashLocation::System,
        trash_path: name.into(),
        info_name: Some(format!("{name}.trashinfo")),
        ..trash_entry(name, TrashReason::Deleted, 1)
    };
    s.insert_trash_entry(&trash_entry("present.md", TrashReason::Deleted, 1))
        .unwrap();
    s.insert_trash_entry(&trash_entry("missing.md", TrashReason::Deleted, 2))
        .unwrap();
    s.insert_trash_entry(&sys_row("present.png")).unwrap();
    s.insert_trash_entry(&sys_row("missing.png")).unwrap();
    s.insert_trash_entry(&trash_entry("../escape.md", TrashReason::Deleted, 3))
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sn = Arc::clone(&seen);
    s.set_change_hook(Box::new(move |t| sn.lock().unwrap().push(t)));
    // No system root: system rows are kept (only the missing vault row goes).
    assert_eq!(s.prune_missing_trash(vault.path(), None).unwrap(), 1);
    assert_eq!(
        s.prune_missing_trash(vault.path(), Some(sys.path()))
            .unwrap(),
        1
    );
    assert_eq!(
        s.prune_missing_trash(vault.path(), Some(sys.path()))
            .unwrap(),
        0
    );
    let mut left: Vec<String> = s
        .trash_entries()
        .unwrap()
        .into_iter()
        .map(|r| r.entry.trash_path)
        .collect();
    left.sort();
    assert_eq!(
        left,
        [".trash/../escape.md", ".trash/present.md", "present.png"]
    );
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["trash_entries", "trash_entries"]
    );
}
