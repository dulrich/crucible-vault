//! Plan clip filing order: filing runs first, enrichment follows the final path.

use super::*;

/// Plan round-16 WP-5's chain shape (as `config/examples/actions.toml`), with stand-in steps.
const FILING_FIRST: &str = r#"
[[action]]
id = "clip-images"
[[action.steps]]
type = "run_action"
action = "clip-auto-file"

[[action]]
id = "clip-auto-file"
needs = ["active_path"]
[[action.steps]]
type = "auto_file"

[[action]]
id = "file-as-kind"
needs = ["active_path"]
[[action.steps]]
type = "set_props"
[[action.steps]]
type = "run_action"
action = "clip-enrich"

[[action]]
id = "clip-enrich"
needs = ["active_path"]
resources = { note = true, keys = ["clip::enrich"] }
[[action.steps]]
type = "localize"
[[action.steps]]
type = "image_describe"
scope = "note"
"#;

/// Plan round-16 WP-5: filing settles before any localize/describe of the same clip; enrichment
/// runs on the final path in all three outcomes (moved, supersede target, left in place); and
/// enrichment jobs never overlap, so the 2-slot queue keeps a slot for filing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clip_filing_runs_first_and_enrichment_follows_the_final_path_serialized() {
    let f = fixture();
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let live = Arc::new(AtomicUsize::new(0));
    let max_live = Arc::new(AtomicUsize::new(0));
    let (l2, lv, ml) = (Arc::clone(&log), Arc::clone(&live), Arc::clone(&max_live));
    let exec = FnExec(Box::new(move |step, ctx| {
        let path = ctx.context.active_path.clone().unwrap_or_default();
        let name = path.rsplit('/').next().unwrap_or_default().to_owned();
        match (ctx.action.id.as_str(), step.kind) {
            (_, StepKind::RunAction) => crate::steps::execute(step, ctx),
            ("clip-auto-file", _) => {
                l2.lock().unwrap().push(format!("dedup {path}"));
                // a: kind match (filed by the child); b: left in place; c: replaced a target.
                let r = match name.as_str() {
                    "a.md" => ctx.children.spawn_child("file-as-kind", Map::new()),
                    "b.md" => ctx
                        .children
                        .spawn_child_at("clip-enrich", Map::new(), &path),
                    _ => ctx
                        .children
                        .spawn_child_at("clip-enrich", Map::new(), "Notes/target.md"),
                };
                r.map_err(StepError::failed)?;
                Ok(StepOutcome::default())
            }
            ("file-as-kind", _) => {
                let to = format!("Kind/{name}");
                l2.lock().unwrap().push(format!("filed {to}"));
                let mut o = StepOutcome::default();
                o.vars.insert("moved_path".into(), serde_json::json!(to));
                o.vars
                    .insert(steps::MOVED_FROM.into(), serde_json::json!(path));
                Ok(o)
            }
            ("clip-enrich", kind) => {
                let now = lv.fetch_add(1, Ordering::SeqCst) + 1;
                ml.fetch_max(now, Ordering::SeqCst);
                l2.lock().unwrap().push(format!("{} {path}", kind.as_str()));
                std::thread::sleep(Duration::from_millis(40));
                lv.fetch_sub(1, Ordering::SeqCst);
                Ok(StepOutcome::default())
            }
            _ => Ok(StepOutcome::default()),
        }
    }));
    let r = runner(&f, Arc::new(exec), Arc::new(SystemClock), 2, FILING_FIRST);
    let ids: Vec<String> = ["Clippings/a.md", "Clippings/b.md", "Clippings/c.md"]
        .iter()
        .map(|p| enqueue(&r, "clip-images", Some(p), Lane::Background))
        .collect();
    let (_stop_tx, stop) = watch::channel(false);
    let _h = r.spawn_claim_loop(stop);
    for id in &ids {
        assert_eq!(wait_terminal(&f, id).await, DbStatus::Done);
    }
    // Wait for the whole job tree (children settle after their parents).
    let deadline = Instant::now() + Duration::from_secs(10);
    while log
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.starts_with("image_describe"))
        .count()
        < 3
    {
        assert!(Instant::now() < deadline, "{:?}", log.lock().unwrap());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let log = log.lock().unwrap().clone();
    let at = |s: &str| {
        log.iter()
            .position(|l| l == s)
            .unwrap_or_else(|| panic!("{s}: {log:?}"))
    };
    // (2) final paths, localize before describe; never the clip's pre-move or trashed path.
    for (dedup, last_filing, fin) in [
        ("Clippings/a.md", "filed Kind/a.md", "Kind/a.md"),
        ("Clippings/b.md", "dedup Clippings/b.md", "Clippings/b.md"),
        ("Clippings/c.md", "dedup Clippings/c.md", "Notes/target.md"),
    ] {
        // (1) filing (dedup, and the move when filed) precedes the clip's enrichment.
        assert!(at(&format!("dedup {dedup}")) <= at(last_filing));
        assert!(at(last_filing) < at(&format!("localize {fin}")), "{log:?}");
        assert!(at(&format!("localize {fin}")) < at(&format!("image_describe {fin}")));
    }
    assert!(
        !log.iter()
            .any(|l| l == "localize Clippings/a.md" || l == "localize Clippings/c.md")
    );
    // (3) one enrichment at a time.
    assert_eq!(max_live.load(Ordering::SeqCst), 1, "{log:?}");
}
