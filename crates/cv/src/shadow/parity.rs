//! `cv shadow parity`: evaluates the plan's shadow parity criteria P1–P7
//! (`plans/v0-core-daemon.md` → Test Plan "Shadow parity criteria") and returns one aggregate
//! JSON object. Never writes the vault, `state.db` or the companion: the P1 rebuild goes into a
//! fresh base under `$CV_SCRATCH/parity-<ts>/` that is deleted afterwards.
//!
//! Verdicts: `pass | fail | warn | unknown | manual | skipped`. The harness exits 0 regardless of
//! verdicts; only a harness error (unusable scratch, unbuildable index) is an `Err`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use cv_actions::{ConfigSet, StepKind};
use cv_index::{Index, Reader};
use cv_lint::{FrontmatterDoc, JsValue};
use cv_search::{CompanionReader, RemoteEntry};
use cv_state::{JobStatus, ListQuery, StateReader};
use regex::Regex;
use serde_json::{Map, Value, json};

use super::probe::{SystemProbe, proc_kb};
use super::report;
use crate::config::Config;
use crate::steps::search::NotePrep;

/// S2 baselines (`docs/assessment-2026-10-13.md`): markdown files, links, unresolved links.
pub const BASELINE_FILES: u64 = 8_008;
/// S2 link baseline.
pub const BASELINE_LINKS: u64 = 17_913;
/// S2 unresolved-link baseline.
pub const BASELINE_UNRESOLVED: u64 = 5_293;
/// P1 limits.
pub const MAX_BUILD_MS: u128 = 10_000;
/// P1 peak RSS limit (kB).
pub const MAX_RSS_KB: u64 = 64 * 1024;
/// P1 daemon idle PSS limit (kB).
pub const MAX_PSS_KB: u64 = 50 * 1024;
/// P1 drift that warrants a manual cause categorization.
pub const MAX_DRIFT_PCT: f64 = 2.0;
/// P3 pass bar.
pub const MIN_MATCH_RATE: f64 = 0.99;
/// P7: a `running` job claimed longer ago than this is stuck.
pub const STUCK_MS: i64 = 3_600_000;
/// P7: required soak length.
pub const SOAK_MS: i64 = 7 * 86_400_000;
/// Cycle-guard log line (`runner/events.rs`).
pub const CYCLE_GUARD_LINE: &str = "cycle guard: daemon write does not re-trigger";

/// Options (the CLI fills them from flags and `CV_SEARCH_URL`/`CV_SEARCH_VAULT_ID`).
#[derive(Debug, Clone)]
pub struct ParityOptions {
    /// Companion base URL.
    pub search_url: String,
    /// Companion vault id.
    pub vault_id: String,
    /// Soak start (epoch ms).
    pub soak_start_ms: Option<i64>,
    /// Criteria to skip (`P1`…`P7`).
    pub skip: Vec<String>,
    /// Repo roots no scratch/config dir may live under.
    pub repo_roots: Vec<PathBuf>,
    /// "Now".
    pub now_ms: i64,
}

fn verdict(status: &str, metrics: Value, notes: Vec<String>) -> Value {
    json!({ "status": status, "metrics": metrics, "notes": notes })
}

fn drift_pct(actual: u64, baseline: u64) -> f64 {
    let d = (actual as f64 - baseline as f64) / baseline as f64 * 100.0;
    (d * 100.0).round() / 100.0
}

fn rate(agreed: usize, compared: usize) -> Value {
    if compared == 0 {
        Value::Null
    } else {
        json!(agreed as f64 / compared as f64)
    }
}

