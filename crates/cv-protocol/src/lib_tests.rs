use super::*;
use serde::de::DeserializeOwned;
use serde_json::json;

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: &T) -> Value {
    let s = serde_json::to_string(v).expect("serialize");
    let back: T = serde_json::from_str(&s).expect("deserialize");
    assert_eq!(&back, v);
    serde_json::from_str(&s).expect("value")
}

#[test]
fn hello_request_wire_shape() {
    let req = RpcRequest::new(
        RpcId::Num(1),
        Request::Hello(HelloParams {
            protocol_version: PROTOCOL_VERSION,
            client: "cv".into(),
        }),
    );
    let v = round_trip(&req);
    assert_eq!(
        v,
        json!({"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol_version":1,"client":"cv"}})
    );
}

#[test]
fn unit_requests_round_trip() {
    for call in [
        Request::ActionsList,
        Request::Status,
        Request::ViewsList,
        Request::LookupCurrencies,
    ] {
        round_trip(&RpcRequest::new(RpcId::Str("a".into()), call));
    }
}

#[test]
fn all_requests_round_trip() {
    let calls = vec![
        Request::Subscribe(SubscribeParams {
            topics: vec![Topic::FileChanged, Topic::ViewsInvalidated],
        }),
        Request::WriteNote(WriteNoteParams {
            id: None,
            path: "a.md".into(),
            content: "x".into(),
            base_hash: "h".into(),
        }),
        Request::CreateNote(CreateNoteParams {
            path: "b.md".into(),
            content: String::new(),
        }),
        Request::WriteAttachment(WriteAttachmentParams {
            note_path: "b.md".into(),
            ext: "png".into(),
            data_base64: "AA==".into(),
        }),
        Request::Rename(RenameParams {
            id: None,
            from: "a.md".into(),
            to: "c.md".into(),
        }),
        Request::Delete(DeleteParams {
            path: "c.md".into(),
            mode: DeleteMode::Permanent,
        }),
        Request::DeletePreview(PathParams {
            path: "c.md".into(),
        }),
        Request::DeleteNote(DeleteNoteParams {
            path: "c.md".into(),
            attachments: vec!["img.png".into()],
            mode: DeleteMode::Trash,
        }),
        Request::RunAction(RunActionParams {
            action_id: "capture.daily".into(),
            params: serde_json::Map::from_iter([("k".to_owned(), json!("v"))]),
            context: ActionContext {
                active_path: Some("a.md".into()),
                line: Some(3),
                ..ActionContext::default()
            },
        }),
        Request::Job(JobIdParams {
            job_id: "j1".into(),
        }),
        Request::Cancel(JobIdParams {
            job_id: "j1".into(),
        }),
        Request::JobsList(JobsListParams {
            status: Some("deferred".into()),
            job_type: Some("t".into()),
            limit: Some(5),
            offset: Some(1),
            recent_first: Some(false),
        }),
        Request::JobsList(JobsListParams::default()),
        Request::JobsStats(JobsStatsParams {
            job_type: Some("t".into()),
            since_ms: Some(5),
        }),
        Request::JobsStats(JobsStatsParams::default()),
        Request::JobTimeline(JobIdParams {
            job_id: "j1".into(),
        }),
        Request::JobRetry(JobIdParams {
            job_id: "j1".into(),
        }),
        Request::IntakeList(IntakeListParams {
            source_kind: Some("youtube".into()),
            source_key: Some("ch".into()),
            day: Some("2026-10-15".into()),
            outcome: Some("line".into()),
            limit: Some(2),
            offset: Some(0),
        }),
        Request::Uri(UriParams {
            url: "obsidian://new?file=x".into(),
        }),
        Request::ViewRun(ViewRunParams {
            view_id: "v".into(),
            this_path: None,
            sort: None,
            select: None,
        }),
    ];
    for (i, call) in calls.into_iter().enumerate() {
        round_trip(&RpcRequest::new(RpcId::Num(i as u64), call));
    }
}

