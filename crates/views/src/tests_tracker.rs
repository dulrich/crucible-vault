use super::*;

/// Plan round-5-reading-pickers-intake WP-10: a fixture `state.db` with `tracker_items` (the
/// columns of `cv-state` `migrate.rs`; this crate cannot depend on `cv-state`).
fn tracker_db(dir: &Path) -> PathBuf {
    let p = dir.join("state.db");
    let c = rusqlite::Connection::open(&p).unwrap();
    c.execute_batch(
        "CREATE TABLE tracker_items (source_kind TEXT NOT NULL, source_key TEXT NOT NULL,
            item_id TEXT NOT NULL, title TEXT NOT NULL DEFAULT '', url TEXT NOT NULL DEFAULT '',
            first_seen_ms INTEGER NOT NULL, day TEXT NOT NULL, outcome TEXT NOT NULL,
            score REAL, full_body INTEGER, note_path TEXT,
            source_name TEXT, duration_s INTEGER, word_count INTEGER, status TEXT,
            access TEXT, complete INTEGER, full_word_count INTEGER,
            PRIMARY KEY (source_kind, source_key, item_id));
         INSERT INTO tracker_items VALUES
            ('youtube', 'UC1', 'v1', 'Old video', 'https://y/1', 1, '2026-10-02', 'line', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL),
            ('blog', 'b', 'p1', 'Post', 'https://b/1', 3, '2026-10-03', 'note', 0.5, 1, 'notes/Post.md', 'Feed B', NULL, 900, NULL, 'paid', 0, 9000),
            ('youtube', 'UC1', 'v2', 'New video', 'https://y/2', 2, '2026-10-03', 'line', 2.0, NULL, NULL, 'Chan', 125, NULL, 'x', NULL, NULL, NULL),
            ('youtube', 'UC1', 'v3', 'Skipped', 'https://y/3', 4, '2026-10-03', 'skipped', NULL, NULL, NULL, NULL, NULL, NULL, '/', NULL, NULL, NULL);",
    )
    .unwrap();
    p
}

pub(super) const INTAKE: &str = "source = 'tracker_items'\n[formulas]\nsrc = 'source_kind + \"/\" + source_key'\n[[views]]\ntype='table'\nname='Day'\nfilters = { and = ['day == this.file.name', 'outcome == \"line\" || note.outcome == \"note\"'] }\norder = ['title', 'url', 'formula.src', 'score', 'full_body', 'item_id']\n";

#[test]
fn tracker_items_view_filters_by_this_day() {
    let fx = fixture(&[("intake.toml", INTAKE)]);
    let state = tracker_db(fx._tmp.path());
    let mut host = fx.host().with_state_db(&state);
    let r = fx.run(&mut host, "intake#Day", Some("daily/day/2026-10-03.md"));
    assert_eq!(r.kind, Kind::Table);
    assert!(r.notices.is_empty(), "{:?}", r.notices);
    assert_eq!(
        grid(&r),
        vec![
            vec!["Post", "https://b/1", "blog/b", "0.5", "true", "p1"],
            vec!["New video", "https://y/2", "youtube/UC1", "2", "", "v2"],
        ]
    );
    assert!(r.rows().all(|row| row.path.is_none()));
    let r = fx.run(&mut host, "intake#Day", Some("daily/day/2026-10-02.md"));
    assert_eq!(grid(&r).len(), 1);
    // No `this`: `day == null` matches nothing.
    assert!(grid(&fx.run(&mut host, "intake#Day", None)).is_empty());
}

#[test]
fn tracker_records_expose_done_note_path_key_and_url_cells() {
    const DONE: &str = "source = 'tracker_items'\n[formulas]\nt = 'if(outcome == \"note\" && note_path, link(note_path, title), url(url, title))'\n[[views]]\ntype='table'\nname='Day'\nfilters = { and = ['day == this.file.name'] }\norder = ['formula.t', 'done', 'note_path', 'status']\n";
    let fx = fixture(&[("intake.toml", DONE)]);
    let state = tracker_db(fx._tmp.path());
    let mut host = fx.host().with_state_db(&state);
    let r = fx.run(&mut host, "intake#Day", Some("daily/day/2026-10-03.md"));
    assert!(r.notices.is_empty(), "{:?}", r.notices);
    let rows: Vec<_> = r.rows().collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[1].cells,
        vec![
            Cell::Link {
                path: "notes/Post.md".into(),
                label: "Post".into()
            },
            Cell::Bool { v: false },
            Cell::Text {
                v: "notes/Post.md".into()
            },
            Cell::Null,
        ]
    );
    assert_eq!(rows[2].cells[3], Cell::Text { v: "x".into() });
    assert_eq!(
        rows[2].cells[..2],
        [
            Cell::Url {
                url: "https://y/2".into(),
                label: "New video".into()
            },
            Cell::Bool { v: true },
        ]
    );
    assert_eq!(rows[2].cells[2], Cell::Null);
    let key = crate::result::TrackerKey::decode(rows[2].key.as_deref().unwrap()).unwrap();
    assert_eq!(
        (
            key.source_kind.as_str(),
            key.source_key.as_str(),
            key.item_id.as_str()
        ),
        ("youtube", "UC1", "v2")
    );
    assert_eq!(crate::result::TrackerKey::decode(&key.encode()), Some(key));
    assert_eq!(crate::result::TrackerKey::decode("a.md"), None);
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(json["groups"][0]["rows"][2]["cells"][0]["t"], "url");
}

