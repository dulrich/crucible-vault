//! Plan sources-publication-model WP-4: a hot reload that changes `sources.toml` enqueues the
//! sources-only reconcile once; nothing else does.

use super::*;

const ACTIONS: &str = "[[action]]\nid = \"intake-reconcile\"\nresources = { note = false, keys = [\"tracker::reconcile\"] }\n[[action.steps]]\ntype = \"intake_reconcile\"\n";
const NO_ACTION: &str = "[[action]]\nid = \"other\"\n[[action.steps]]\ntype = \"set_props\"\n";
const ONE: &str = "[[publication]]\nname = \"One\"\nfrom = [\"a@one.example\"]\n";
const TWO: &str =
    "[[publication]]\nname = \"One\"\nfrom = [\"a@one.example\", \"b@one.example\"]\n";

fn queued(f: &Fixture) -> Vec<cv_state::Job> {
    f.state
        .lock()
        .unwrap()
        .list(&cv_state::ListQuery {
            job_type: Some("action:intake-reconcile".into()),
            ..cv_state::ListQuery::default()
        })
        .unwrap()
}

fn put(r: &Runner, name: &str, text: &str) {
    let dir = r.inner().config_dir.clone();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), text).unwrap();
}

fn setup(actions: &str) -> (Fixture, Runner) {
    let f = fixture();
    let r = runner(
        &f,
        Arc::new(FnExec(Box::new(|_, _| Ok(StepOutcome::default())))),
        Arc::new(SystemClock),
        1,
        actions,
    );
    put(&r, "actions.toml", actions);
    // Start-up load: never enqueues.
    r.reload_config();
    assert!(queued(&f).is_empty());
    (f, r)
}

#[test]
fn a_changed_sources_file_enqueues_one_sources_only_job_and_a_second_save_is_coalesced() {
    let (f, r) = setup(ACTIONS);
    put(&r, "sources.toml", ONE);
    assert!(r.reload_config_observed().is_empty());
    let jobs = queued(&f);
    assert_eq!(jobs.len(), 1);
    assert_eq!(
        jobs[0].dedupe_key.as_deref(),
        Some("action:intake-reconcile::intake-reconcile:sources")
    );
    assert_eq!(jobs[0].lane, Lane::Background);
    assert_eq!(jobs[0].params["params"]["only"], "sources");
    assert_eq!(jobs[0].resource_keys[0], "intake::identity");
    assert!(
        jobs[0]
            .resource_keys
            .contains(&"tracker::reconcile".to_owned())
    );
    // Another change while the first is pending leaves exactly one.
    put(&r, "sources.toml", TWO);
    r.reload_config_observed();
    assert_eq!(queued(&f).len(), 1);
}

#[test]
fn an_unchanged_or_start_up_reload_and_a_broken_sources_file_enqueue_nothing() {
    let (f, r) = setup(ACTIONS);
    // The start-up path loads a new sources file without enqueueing.
    put(&r, "sources.toml", ONE);
    r.reload_config();
    // A watcher tick with the same typed sources is not a change.
    r.reload_config_observed();
    assert!(queued(&f).is_empty());
    // A file that fails to parse keeps the previous sources: not a change.
    put(&r, "sources.toml", "[[publication]\nname = ");
    assert!(!r.reload_config_observed().is_empty());
    assert!(queued(&f).is_empty());
}

#[test]
fn a_config_without_the_action_enqueues_nothing() {
    let (f, r) = setup(NO_ACTION);
    put(&r, "sources.toml", ONE);
    r.reload_config_observed();
    assert!(queued(&f).is_empty());
}
