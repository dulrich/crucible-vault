use super::*;
use cv_protocol::JobsStatsResult;
use cv_state::{AttemptTime, JobStatus, JobTelemetry, StepTime, WaitCause, WaitEvent};

fn a(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn parse_stats_and_show() {
    assert_eq!(
        parse(&a(&["jobs", "stats"])),
        Some(Command::JobsStats {
            job_type: None,
            since: None
        })
    );
    assert_eq!(
        parse(&a(&["jobs", "stats", "--type", "t", "--since", "90m"])),
        Some(Command::JobsStats {
            job_type: Some("t".into()),
            since: Some(Since::Ago(90 * 60_000))
        })
    );
    assert_eq!(parse_since("2h"), Some(Since::Ago(7_200_000)));
    assert_eq!(parse_since("3d"), Some(Since::Ago(3 * 86_400_000)));
    assert_eq!(parse_since("1970-01-02"), Some(Since::At(86_400_000)));
    assert_eq!(
        parse_since("2024-02-29"),
        Some(Since::At(1_709_164_800_000))
    );
    assert_eq!(Since::Ago(1_000).resolve(5_000), 4_000);
    assert_eq!(Since::At(7).resolve(5_000), 7);
    assert_eq!(
        parse(&a(&["jobs", "show", "j1"])),
        Some(Command::JobShow("j1".into()))
    );
    for bad in [
        "",
        "m",
        "5",
        "5x",
        "-5m",
        "1.5h",
        "2023-02-29",
        "2023-13-01",
        "1969-12-31",
        "2023-1-01",
        "99999999999999999999d",
    ] {
        assert_eq!(parse_since(bad), None, "{bad:?}");
    }
    for bad in [
        &["jobs", "stats", "--since", "soon"][..],
        &["jobs", "stats", "--since"],
        &["jobs", "stats", "--bogus", "1"],
        &["jobs", "show"],
        &["jobs", "show", "a", "b"],
    ] {
        assert_eq!(parse(&a(bad)), None, "{bad:?}");
        assert_eq!(run(&a(bad)), 2, "{bad:?}");
    }
}

fn tel(partial: bool) -> JobTelemetry {
    JobTelemetry {
        id: "job-1".into(),
        job_type: "synth".into(),
        status: JobStatus::Done,
        created_ms: 1_000,
        settled_ms: Some(9_000),
        partial,
        attempts: vec![
            AttemptTime {
                attempt: 1,
                claimed_ms: 3_000,
                settled_ms: Some(4_000),
            },
            AttemptTime {
                attempt: 2,
                claimed_ms: 6_000,
                settled_ms: Some(9_000),
            },
        ],
        waits: vec![WaitEvent {
            at_ms: 1_000,
            cause: WaitCause::Key,
            blocker: Some("holder".into()),
            until_ms: None,
        }],
        steps: vec![
            StepTime {
                attempt: 1,
                step_index: 0,
                step_kind: "guard".into(),
                start_ms: 3_000,
                end_ms: Some(3_500),
            },
            StepTime {
                attempt: 2,
                step_index: 0,
                step_kind: "shell".into(),
                start_ms: 6_000,
                end_ms: Some(8_400),
            },
        ],
    }
}

#[test]
fn show_renders_a_timeline_and_never_a_fabricated_zero_for_partial_jobs() {
    let full = render_timeline(&job_timeline(&tel(false)));
    println!("{full}");
    assert!(full.contains("wait key holder  2.0s"), "{full}");
    assert!(full.contains("step 0 shell  2.4s"), "{full}");
    assert!(full.contains("bucket split: total 8.0s"), "{full}");
    assert!(!full.contains("unavailable"), "{full}");
    let partial = render_timeline(&job_timeline(&tel(true)));
    println!("{partial}");
    assert!(
        partial.contains("bucket split: unavailable (partial coverage)"),
        "{partial}"
    );
    assert!(
        partial.contains("wait key holder  unavailable"),
        "{partial}"
    );
    let mut open = tel(false);
    open.settled_ms = None;
    let open = render_timeline(&job_timeline(&open));
    assert!(
        open.contains("bucket split: unavailable (not settled)"),
        "{open}"
    );
}

#[test]
fn stats_output_carries_aggregates_only() {
    // Seed text the renderer must never see: a sentinel in a job id-adjacent field cannot
    // reach `JobTelemetry` (it holds no params), so assert the rendered block directly.
    let jobs = [tel(false), tel(true)];
    let r = jobs_stats_result(&cv_state::job_stats(&jobs));
    let text = render_stats(&r);
    println!("{text}");
    assert!(text.starts_with("synth  n=1  partial=1\n"), "{text}");
    assert!(
        text.contains("  total     p50 8.0s  p95 8.0s  max 8.0s"),
        "{text}"
    );
    assert!(text.contains("  blocker   key holder  2.0s"), "{text}");
    assert!(text.contains("partial_total=1"), "{text}");
    assert!(!text.contains("job-1"), "no job ids in the aggregate view");
    assert_eq!(
        render_stats(&JobsStatsResult {
            types: vec![],
            partial_total: 0
        }),
        "no settled jobs\n"
    );
}