#[test]
fn delete_without_mode_is_trash() {
    let v: RpcRequest = serde_json::from_value(
        json!({"jsonrpc":"2.0","id":1,"method":"delete","params":{"path":"a.md"}}),
    )
    .expect("parse");
    assert_eq!(
        v.call,
        Request::Delete(DeleteParams {
            path: "a.md".into(),
            mode: DeleteMode::Trash,
        })
    );
    let v: RpcRequest = serde_json::from_value(
        json!({"jsonrpc":"2.0","id":2,"method":"delete_note","params":{"path":"a.md"}}),
    )
    .expect("parse");
    let Request::DeleteNote(p) = v.call else {
        panic!("wrong variant")
    };
    assert_eq!(p.mode, DeleteMode::Trash);
    assert!(p.attachments.is_empty());
}

#[test]
fn run_action_defaults_context_and_params() {
    let v: RpcRequest = serde_json::from_value(
        json!({"jsonrpc":"2.0","id":2,"method":"run_action","params":{"action_id":"x"}}),
    )
    .expect("parse");
    let Request::RunAction(p) = v.call else {
        panic!("wrong variant")
    };
    assert!(p.params.is_empty());
    assert_eq!(p.context, ActionContext::default());
}

#[test]
fn responses_round_trip() {
    let ok = RpcResponse {
        jsonrpc: JSONRPC_VERSION.into(),
        id: RpcId::Num(1),
        outcome: RpcOutcome::Result(json!({"hash":"abc"})),
    };
    assert_eq!(
        round_trip(&ok),
        json!({"jsonrpc":"2.0","id":1,"result":{"hash":"abc"}})
    );
    let err = RpcResponse {
        jsonrpc: JSONRPC_VERSION.into(),
        id: RpcId::Num(2),
        outcome: RpcOutcome::Error(RpcError {
            code: error_codes::VERSION_MISMATCH,
            message: "index_schema mismatch".into(),
            data: None,
        }),
    };
    round_trip(&err);
}

#[test]
fn search_wire_shapes() {
    let v = round_trip(&RpcRequest::new(
        RpcId::Num(1),
        Request::Search(SearchParams {
            query: "dragons".into(),
            limit: 50,
        }),
    ));
    assert_eq!(v["method"], "search");
    assert_eq!(v["params"], json!({"query": "dragons", "limit": 50}));
    let v = round_trip(&SearchResult {
        hits: vec![SearchHit {
            path: "n/a.md".into(),
            title: "A".into(),
            heading: "H".into(),
            snippet: "s".into(),
            score: 0.5,
        }],
        semantic: true,
    });
    assert_eq!(
        v,
        json!({"hits": [{"path": "n/a.md", "title": "A", "heading": "H", "snippet": "s", "score": 0.5}], "semantic": true})
    );
}

