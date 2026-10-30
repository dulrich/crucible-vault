use super::*;
use cv_protocol::{IntakeListParams, JobsListParams};

fn a(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn parse_accepts_and_rejects() {
    assert_eq!(
        parse(&a(&["jobs", "list", "--status", "failed", "--limit", "5"])),
        Some(Command::JobsList(JobsListParams {
            status: Some("failed".into()),
            limit: Some(5),
            ..JobsListParams::default()
        }))
    );
    assert_eq!(
        parse(&a(&["run", "link-scan", "--note", "a/b.md"])),
        Some(Command::Run {
            action_id: "link-scan".into(),
            note: Some("a/b.md".into())
        })
    );
    assert!(matches!(
        parse(&a(&[
            "intake",
            "list",
            "--day",
            "2026-10-15",
            "--outcome",
            "line"
        ])),
        Some(Command::IntakeList(_))
    ));
    assert_eq!(
        parse(&a(&["intake", "reconcile"])),
        Some(Command::IntakeReconcile { dry_run: false })
    );
    assert_eq!(
        parse(&a(&["intake", "reconcile", "--dry-run"])),
        Some(Command::IntakeReconcile { dry_run: true })
    );
    assert_eq!(
        parse(&a(&["jobs", "retry", "j1"])),
        Some(Command::JobRetry("j1".into()))
    );
    for bad in [
        &["jobs"][..],
        &["jobs", "list", "--limit"],
        &["jobs", "list", "--limit", "x"],
        &["jobs", "list", "--bogus", "1"],
        &["jobs", "retry"],
        &["jobs", "cancel", "a", "b"],
        &["run"],
        &["run", "x", "--note"],
        &["intake", "list", "stray", "x"],
        &["intake", "reconcile", "stray"],
        &["intake", "reconcile", "--dry-run", "x"],
        &["intake", "reconcile", "--dryrun"],
    ] {
        assert_eq!(parse(&a(bad)), None, "{bad:?}");
        assert_eq!(run(&a(bad)), 2, "{bad:?}");
    }
}

#[test]
fn unreachable_daemon_exits_1() {
    let dir = std::env::temp_dir().join(format!("cv-wp11-jobs-{}", std::process::id()));
    let home = dir.to_string_lossy().into_owned();
    let sock = dir.join("none.sock").to_string_lossy().into_owned();
    let config = Config::resolve(|k| match k {
        "HOME" => Some(home.clone()),
        "CV_SOCKET" => Some(sock.clone()),
        _ => None,
    })
    .unwrap();
    for cmd in [
        Command::JobsList(JobsListParams::default()),
        Command::JobRetry("j".into()),
        Command::Run {
            action_id: "x".into(),
            note: None,
        },
    ] {
        assert_eq!(execute(&config, &cmd), 1);
    }
    assert!(
        !config.spool_dir().exists(),
        "nothing spooled for an unreachable daemon"
    );
}

#[test]
fn query_validation() {
    let bad = JobsListParams {
        status: Some("pending".into()),
        ..JobsListParams::default()
    };
    assert!(list_query(&bad, 0).is_err());
    let q = list_query(
        &JobsListParams {
            status: Some("deferred".into()),
            limit: Some(10_000),
            ..JobsListParams::default()
        },
        7,
    )
    .unwrap();
    assert_eq!((q.deferred, q.limit, q.now_ms), (Some(true), MAX_LIMIT, 7));
    assert!(q.recent_first);
    assert!(
        intake_query(&IntakeListParams {
            day: Some("2026-1-5".into()),
            ..IntakeListParams::default()
        })
        .is_err()
    );
    assert!(
        intake_query(&IntakeListParams {
            outcome: Some("maybe".into()),
            ..IntakeListParams::default()
        })
        .is_err()
    );
}