/// Deletes the parity scratch base on drop.
struct ScratchGuard(PathBuf);
impl Drop for ScratchGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The fresh index's file list (every indexed file, path order).
fn all_files(reader: &Reader) -> Result<Vec<cv_index::FileRow>> {
    let q = reader.query();
    let mut out = Vec::new();
    let mut folders = vec![String::new()];
    while let Some(folder) = folders.pop() {
        out.extend(q.folder_files(&folder)?);
        folders.extend(q.subfolders(&folder)?);
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Reads a vault-relative path from the live vault read-only (`None` when absent).
fn read_vault(vault: &Path, rel: &str) -> std::io::Result<Option<Vec<u8>>> {
    if cv_vault::validate_rel_path(rel).is_err() {
        return Ok(None);
    }
    match std::fs::read(vault.join(rel)) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Runs every criterion. `probe` supplies the systemd/journal facts.
pub fn run(config: &Config, opts: &ParityOptions, probe: &dyn SystemProbe) -> Result<Value> {
    let skip: BTreeSet<String> = opts.skip.iter().map(|s| s.to_ascii_uppercase()).collect();
    let scratch = cv_vault::validate_scratch(&config.scratch, &config.vault, &opts.repo_roots)
        .context("CV_SCRATCH refused for the parity rebuild")?;
    let base_dir = scratch.join(format!("parity-{}", opts.now_ms));
    std::fs::create_dir_all(&base_dir)
        .with_context(|| format!("creating {}", base_dir.display()))?;
    let _guard = ScratchGuard(base_dir.clone());
    let base = base_dir.join("index.db");

    // P1 build (always: P2/P3 read the fresh index).
    let rss_before = proc_kb("/proc/self/status", "VmHWM");
    let t0 = Instant::now();
    let index = Index::create(&config.vault, &base)
        .map_err(|e| anyhow!("{e}"))
        .context("building the parity index")?;
    let build_ms = t0.elapsed().as_millis();
    let rss_after = proc_kb("/proc/self/status", "VmHWM");
    drop(index);
    let reader = Reader::open_current(&base).map_err(|e| anyhow!("{e}"))?;
    let files = all_files(&reader)?;

    let state = StateReader::open(&config.state_db).ok();
    let mut criteria = Map::new();
    let mut put = |k: &str, f: &mut dyn FnMut() -> Result<Value>| -> Result<()> {
        let v = if skip.contains(k) {
            verdict("skipped", json!({}), vec!["skipped by --skip".into()])
        } else {
            f()?
        };
        criteria.insert(k.to_owned(), v);
        Ok(())
    };
    put("P1", &mut || {
        p1(&reader, &files, build_ms, rss_before, rss_after, probe)
    })?;
    put("P2", &mut || p2(&config.vault, &files))?;
    put("P3", &mut || p3(config, opts, &reader, state.as_ref()))?;
    let p45 = || p4_p5(config, state.as_ref());
    let (p4v, p5v) = if skip.contains("P4") && skip.contains("P5") {
        (Value::Null, Value::Null)
    } else {
        p45()?
    };
    put("P4", &mut || Ok(p4v.clone()))?;
    put("P5", &mut || Ok(p5v.clone()))?;
    put("P6", &mut || p6(config, opts, state.as_ref(), probe))?;
    put("P7", &mut || p7(opts, state.as_ref(), probe))?;
    Ok(json!({
        "generated_at": chrono::DateTime::from_timestamp_millis(opts.now_ms)
            .map(|d| d.to_rfc3339())
            .unwrap_or_default(),
        "mode": serde_json::to_value(config.mode)?,
        "criteria": criteria,
    }))
}

fn p1(
    reader: &Reader,
    files: &[cv_index::FileRow],
    build_ms: u128,
    rss_before: Option<u64>,
    rss_after: Option<u64>,
    probe: &dyn SystemProbe,
) -> Result<Value> {
    let q = reader.query();
    let md = files.iter().filter(|f| f.is_md).count() as u64;
    // The S2 baseline (`docs/spikes/core-index/FINDINGS.md:30`) counts vault links only; md links
    // with a URI scheme (`external`) are reported separately, like `unresolved(false)` below.
    let mut links = 0u64;
    let mut external_links = 0u64;
    for f in files {
        for l in q.outgoing(&f.path)? {
            if l.external {
                external_links += 1;
            } else {
                links += 1;
            }
        }
    }
    let unresolved = q.unresolved(false)?.len() as u64;
    let tags = q.tags()?.len() as u64;
    let pss = probe
        .main_pid()
        .and_then(|pid| proc_kb(&format!("/proc/{pid}/smaps_rollup"), "Pss"));
    let mut notes = Vec::new();
    let mut fail = build_ms > MAX_BUILD_MS;
    if rss_after.is_some_and(|r| r > MAX_RSS_KB) {
        fail = true;
    }
    match pss {
        Some(p) if p > MAX_PSS_KB => fail = true,
        Some(_) => {}
        None => notes.push("daemon PSS unknown (unit not running or probe failed)".into()),
    }
    if rss_after.is_none() {
        notes.push("peak RSS unknown (/proc/self/status unreadable)".into());
    }
    notes.push("peak RSS is this process's VmHWM after the build (it includes the harness)".into());
    let drift = json!({
        "files": drift_pct(md, BASELINE_FILES),
        "links": drift_pct(links, BASELINE_LINKS),
        "unresolved": drift_pct(unresolved, BASELINE_UNRESOLVED),
    });
    let drifted = [md, links, unresolved]
        .iter()
        .zip([BASELINE_FILES, BASELINE_LINKS, BASELINE_UNRESOLVED])
        .any(|(&a, b)| drift_pct(a, b).abs() > MAX_DRIFT_PCT);
    if drifted {
        notes.push("drift above 2 %: categorize the cause (manual)".into());
    }
    let status = if fail {
        "fail"
    } else if drifted {
        "warn"
    } else if pss.is_none() || rss_after.is_none() {
        "unknown"
    } else {
        "pass"
    };
    Ok(verdict(
        status,
        json!({
            "build_ms": build_ms,
            "peak_rss_kb_before": rss_before,
            "peak_rss_kb": rss_after,
            "daemon_pss_kb": pss,
            "indexed_files": files.len(),
            "md_files": md,
            "links": links,
            "external_links": external_links,
            "unresolved": unresolved,
            "tags": tags,
            "baseline": {"files": BASELINE_FILES, "links": BASELINE_LINKS, "unresolved": BASELINE_UNRESOLVED},
            "drift_pct": drift,
            "limits": {"build_ms": MAX_BUILD_MS, "peak_rss_kb": MAX_RSS_KB, "daemon_pss_kb": MAX_PSS_KB},
        }),
        notes,
    ))
}

#[derive(Default)]
struct Agreement {
    compared: usize,
    agreed: usize,
}

impl Agreement {
    fn add(&mut self, ok: bool) {
        self.compared += 1;
        if ok {
            self.agreed += 1;
        }
    }
    fn json(&self) -> Value {
        json!({"compared": self.compared, "agreed": self.agreed, "rate": rate(self.agreed, self.compared)})
    }
}

/// The source-id value lint would derive for `text` (stored ids removed first).
fn derived_id(text: &str, key: &str) -> Option<JsValue> {
    let mut d = FrontmatterDoc::new(text);
    d.remove("yt-video-id").ok()?;
    d.remove("post-id").ok()?;
    cv_lint::derive_source_ids(&mut d, &cv_text::PostIdOptions::default()).ok()?;
    d.get(key).ok().flatten()
}

fn p2(vault: &Path, files: &[cv_index::FileRow]) -> Result<Value> {
    let mut wc = Agreement::default();
    let mut yt = Agreement::default();
    let mut post = Agreement::default();
    let mut unreadable = 0usize;
    for f in files.iter().filter(|f| f.is_md && f.has_fm) {
        let Ok(Some(bytes)) = read_vault(vault, &f.path) else {
            unreadable += 1;
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let doc = FrontmatterDoc::new(text.as_ref());
        if let Ok(Some(stored)) = doc.get("word-count") {
            let now = cv_lint::calculate_word_count(&text) as f64;
            let ok = match stored {
                JsValue::Num(n) => (n - now).abs() < f64::EPSILON,
                JsValue::Str(s) => s
                    .trim()
                    .parse::<f64>()
                    .is_ok_and(|n| (n - now).abs() < f64::EPSILON),
                _ => false,
            };
            wc.add(ok);
        }
        for (key, acc) in [("yt-video-id", &mut yt), ("post-id", &mut post)] {
            if let Ok(Some(stored)) = doc.get(key) {
                acc.add(derived_id(&text, key).as_ref() == Some(&stored));
            }
        }
    }
    Ok(verdict(
        "manual",
        json!({
            "word_count": wc.json(),
            "yt_video_id": yt.json(),
            "post_id": post.json(),
            "unreadable": unreadable,
        }),
        vec![
            "the user sets the agreement bar after the first run; lintOnSave is off, so notes edited after linting disagree by design".into(),
            "post-id is derived with default PostIdOptions (no blogs-registry host rules)".into(),
        ],
    ))
}

fn p3(
    config: &Config,
    opts: &ParityOptions,
    reader: &Reader,
    state: Option<&StateReader>,
) -> Result<Value> {
    let mut notes = vec![
        "companion_mutations is structural: shadow parity holds only a CompanionReader (no CompanionWriter), so no /v1/chunks request can be sent".to_owned(),
        "the companion's /v1/paths and /v1/files/state carry no timestamp; note_drift uses the proxy `file mtime >= soak start` (without --soak-start it is null and those mismatches are unclassified)".to_owned(),
        "facet_drift: a mismatched note whose contentHash folds >=1 facet (image description or linked document, filePreparation.ts:145-159,204-216,249-264)".to_owned(),
    ];
    if state.is_none() {
        notes.push(
            "state.db unreadable: image descriptions unavailable, facet hashes may differ".into(),
        );
    }
    let vault = config.vault.clone();
    let prep = NotePrep::new(
        Box::new(move |p| {
            read_vault(&vault, p)
                .map_err(|e| crate::steps::StepError::failed(format!("read: {}", e.kind())))
        }),
        reader,
        Box::new(move |md5| {
            state?
                .image_descriptions_for(&[md5.to_owned()])
                .ok()?
                .into_iter()
                .next()
        }),
    );
    let local = prep
        .local_notes()
        .map_err(|e| anyhow!("preparing notes: {e}"))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building the tokio runtime")?;
    let client = CompanionReader::new(&opts.search_url, &opts.vault_id)
        .map_err(|e| anyhow!("companion client: {e}"))?;
    let remote = rt.block_on(async {
        client.health().await?;
        client.list_paths().await
    });
    let remote = match remote {
        Ok(r) => r,
        Err(e) => {
            let kind = match e {
                cv_search::SearchError::Unavailable { .. } => "unavailable",
                cv_search::SearchError::Schema { .. } => "schema mismatch",
                _ => "error",
            };
            notes.push(format!("companion {kind}: P3 not evaluated"));
            return Ok(verdict(
                "unknown",
                json!({"indexable": local.len(), "companion_mutations": "structural"}),
                notes,
            ));
        }
    };
    let remote: Vec<RemoteEntry> = remote
        .into_iter()
        .map(|p| RemoteEntry {
            fully_embedded: p.chunk_count > 0 && p.embedded_count == p.chunk_count,
            path: p.path,
            content_hash: p.content_hash,
            embedding_space: None,
        })
        .collect();
    let hashes: Vec<(String, String)> = local
        .iter()
        .map(|(p, n)| (p.clone(), n.prepared.content_hash.clone()))
        .collect();
    let cmp = cv_search::compare(&hashes, &remote, None, true);
    let by_path: HashMap<&str, &cv_search::LocalNote> =
        local.iter().map(|(p, n)| (p.as_str(), n)).collect();
    let (mut note_drift, mut facet_drift, mut unexplained, mut unclassified) = (0, 0, 0, 0);
    for p in &cmp.mismatched {
        let Some(n) = by_path.get(p.as_str()) else {
            unexplained += 1;
            continue;
        };
        match opts.soak_start_ms {
            Some(s) if n.mtime >= s => note_drift += 1,
            _ if !n.prepared.hash_facets.is_empty() => facet_drift += 1,
            None => unclassified += 1,
            Some(_) => unexplained += 1,
        }
    }
    let c = cmp.counts();
    let indexable = hashes.len();
    let match_rate = if indexable == 0 {
        None
    } else {
        Some(c.matched as f64 / indexable as f64)
    };
    let status = match match_rate {
        None => "unknown",
        Some(r) if r >= MIN_MATCH_RATE => "pass",
        Some(_) => "fail",
    };
    if unexplained > 0 {
        notes.push(
            "unexplained mismatches are port-defect candidates (never folded into drift)".into(),
        );
    }
    Ok(verdict(
        status,
        json!({
            "indexable": indexable,
            "matched": c.matched,
            "mismatched": c.mismatched,
            "missing_remote": c.missing_remote,
            "missing_local": c.missing_local,
            "match_rate": match_rate,
            "note_drift": opts.soak_start_ms.map(|_| note_drift),
            "facet_drift": facet_drift,
            "unexplained": unexplained,
            "unclassified": unclassified,
            "companion_mutations": "structural",
        }),
        notes,
    ))
}

/// Which criterion a write's job feeds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Feed {
    Materialize,
    Intake,
    Neither,
}

fn frontmatter_equal(a: &str, b: &str) -> bool {
    let entries = |t: &str| {
        let d = FrontmatterDoc::new(t);
        let mut e = d.entries().ok()?;
        e.sort_by(|x, y| x.0.cmp(&y.0));
        Some((e, d.body().to_owned()))
    };
    match (entries(a), entries(b)) {
        (Some((ea, ba)), Some((eb, bb))) => ea == eb && ba.as_bytes() == bb.as_bytes(),
        _ => false,
    }
}

fn p4_p5(config: &Config, state: Option<&StateReader>) -> Result<(Value, Value)> {
    let Some(state) = state else {
        let v = verdict("unknown", json!({}), vec!["state.db unreadable".into()]);
        return Ok((v.clone(), v));
    };
    let mut set = ConfigSet::new();
    crate::runner::load_config_dir(&config.config_dir, &mut set);
    let overlay = config.scratch.join("vault");
    let mut feeds: HashMap<String, Feed> = HashMap::new();
    let mut last: BTreeMap<String, (Feed, bool)> = BTreeMap::new();
    for w in report::all_writes(state, None)? {
        let feed = match w.job_id.as_deref() {
            None => {
                if w.op == "create" {
                    Feed::Intake
                } else {
                    Feed::Neither
                }
            }
            Some(id) => {
                if let Some(f) = feeds.get(id) {
                    *f
                } else {
                    let action = state
                        .job(id)?
                        .and_then(|j| j.job_type.strip_prefix("action:").map(str::to_owned));
                    let kinds: Vec<StepKind> = action
                        .and_then(|a| set.action(&a).cloned())
                        .map(|a| a.steps.iter().map(cv_actions::Step::kind).collect())
                        .unwrap_or_default();
                    let f = if kinds.contains(&StepKind::Materialize) {
                        Feed::Materialize
                    } else if kinds.contains(&StepKind::Capture) {
                        Feed::Intake
                    } else {
                        Feed::Neither
                    };
                    feeds.insert(id.to_owned(), f);
                    f
                }
            }
        };
        if report::outside_overlay(&w.path) {
            continue;
        }
        last.insert(w.path.clone(), (feed, w.after_hash.is_some()));
    }
    let mut p4 = (0usize, 0usize, 0usize);
    let mut p5 = (0usize, 0usize, 0usize);
    for (path, (feed, exists)) in &last {
        if !exists || *feed == Feed::Neither {
            continue;
        }
        let acc = if *feed == Feed::Materialize {
            &mut p4
        } else {
            &mut p5
        };
        acc.0 += 1;
        let (Ok(Some(ours)), Ok(Some(theirs))) =
            (read_vault(&overlay, path), read_vault(&config.vault, path))
        else {
            continue;
        };
        acc.1 += 1;
        let equal = if *feed == Feed::Materialize {
            frontmatter_equal(
                &String::from_utf8_lossy(&ours),
                &String::from_utf8_lossy(&theirs),
            )
        } else {
            ours == theirs
        };
        if equal {
            acc.2 += 1;
        }
    }
    let mk = |(cand, cmp, eq): (usize, usize, usize), note: &str| {
        let status = if cand == 0 || cmp == 0 {
            "unknown"
        } else if eq == cmp {
            "pass"
        } else {
            "fail"
        };
        verdict(
            status,
            json!({"candidates": cand, "compared": cmp, "equal": eq}),
            vec![note.to_owned()],
        )
    };
    Ok((
        mk(
            p4,
            "materialize overlay files vs the live file at the same path: frontmatter as parsed values (key order ignored), body byte for byte",
        ),
        mk(
            p5,
            "URI-intake (direct create) and capture overlay files vs the live file at the same path, byte for byte; re-send clipper URLs via `cv uri` to produce candidates",
        ),
    ))
}

fn count_lines(lines: &[String], re: &Regex) -> usize {
    lines.iter().filter(|l| re.is_match(l)).count()
}

/// Landlock denial lines: a write refused with EACCES, or a Landlock line reporting a denial —
/// not the startup `landlock` status line.
fn landlock_re() -> Result<Regex> {
    Regex::new(r"(?i)permission denied|landlock\W.*(denied|refused|fail)").map_err(Into::into)
}

fn outside(p: &Path, roots: &[&Path]) -> bool {
    let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    roots.iter().all(|r| {
        let r = std::fs::canonicalize(r).unwrap_or_else(|_| r.to_path_buf());
        !canon.starts_with(&r) && !r.starts_with(&canon)
    })
}

fn p6(
    config: &Config,
    opts: &ParityOptions,
    state: Option<&StateReader>,
    probe: &dyn SystemProbe,
) -> Result<Value> {
    let mut notes = Vec::new();
    let outside_rows = match state {
        Some(s) => Some(report::build(s, None)?.outside_overlay),
        None => {
            notes.push("state.db unreadable".to_owned());
            None
        }
    };
    let mut roots: Vec<&Path> = vec![config.vault.as_path(), config.protected_vault.as_path()];
    roots.extend(opts.repo_roots.iter().map(PathBuf::as_path));
    let scratch_ok = outside(&config.scratch, &roots);
    let config_ok = config.config_dir.is_dir()
        && outside(&config.config_dir, &roots)
        && !config
            .config_dir
            .ancestors()
            .any(|a| a.join(".git").symlink_metadata().is_ok());
    let since = opts
        .soak_start_ms
        .or_else(|| probe.active_since_ms())
        .unwrap_or(0);
    let landlock = match probe.journal_lines(since) {
        Some(lines) => Some(count_lines(&lines, &landlock_re()?)),
        None => {
            notes.push("journal unavailable: Landlock denials unknown".into());
            None
        }
    };
    let status = if outside_rows.is_some_and(|n| n > 0)
        || !scratch_ok
        || !config_ok
        || landlock.is_some_and(|n| n > 0)
    {
        "fail"
    } else if outside_rows.is_none() || landlock.is_none() {
        "unknown"
    } else {
        "pass"
    };
    Ok(verdict(
        status,
        json!({
            "outside_overlay_rows": outside_rows,
            "scratch_outside_vault_and_repos": scratch_ok,
            "config_dir_outside_vault_and_repos": config_ok,
            "landlock_denials": landlock,
        }),
        notes,
    ))
}

fn p7(opts: &ParityOptions, state: Option<&StateReader>, probe: &dyn SystemProbe) -> Result<Value> {
    let mut notes = Vec::new();
    let active_since = probe.active_since_ms();
    let restarts = probe.n_restarts();
    let start = opts.soak_start_ms.or(active_since);
    let journal = start.and_then(|s| probe.journal_lines(s));
    let (panics, cycles) = match &journal {
        Some(lines) => (
            Some(count_lines(lines, &Regex::new("panicked at")?)),
            Some(
                lines
                    .iter()
                    .filter(|l| l.contains(CYCLE_GUARD_LINE))
                    .count(),
            ),
        ),
        None => (None, None),
    };
    let stuck = match state {
        Some(s) => Some(
            s.list(&ListQuery {
                status: Some(JobStatus::Running),
                ..ListQuery::default()
            })?
            .iter()
            .filter(|j| j.claimed_at_ms.is_some_and(|c| opts.now_ms - c > STUCK_MS))
            .count(),
        ),
        None => None,
    };
    let soak_days =
        start.map(|s| ((opts.now_ms - s) as f64 / 86_400_000.0 * 100.0).round() / 100.0);
    let soak_ok = start.map(|s| opts.now_ms - s >= SOAK_MS);
    if active_since.is_some_and(|a| opts.soak_start_ms.is_some_and(|s| a > s)) {
        notes.push("the unit became active after the soak start (a restart or manual stop)".into());
    }
    let bad = restarts.is_some_and(|n| n > 0)
        || panics.is_some_and(|n| n > 0)
        || stuck.is_some_and(|n| n > 0)
        || soak_ok == Some(false);
    let unknown = restarts.is_none() || panics.is_none() || stuck.is_none() || soak_ok.is_none();
    if cycles.is_some_and(|n| n > 0) {
        notes.push("cycle-guard trips: explain each (manual)".into());
    }
    let status = if bad {
        "fail"
    } else if unknown {
        "unknown"
    } else if cycles.is_some_and(|n| n > 0) {
        "warn"
    } else {
        "pass"
    };
    Ok(verdict(
        status,
        json!({
            "active_since_ms": active_since,
            "soak_days": soak_days,
            "n_restarts": restarts,
            "panics": panics,
            "cycle_guard_trips": cycles,
            "stuck_running_jobs": stuck,
        }),
        notes,
    ))
}