#[test]
fn lookup_wire_shapes() {
    let v = round_trip(&RpcRequest::new(
        RpcId::Num(1),
        Request::LookupModels(LookupModelsParams {
            provider: "local".into(),
        }),
    ));
    assert_eq!(v["method"], "lookup_models");
    assert_eq!(v["params"], json!({"provider": "local"}));
    round_trip(&RpcRequest::new(
        RpcId::Num(2),
        Request::LookupGeocode(LookupGeocodeParams {
            query: "aus".into(),
        }),
    ));
    round_trip(&RpcRequest::new(
        RpcId::Num(3),
        Request::LookupChannels(LookupChannelsParams {
            ids: vec!["UC1".into()],
        }),
    ));
    let v = round_trip(&RpcRequest::new(
        RpcId::Num(5),
        Request::TrackerItemSetStatus(TrackerItemSetStatusParams {
            source_kind: "youtube".into(),
            source_key: "UC1".into(),
            item_id: "v1".into(),
            status: Some('/'),
        }),
    ));
    assert_eq!(v["method"], "tracker_item_set_status");
    assert_eq!(
        v["params"],
        json!({"source_kind": "youtube", "source_key": "UC1", "item_id": "v1", "status": "/"})
    );
    let v = round_trip(&RpcRequest::new(
        RpcId::Num(6),
        Request::TrackerItemSetStatus(TrackerItemSetStatusParams {
            source_kind: "youtube".into(),
            source_key: "UC1".into(),
            item_id: "v1".into(),
            status: None,
        }),
    ));
    assert_eq!(v["params"]["status"], json!(null));
    let v = round_trip(&RpcRequest::new(
        RpcId::Num(7),
        Request::NoteSetBool(NoteSetBoolParams {
            path: "a.md".into(),
            key: "read".into(),
            value: true,
        }),
    ));
    assert_eq!(v["method"], "note_set_bool");
    assert_eq!(
        v["params"],
        json!({"path": "a.md", "key": "read", "value": true})
    );
    round_trip(&NoteSetBoolResult { hash: "h".into() });
    let report = MemoryReport {
        uptime_secs: 60,
        rss: Some(1 << 20),
        anon: Some(512),
        swap: None,
        hwm: Some(2 << 20),
        malloc_in_use: Some(100),
        malloc_system: Some(300),
        gauges: vec![
            GaugeValue {
                name: "g".into(),
                value: Some(3),
                error: None,
            },
            GaugeValue {
                name: "bad".into(),
                value: None,
                error: Some("lock poisoned".into()),
            },
        ],
    };
    let v = round_trip(&RpcRequest::new(
        RpcId::Num(8),
        Request::MemoryReport(report.clone()),
    ));
    assert_eq!(v["method"], "memory_report");
    assert_eq!(v["params"].get("swap"), None);
    round_trip(&StatusResult {
        generation: 1,
        queue: std::collections::BTreeMap::new(),
        config_errors: vec![],
        services: vec![],
        tracker_items: None,
        link_records: None,
        memory: Some(Box::new(MemoryStatus {
            daemon: report.clone(),
            editor: Some(ReportedMemory {
                report,
                age_secs: 5,
            }),
        })),
    });
    // A minimal report (future peer omitting everything optional) still parses.
    let min: MemoryReport = serde_json::from_value(json!({})).expect("min report");
    assert_eq!(min, MemoryReport::default());
    let r = round_trip(&LookupModelsResult {
        models: vec![LookupModel {
            id: "m".into(),
            display_name: None,
        }],
        live: false,
    });
    assert_eq!(r, json!({"models": [{"id": "m"}], "live": false}));
    round_trip(&vec![LookupGeoResult {
        label: "Gotham, US".into(),
        admin1: Some("Exampleland".into()),
        country_code: "US".into(),
        lat: 30.0,
        lon: -100.0,
    }]);
    round_trip(&vec![LookupCurrency {
        code: "EUR".into(),
        name: "Euro".into(),
    }]);
    round_trip(&vec![LookupChannel {
        id: "UC1".into(),
        title: "T".into(),
    }]);
}

