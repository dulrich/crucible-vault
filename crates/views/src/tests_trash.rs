use super::tracker::INTAKE;
use super::*;

const TRASH_DEF: &str = "source = 'trash'\n[[views]]\ntype='table'\nname='T'\norder=['name', 'reason', 'expires', 'bytes']\n";

fn trash_db(dir: &Path) -> PathBuf {
    let p = dir.join("trash-state.db");
    let c = rusqlite::Connection::open(&p).unwrap();
    c.execute_batch(
        "CREATE TABLE trash_entries (id INTEGER PRIMARY KEY, location TEXT NOT NULL,
            trash_path TEXT NOT NULL, original_path TEXT NOT NULL, trashed_ms INTEGER NOT NULL,
            bytes INTEGER NOT NULL, reason TEXT NOT NULL, mtime_ms INTEGER, inode INTEGER,
            info_name TEXT);
         INSERT INTO trash_entries (id, location, trash_path, original_path, trashed_ms, bytes, reason) VALUES
            (1, 'vault', '.trash/b.md', 'core/notes/b.md', 1790000000000, 2097152, 'deleted'),
            (2, 'system', 'files/a.txt', 'a.txt', 1789000000000, 10, 'orphan');",
    )
    .unwrap();
    p
}

#[test]
fn state_change_signals_the_trash_views() {
    let fx = fixture(&[("intake.toml", INTAKE), ("trash.toml", TRASH_DEF)]);
    let mut host = fx.host();
    assert_eq!(host.on_state_changed("trash_entries"), vec!["trash#T"]);
    assert_eq!(host.on_state_changed("tracker_items"), vec!["intake#Day"]);
}

#[test]
fn trash_view_needs_a_state_db() {
    let fx = fixture(&[("trash.toml", TRASH_DEF)]);
    let mut host = fx.host();
    let g = fx.reader().generation();
    let e = host
        .run(
            g,
            &RunRequest {
                this_path: None,
                ..RunRequest::view("trash#T")
            },
        )
        .unwrap_err();
    assert!(e.to_string().contains("no state.db"), "{e}");
}

#[test]
fn trash_records_bind_name_expiry_and_size() {
    use crate::value::Value;
    let dir = tempfile::tempdir().unwrap();
    let c = rusqlite::Connection::open(trash_db(dir.path())).unwrap();
    let recs = crate::service::trash_records(&c, 30).unwrap();
    // Oldest first.
    assert_eq!(
        format!("{:?}", recs[0]["name"]),
        format!("{:?}", Value::Str("a.txt".into()))
    );
    assert_eq!(
        format!("{:?}", recs[1]["name"]),
        format!("{:?}", Value::Str("b.md".into()))
    );
    assert_eq!(
        format!("{:?}", recs[1]["original_path"]),
        format!("{:?}", Value::Str("core/notes/b.md".into()))
    );
    assert_eq!(
        format!("{:?}", recs[1]["location"]),
        format!("{:?}", Value::Str("vault".into()))
    );
    assert_eq!(
        format!("{:?}", recs[1]["reason"]),
        format!("{:?}", Value::Str("deleted".into()))
    );
    assert_eq!(
        format!("{:?}", recs[1]["bytes"]),
        format!("{:?}", Value::Num(2_097_152.0))
    );
    assert_eq!(
        format!("{:?}", recs[1]["size_mib"]),
        format!("{:?}", Value::Num(2.0))
    );
    // Expiry is trashed + 30 days exactly.
    assert_eq!(
        format!("{:?}", recs[1]["expires"]),
        format!(
            "{:?}",
            crate::eval::ms_date(1_790_000_000_000 + 30 * 86_400_000)
        )
    );
    assert!(matches!(recs[1]["trashed"], Value::Date(..)));
}

#[test]
fn trash_retention_zero_means_no_expiry() {
    use crate::value::Value;
    let dir = tempfile::tempdir().unwrap();
    let c = rusqlite::Connection::open(trash_db(dir.path())).unwrap();
    let recs = crate::service::trash_records(&c, 0).unwrap();
    assert!(recs.iter().all(|r| matches!(r["expires"], Value::Null)));
}

#[test]
fn trash_view_runs_end_to_end_and_follows_the_pushed_retention() {
    let fx = fixture(&[("trash.toml", TRASH_DEF)]);
    let state = trash_db(fx._tmp.path());
    let mut host = fx
        .host()
        .with_state_db(&state)
        .with_trash_retention_days(30);
    let r = fx.run(&mut host, "trash#T", None);
    assert!(r.notices.is_empty(), "{:?}", r.notices);
    assert!(
        r.rows()
            .all(|row| matches!(row.cells[2], Cell::Date { .. }))
    );
    // `vault.toml` is never read by the host: only the pushed value counts.
    std::fs::write(
        fx.config.join("vault.toml"),
        "[trash]\nretention_days = 0\n",
    )
    .unwrap();
    let r = fx.run(&mut host, "trash#T", None);
    assert!(
        r.rows()
            .all(|row| matches!(row.cells[2], Cell::Date { .. }))
    );
    host.set_trash_retention_days(0);
    let r = fx.run(&mut host, "trash#T", None);
    assert!(r.rows().all(|row| row.cells[2] == Cell::Null));
}
