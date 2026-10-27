use super::*;

// ---- stored forms of the protocol enums (WP-X3: byte-identical to the retired mirrors) --------

#[test]
fn protocol_enums_keep_stored_forms_and_corrupt_handling() {
    let (_d, mut s) = db();
    let raw_str = |s: &StateDb, sql: &str, key: &str| -> String {
        s.conn().query_row(sql, [key], |r| r.get(0)).unwrap()
    };
    // lane: INTEGER rank, User=0 / Background=1.
    for (lane, rank) in [(Lane::User, 0_i64), (Lane::Background, 1)] {
        let id = id_of(
            s.enqueue(
                NewJob {
                    lane,
                    ..job("t", None, &[])
                },
                1,
            )
            .unwrap(),
        );
        let got: i64 = s
            .conn()
            .query_row("SELECT lane FROM jobs WHERE id = ?1", [&id], |r| r.get(0))
            .unwrap();
        assert_eq!(got, rank);
        assert_eq!(s.job(&id).unwrap().unwrap().lane, lane);
        s.cancel(&id, 1).unwrap();
    }
    // failure_kind: settled through the store.
    for (reason, kind, text) in [
        (
            Some(FailureReason::ServiceOutage),
            FailureKind::Service,
            "service",
        ),
        (None, FailureKind::Job, "job"),
    ] {
        let id = id_of(s.enqueue(job("f", None, &[]), 2).unwrap());
        assert_eq!(s.claim(2).unwrap().unwrap().id, id);
        let r = JobResult::Failed {
            error: "e".into(),
            reason,
        };
        s.settle(&id, &r, 3).unwrap();
        let sql = "SELECT failure_kind FROM jobs WHERE id = ?1";
        assert_eq!(raw_str(&s, sql, &id), text);
        assert_eq!(s.job(&id).unwrap().unwrap().failure_kind, Some(kind));
    }
    // tracker outcome.
    for (o, text) in [
        (TrackerOutcome::Line, "line"),
        (TrackerOutcome::Note, "note"),
        (TrackerOutcome::Ignored, "ignored"),
        (TrackerOutcome::Skipped, "skipped"),
    ] {
        let item = TrackerItem {
            source_kind: "blog".into(),
            source_key: "k".into(),
            item_id: text.into(),
            title: "T".into(),
            url: "u".into(),
            first_seen_ms: 1,
            day: "2026-10-17".into(),
            outcome: o,
            score: None,
            full_body: None,
            note_path: None,
            source_name: None,
            duration_s: None,
            word_count: None,
            access: None,
            complete: None,
            full_word_count: None,
        };
        assert!(s.insert_tracker_item(&item).unwrap());
        let sql = "SELECT outcome FROM tracker_items WHERE item_id = ?1";
        assert_eq!(raw_str(&s, sql, text), text);
        let back = s.tracker_item("blog", "k", text).unwrap().unwrap();
        assert_eq!(back.outcome, o);
    }
    // breaker state.
    for (st, text) in [
        (BreakerState::Closed, "closed"),
        (BreakerState::Open, "open"),
        (BreakerState::HalfOpen, "half_open"),
    ] {
        let mut h = ServiceHealth::fresh(text);
        h.state = st;
        crate::health::store(s.conn(), &h).unwrap();
        let sql = "SELECT state FROM service_health WHERE service = ?1";
        assert_eq!(raw_str(&s, sql, text), text);
        assert_eq!(s.service_health(text).unwrap().state, st);
    }
    // Pre-existing bad raw values: lane / failure_kind / breaker state are Corrupt; an unknown
    // outcome string parses to `None` (the read path then falls back to `Skipped`).
    let bad = id_of(s.enqueue(job("b", None, &[]), 4).unwrap());
    let c = s.conn();
    c.execute("UPDATE jobs SET lane = 7 WHERE id = ?1", [&bad])
        .unwrap();
    assert!(matches!(
        s.job(&bad),
        Err(StateError::Corrupt { column: "lane", .. })
    ));
    c.execute(
        "UPDATE jobs SET lane = 1, failure_kind = 'x' WHERE id = ?1",
        [&bad],
    )
    .unwrap();
    assert!(matches!(
        s.job(&bad),
        Err(StateError::Corrupt {
            column: "failure_kind",
            ..
        })
    ));
    c.execute(
        "UPDATE service_health SET state = 'x' WHERE service = 'open'",
        [],
    )
    .unwrap();
    assert!(matches!(
        s.service_health("open"),
        Err(StateError::Corrupt {
            column: "service_health.state",
            ..
        })
    ));
    // `tracker_items.outcome` has a CHECK constraint, so a bad stored outcome cannot exist.
    assert_eq!(TrackerOutcome::parse("x"), None);
}