#[test]
fn results_round_trip() {
    round_trip(&HelloResult {
        protocol_version: 1,
        index_schema: 1,
        vault_root: "/v".into(),
        index_db: "/i.db".into(),
        state_db: "/s.db".into(),
        generation: 7,
    });
    assert_eq!(
        round_trip(&WriteNoteResult::Conflict {
            path: None,
            current_hash: "h2".into()
        }),
        json!({"conflict":{"current_hash":"h2"}})
    );
    round_trip(&WriteNoteResult::Written {
        path: None,
        hash: "h".into(),
    });
    round_trip(&CreateNoteResult {
        path: "x 1.md".into(),
    });
    round_trip(&WriteAttachmentResult {
        path: "_attachments/x/a_MD5.png".into(),
    });
    round_trip(&vec![ActionInfo {
        id: "a".into(),
        title: "A".into(),
        group: None,
        keys: vec![],
        params: vec![ParamSpec {
            name: "p".into(),
            label: "P".into(),
            kind: ParamKind::Enum,
            required: true,
            choices: vec!["x".into()],
            default: None,
        }],
        needs: vec![ContextNeed::ActivePath],
        durable: true,
    }]);
    round_trip(&RunActionResult {
        job_id: "j".into(),
        open_path: Some("d.md".into()),
    });
    round_trip(&UriResult::Spooled);
    round_trip(&UriResult::Created {
        path: "n.md".into(),
    });
    round_trip(&StatusResult {
        generation: 1,
        queue: [("pending".to_owned(), 2)].into_iter().collect(),
        config_errors: vec![],
        services: vec![ServiceHealth {
            service: "search".into(),
            state: BreakerState::Open,
            failure_score: Some(3.0),
            retry_at_ms: Some(42),
            last_reason: Some("transient Timeout".into()),
        }],
        tracker_items: Some(4),
        link_records: Some(9),
        memory: None,
    });
    // An older daemon's status (no WP-11 fields) still parses.
    let old: StatusResult = serde_json::from_value(json!({
        "mode": "live", "generation": 1, "services": [{"service": "s", "state": "closed"}]
    }))
    .expect("old status");
    assert_eq!(old.tracker_items, None);
    assert_eq!(old.memory, None);
    assert_eq!(old.services[0].failure_score, None);
    round_trip(&JobsListResult {
        jobs: vec![JobSummary {
            job_id: "j".into(),
            job_type: "t".into(),
            lane: Lane::Background,
            status: JobState::Deferred,
            service: Some("x".into()),
            attempts: 2,
            created_ms: 1,
            settled_at_ms: None,
            defer_until_ms: Some(5),
            error: Some("e".into()),
            failure_kind: None,
        }],
    });
    round_trip(&IntakeListResult {
        items: vec![IntakeItem {
            source_kind: "blog".into(),
            source_key: "f".into(),
            item_id: "i".into(),
            title: "t".into(),
            url: "u".into(),
            first_seen_ms: 1,
            day: "2026-10-15".into(),
            outcome: TrackerOutcome::Line,
            score: Some(0.5),
            full_body: Some(true),
        }],
    });
    round_trip(&ViewsListResult { view_ids: vec![] });
    let vr = ViewResult(json!({"rows":[]}));
    assert_eq!(round_trip(&vr), json!({"rows":[]}));
}

#[test]
fn job_status_open_path_is_additive() {
    // Old daemon payload (no open_path) still parses.
    let old: JobStatus =
        serde_json::from_value(json!({"job_id":"j","status":"done","output_paths":[]}))
            .expect("old");
    assert_eq!(old.open_path, None);
    // None is omitted, so old clients see the exact old shape.
    let v = serde_json::to_value(&old).expect("ser");
    assert!(v.get("open_path").is_none());
    // Some round-trips; an old client ignoring unknown fields still parses it.
    let new = JobStatus {
        open_path: Some("k/a.md".into()),
        ..old
    };
    assert_eq!(round_trip(&new)["open_path"], "k/a.md");
}

#[test]
fn job_status_held_note_is_additive() {
    // Old daemon payload (no hold fields) parses; None fields are omitted on the wire.
    let old: JobStatus =
        serde_json::from_value(json!({"job_id":"j","status":"running","output_paths":[]}))
            .expect("old");
    assert_eq!(old.hold, None);
    let v = serde_json::to_value(&old).expect("ser");
    assert!(v.get("held_note").is_none() && v.get("action_label").is_none());
    let held = JobStatus {
        hold: Some(NoteHold {
            path: "k/a.md".into(),
            label: "Refine".into(),
        }),
        ..old
    };
    let v = round_trip(&held);
    // Exactly the old flat shape: two top-level keys, no nested `hold`.
    assert_eq!(
        v,
        json!({"job_id":"j","status":"running","output_paths":[],
               "held_note":"k/a.md","action_label":"Refine"})
    );
    let back: JobStatus = serde_json::from_value(v).expect("back");
    assert_eq!(back, held);
    // Old JSON carrying both fields decodes to `Some`.
    let parsed: JobStatus = serde_json::from_value(json!({"job_id":"j","status":"running",
        "output_paths":[],"held_note":"k/a.md","action_label":"Refine"}))
    .expect("old held");
    assert_eq!(parsed, held);
}