/// A record source's `status` column carries
/// `ColumnRole::Status` (nothing else does); it round-trips the wire and an index view's columns
/// carry no role.
#[test]
fn record_status_column_carries_the_status_role() {
    use crate::result::{ColumnRole, ViewResult};
    const DONE: &str = "source = 'tracker_items'\n[[views]]\ntype='table'\nname='Day'\nfilters = { and = ['day == this.file.name'] }\norder = ['title', 'done', 'status']\n";
    let fx = fixture(&[("intake.toml", DONE)]);
    let state = tracker_db(fx._tmp.path());
    let mut host = fx.host().with_state_db(&state);
    let r = fx.run(&mut host, "intake#Day", Some("daily/day/2026-10-03.md"));
    let roles: Vec<_> = r.columns.iter().map(|c| c.role.clone()).collect();
    assert_eq!(roles, [None, None, Some(ColumnRole::Status)]);
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(
        json["columns"][2]["role"],
        serde_json::json!({"kind": "status"})
    );
    assert!(json["columns"][0].get("role").is_none(), "no role = absent");
    let back: ViewResult = serde_json::from_value(json).unwrap();
    assert_eq!(back.columns, r.columns);
}

#[test]
fn tracker_records_expose_source_name_duration_and_word_count() {
    use crate::value::Value;
    let dir = tempfile::tempdir().unwrap();
    let c = rusqlite::Connection::open(tracker_db(dir.path())).unwrap();
    let recs = crate::service::tracker_records(&c).unwrap();
    let show = |v: &Value| match v {
        Value::Null => "null".to_owned(),
        Value::Str(s) => format!("s:{s}"),
        Value::Num(n) => format!("n:{n}"),
        _ => "other".to_owned(),
    };
    let get = |id: &str| {
        let r = recs
            .iter()
            .find(|r| matches!(&r["item_id"], Value::Str(s) if s == id))
            .unwrap();
        [
            show(&r["source_name"]),
            show(&r["duration"]),
            show(&r["word_count"]),
        ]
    };
    assert_eq!(get("v2"), ["s:Chan", "n:125", "null"]);
    assert_eq!(get("p1"), ["s:Feed B", "null", "n:900"]);
    assert_eq!(get("v1"), ["null", "null", "null"]);
    let p1 = recs
        .iter()
        .find(|r| matches!(&r["item_id"], Value::Str(s) if s == "p1"))
        .unwrap();
    assert_eq!(show(&p1["access"]), "s:paid");
    assert!(matches!(p1["complete"], Value::Bool(false)));
    assert_eq!(show(&p1["full_word_count"]), "n:9000");
    let v1 = recs
        .iter()
        .find(|r| matches!(&r["item_id"], Value::Str(s) if s == "v1"))
        .unwrap();
    assert_eq!(
        [
            show(&v1["access"]),
            show(&v1["complete"]),
            show(&v1["full_word_count"])
        ],
        ["null", "null", "null"]
    );
}

#[test]
fn tracker_items_view_needs_a_state_db() {
    let fx = fixture(&[("intake.toml", INTAKE)]);
    let mut host = fx.host();
    let g = fx.reader().generation();
    let e = host
        .run(
            g,
            &RunRequest {
                this_path: None,
                ..RunRequest::view("intake#Day")
            },
        )
        .unwrap_err();
    assert!(e.to_string().contains("no state.db"), "{e}");
}

#[test]
fn state_change_invalidates_only_tracker_views() {
    let fx = fixture(&[
        ("intake.toml", INTAKE),
        (
            "notes.toml",
            "[[views]]\ntype='table'\nname='All'\norder=['file.name']\n",
        ),
    ]);
    let mut host = fx.host();
    assert_eq!(host.on_state_changed("tracker_items"), vec!["intake#Day"]);
    assert!(host.on_state_changed("link_records").is_empty());
    assert_eq!(host.on_index_generation(99), vec!["notes#All"]);
}