#[test]
fn file_changed_from_is_optional_both_ways() {
    let old =
        r#"{"method":"file_changed","params":{"path":"a.md","kind":"renamed","origin":"daemon"}}"#;
    let n: Notification = serde_json::from_str(old).expect("old json parses");
    assert!(matches!(
        n,
        Notification::FileChanged {
            from: None,
            hash: None,
            ..
        }
    ));
    assert!(!serde_json::to_string(&n).expect("ser").contains("from"));
    let with = Notification::FileChanged {
        id: None,
        path: "b.md".into(),
        kind: ChangeKind::Renamed,
        hash: None,
        origin: Origin::Daemon,
        from: Some("a.md".into()),
        replaced_by: None,
    };
    let v = serde_json::to_value(&with).expect("ser");
    assert_eq!(v["params"]["from"], "a.md");
    assert_eq!(serde_json::from_value::<Notification>(v).expect("de"), with);
}

#[test]
fn notifications_round_trip_and_topic() {
    let all = vec![
        Notification::FileChanged {
            id: None,
            path: "a.md".into(),
            kind: ChangeKind::Modified,
            hash: Some("h".into()),
            origin: Origin::External,
            from: None,
            replaced_by: None,
        },
        Notification::IndexGeneration { n: 3 },
        Notification::JobUpdated(JobStatus {
            job_id: "j".into(),
            status: JobState::Deferred,
            message: None,
            output_paths: vec!["o.md".into()],
            open_path: None,
            hold: None,
        }),
        Notification::ConfigReloaded { errors: vec![] },
        Notification::OpenRequested {
            path: "a.md".into(),
        },
        Notification::ViewsInvalidated {
            view_ids: vec!["v".into()],
        },
    ];
    for n in all {
        let topic = n.topic();
        let v = round_trip(&RpcNotification::new(n));
        assert_eq!(v["method"], serde_json::to_value(topic).expect("topic"));
        assert!(v.get("id").is_none());
    }
}

/// Every marker builds its own variant, whose wire `method` is the marker's `NAME`
/// and is listed in `METHOD_NAMES` (the server's known-method set).
#[test]
fn method_markers_match_the_wire() {
    fn check<M: Method>(p: M::Params, seen: &mut Vec<&'static str>) {
        let req = M::request(p);
        assert_eq!(req.method(), M::NAME);
        let v = round_trip(&RpcRequest::new(RpcId::Num(1), req));
        assert_eq!(v["method"], M::NAME);
        seen.push(M::NAME);
    }
    use methods as m;
    let id = || JobIdParams { job_id: "j".into() };
    let mut seen = Vec::new();
    let s = &mut seen;
    check::<m::Hello>(
        HelloParams {
            protocol_version: 1,
            client: "c".into(),
        },
        s,
    );
    check::<m::Subscribe>(
        SubscribeParams {
            topics: vec![Topic::FileChanged],
        },
        s,
    );
    check::<m::WriteNote>(
        WriteNoteParams {
            id: None,
            path: "a".into(),
            content: "x".into(),
            base_hash: String::new(),
        },
        s,
    );
    check::<m::CreateNote>(
        CreateNoteParams {
            path: "a".into(),
            content: String::new(),
        },
        s,
    );
    check::<m::WriteAttachment>(
        WriteAttachmentParams {
            note_path: "a".into(),
            ext: "png".into(),
            data_base64: String::new(),
        },
        s,
    );
    check::<m::Rename>(
        RenameParams {
            id: None,
            from: "a".into(),
            to: "b".into(),
        },
        s,
    );
    check::<m::Delete>(
        DeleteParams {
            path: "a".into(),
            mode: DeleteMode::Trash,
        },
        s,
    );
    check::<m::DeletePreview>(PathParams { path: "a".into() }, s);
    check::<m::DeleteNote>(
        DeleteNoteParams {
            path: "a".into(),
            attachments: vec![],
            mode: DeleteMode::Trash,
        },
        s,
    );
    check::<m::ActionsList>((), s);
    check::<m::RunAction>(
        RunActionParams {
            action_id: "x".into(),
            params: serde_json::Map::new(),
            context: ActionContext::default(),
        },
        s,
    );
    check::<m::Job>(id(), s);
    check::<m::Cancel>(id(), s);
    check::<m::JobsList>(JobsListParams::default(), s);
    check::<m::JobRetry>(id(), s);
    check::<m::JobsStats>(JobsStatsParams::default(), s);
    check::<m::JobTimeline>(id(), s);
    check::<m::IntakeList>(
        IntakeListParams {
            source_kind: None,
            source_key: None,
            day: None,
            outcome: None,
            limit: None,
            offset: None,
        },
        s,
    );
    check::<m::Uri>(UriParams { url: "u".into() }, s);
    check::<m::Status>((), s);
    check::<m::ViewsList>((), s);
    check::<m::ViewRun>(
        ViewRunParams {
            view_id: "v".into(),
            this_path: None,
            sort: Some(ViewSort {
                property: "p".into(),
                direction: ViewSortDir::Desc,
            }),
            select: Some(ViewSelect {
                property: "file.serial".into(),
                value: "Saga".into(),
            }),
        },
        s,
    );
    check::<m::LookupCurrencies>((), s);
    check::<m::LookupGeocode>(LookupGeocodeParams { query: "q".into() }, s);
    check::<m::LookupModels>(
        LookupModelsParams {
            provider: "p".into(),
        },
        s,
    );
    check::<m::LookupChannels>(LookupChannelsParams { ids: vec![] }, s);
    check::<m::Search>(
        SearchParams {
            query: "q".into(),
            limit: 1,
        },
        s,
    );
    check::<m::TrackerItemSetStatus>(
        TrackerItemSetStatusParams {
            source_kind: "k".into(),
            source_key: "s".into(),
            item_id: "i".into(),
            status: None,
        },
        s,
    );
    check::<m::NoteSetBool>(
        NoteSetBoolParams {
            path: "a".into(),
            key: "k".into(),
            value: true,
        },
        s,
    );
    check::<m::MemoryReport>(MemoryReport::default(), s);
    check::<m::ReconcileIntake>(ReconcileIntakeParams { dry_run: true }, s);
    check::<m::BindNote>(
        BindNoteParams {
            path: "a.md".into(),
        },
        s,
    );
    check::<m::LocateNote>(LocateNoteParams { id: NoteId(1) }, s);
    assert_eq!(seen, METHOD_NAMES);
    // Unit methods carry no `params` key, exactly as the enum always serialised them.
    let v =
        serde_json::to_value(RpcRequest::new(RpcId::Num(2), m::Status::request(()))).expect("v");
    assert_eq!(v, json!({"jsonrpc":"2.0","id":2,"method":"status"}));
}

/// The four results that were ad-hoc `json!` keep their wire shape.
#[test]
fn retyped_results_keep_their_wire_shape() {
    let r = RenameResult {
        from: None,
        rewritten: vec!["a.md".into()],
        skipped_conflicts: vec![],
    };
    assert_eq!(
        round_trip(&r),
        json!({"rewritten": ["a.md"], "skipped_conflicts": []})
    );
    assert_eq!(round_trip(&DeleteResult {}), json!({}));
    let t = SubscribeResult {
        topics: vec![Topic::JobUpdated],
    };
    assert_eq!(round_trip(&t), json!({"topics": ["job_updated"]}));
    assert_eq!(serde_json::to_value(()).expect("unit"), Value::Null);
    assert_eq!(
        (error_codes::REFUSED, error_codes::UNAVAILABLE),
        (1005, 1006)
    );
}

/// Plan round-13 WP-4: `select` round-trips, and its absence keeps the old wire bytes.
#[test]
fn view_run_select_is_optional_and_additive() {
    let old = json!({"view_id": "works#Entries", "this_path": "core/Works.md"});
    let p: ViewRunParams = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(p.select, None);
    assert_eq!(serde_json::to_value(&p).unwrap(), old);
    let with = ViewRunParams {
        select: Some(ViewSelect {
            property: "file.serial".into(),
            value: "Saga".into(),
        }),
        ..p
    };
    let v = round_trip(&with);
    assert_eq!(
        v["select"],
        json!({"property": "file.serial", "value": "Saga"})
    );
}

#[test]
fn trash_enums_keep_their_ledger_and_wire_strings() {
    let loc = [
        (TrashLocation::Vault, "vault"),
        (TrashLocation::System, "system"),
    ];
    for (v, s) in loc {
        assert_eq!(v.as_str(), s);
        assert_eq!(TrashLocation::parse(s), Some(v));
        assert_eq!(serde_json::to_string(&v).unwrap(), format!("\"{s}\""));
        assert_eq!(
            serde_json::from_str::<TrashLocation>(&format!("\"{s}\"")).unwrap(),
            v
        );
    }
    assert_eq!(TrashLocation::default(), TrashLocation::Vault);
    let reasons = [
        (TrashReason::Deleted, "deleted"),
        (TrashReason::Replaced, "replaced"),
        (TrashReason::NoteAttachment, "note-attachment"),
        (TrashReason::Orphan, "orphan"),
        (TrashReason::Seeded, "seeded"),
    ];
    assert_eq!(TrashReason::ALL.len(), reasons.len());
    for (v, s) in reasons {
        assert_eq!(v.as_str(), s);
        assert_eq!(TrashReason::parse(s), Some(v));
        assert_eq!(serde_json::to_string(&v).unwrap(), format!("\"{s}\""));
        assert_eq!(
            serde_json::from_str::<TrashReason>(&format!("\"{s}\"")).unwrap(),
            v
        );
    }
}

#[test]
fn job_timeline_wire_strings() {
    let t = JobTimelineResult {
        job_id: "j".into(),
        job_type: "t".into(),
        status: JobState::Pending,
        created_ms: 1,
        settled_ms: None,
        unavailable: Some(TimelineUnavailable::NotSettled),
        waits: vec![TimelineWait {
            at_ms: 1,
            cause: WaitCause::Breaker,
            blocker: None,
            until_ms: None,
            duration_ms: None,
        }],
        attempts: vec![],
        split: None,
    };
    let v = round_trip(&t);
    assert_eq!(v["status"], "pending");
    assert_eq!(v["unavailable"], "not settled");
    assert_eq!(v["waits"][0]["cause"], "breaker");
    assert!(v.get("available").is_none());
    for (u, s) in [
        (TimelineUnavailable::NotSettled, "not settled"),
        (TimelineUnavailable::PartialCoverage, "partial coverage"),
    ] {
        assert_eq!(serde_json::to_value(u).unwrap(), json!(s));
        assert_eq!(u.to_string(), s);
    }
    assert_eq!(
        WaitCause::parse(WaitCause::Key.as_str()),
        Some(WaitCause::Key)
    );
}

/// Plan note-identity D2: the id rides additively. Old peers' messages (no `id` / `active_id` /
/// result `path` / `from`) parse with the new fields absent, and absent fields are not
/// serialized; present ones are bare integers.
#[test]
fn note_id_fields_are_additive_on_the_wire() {
    let old: WriteNoteParams =
        serde_json::from_value(json!({"path":"a.md","content":"x","base_hash":""})).unwrap();
    assert_eq!(old.id, None);
    assert_eq!(
        round_trip(&old),
        json!({"path":"a.md","content":"x","base_hash":""})
    );
    let new = WriteNoteParams {
        id: Some(NoteId(9)),
        ..old
    };
    assert_eq!(round_trip(&new)["id"], json!(9));
    let r: RenameParams = serde_json::from_value(json!({"from":"a","to":"b"})).unwrap();
    assert_eq!(r.id, None);
    assert_eq!(round_trip(&r), json!({"from":"a","to":"b"}));
    let res: RenameResult =
        serde_json::from_value(json!({"rewritten":[],"skipped_conflicts":[]})).unwrap();
    assert_eq!(
        round_trip(&res),
        json!({"rewritten":[],"skipped_conflicts":[]})
    );
    let w: WriteNoteResult = serde_json::from_value(json!({"written":{"hash":"h"}})).unwrap();
    assert_eq!(
        w,
        WriteNoteResult::Written {
            hash: "h".into(),
            path: None
        }
    );
    assert_eq!(round_trip(&w), json!({"written":{"hash":"h"}}));
    let ctx: ActionContext = serde_json::from_value(json!({"active_path":"a.md"})).unwrap();
    assert_eq!(ctx.active_id, None);
    assert_eq!(round_trip(&ctx), json!({"active_path":"a.md"}));
    let ctx = ActionContext {
        active_id: Some(NoteId(4)),
        ..ctx
    };
    assert_eq!(round_trip(&ctx)["active_id"], json!(4));
    let n: Notification = serde_json::from_value(json!({
        "method":"file_changed",
        "params":{"path":"a.md","kind":"created","origin":"daemon"}
    }))
    .unwrap();
    assert!(matches!(n, Notification::FileChanged { id: None, .. }));
    assert!(round_trip(&n)["params"].get("id").is_none());
}

/// `bind_note` / `locate_note` wire shapes.
#[test]
fn bind_and_locate_note_wire_shapes() {
    let req = Request::BindNote(BindNoteParams {
        path: "a.md".into(),
    });
    assert_eq!(
        round_trip(&req),
        json!({"method":"bind_note","params":{"path":"a.md"}})
    );
    assert_eq!(req.method(), "bind_note");
    assert_eq!(
        round_trip(&BindNoteResult { id: NoteId(3) }),
        json!({"id":3})
    );
    let req = Request::LocateNote(LocateNoteParams { id: NoteId(3) });
    assert_eq!(
        round_trip(&req),
        json!({"method":"locate_note","params":{"id":3}})
    );
    assert!(METHOD_NAMES.contains(&"locate_note") && METHOD_NAMES.contains(&"bind_note"));
    assert_eq!(
        round_trip(&NoteLocation::Live {
            path: "b.md".into()
        }),
        json!({"state":"live","path":"b.md"})
    );
    assert_eq!(
        round_trip(&NoteLocation::Gone {
            replaced_by: Some(NoteId(5))
        }),
        json!({"state":"gone","replaced_by":5})
    );
    assert_eq!(
        round_trip(&NoteLocation::Gone { replaced_by: None }),
        json!({"state":"gone"})
    );
    assert_eq!(
        round_trip(&NoteLocation::Unknown),
        json!({"state":"unknown"})
    );
}

/// D7: the shared constructor's rename pair — destination `Renamed { from }` first, then the
/// source `Deleted` and one `id`; a non-rename is one notification.
#[test]
fn file_changed_builds_the_rename_pair() {
    let got = file_changed(FileChange {
        path: "b.md".into(),
        kind: ChangeKind::Renamed,
        hash: None,
        origin: Origin::External,
        from: Some("a.md".into()),
        replaced_by: None,
        id: Some(NoteId(8)),
    });
    let shape: Vec<_> = got
        .iter()
        .map(|n| match n {
            Notification::FileChanged {
                path,
                kind,
                from,
                id,
                origin,
                ..
            } => (path.as_str(), *kind, from.clone(), *id, *origin),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            (
                "b.md",
                ChangeKind::Renamed,
                Some("a.md".to_owned()),
                Some(NoteId(8)),
                Origin::External
            ),
            (
                "a.md",
                ChangeKind::Deleted,
                None,
                Some(NoteId(8)),
                Origin::External
            ),
        ]
    );
    let one = file_changed(FileChange {
        path: "c.md".into(),
        kind: ChangeKind::Modified,
        hash: Some("h".into()),
        origin: Origin::Daemon,
        from: None,
        replaced_by: None,
        id: Some(NoteId(1)),
    });
    assert_eq!(one.len(), 1);
}
