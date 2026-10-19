//! Trackers (plan `v0-automation-pipelines` WP-7/WP-8): the source-kind-generic tracker core plus
//! one submodule per source family.
//!
//! Core pieces (WP-8's blog tracker builds on them):
//!
//! - [`SeenSet`]: `tracker_items` ids + `ignored_ids` + an index prop (`yt-video-id`, `post-id`,
//!   …), seeded once on the kind's first run (rows persisted as `Skipped`) so nothing the vault
//!   or Crucible's intake already surfaced produces a line;
//! - [`fetch_bounded`]: the per-run fetch loop, at most [`MAX_IN_FLIGHT`] requests in flight;
//! - [`emit_line`]: one daily intake line through the `tracker-intake` capture preset, with
//!   item-level replay (audit E2): recorded → skip; URL already in the daily note → record only
//!   (crash between write and record); else write, then record.
//!
//! Crucible reference: `src/orchestration/workflows/FeedTrackerWorkflow.ts` and
//! `utils/feedIntake.ts` (seen-set + intake notes). Dropped as Obsidian-induced: the per-run
//! intake notes under `_crucible/orchestration/*/new-*` (lines go to the daily note instead, plan
//! Decisions locked "Tracker intake → daily lines") and the metadata-cache vault scan (the index
//! prop query replaces it).

pub mod blogs;
pub mod youtube;

use std::collections::{BTreeSet, HashSet};
use std::future::Future;

use cv_actions::pipelines::IntakeMode;
use cv_actions::{ConfigSet, Step};
use cv_state::{TrackerItem, TrackerOutcome};
use serde_json::{Value, json};
use tokio::task::JoinSet;

use super::web::{block_on, local_day};
use super::{StepCtx, StepError, capture, materialize};

/// Fetches in flight per run.
pub const MAX_IN_FLIGHT: usize = 4;
/// The `[[capture]]` preset intake lines go through (`captures.toml`).
pub const INTAKE_PRESET: &str = "tracker-intake";
/// The intake line format when the preset is not configured.
pub const INTAKE_FORMAT: &str = "- [ ] [{{vars.title}}]({{vars.url}}) — {{vars.source}}";
/// The daily-note section intake lines go into when the preset is not configured.
pub const INTAKE_SECTION: &str = "# Intake";

fn state_err(e: cv_state::StateError) -> StepError {
    StepError::failed(format!("state: {e}"))
}

fn lock<'a>(ctx: &StepCtx<'a>) -> std::sync::MutexGuard<'a, cv_state::StateDb> {
    ctx.state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Wall-clock now in ms.
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// One tracked item about to be recorded / surfaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// `youtube` | `blog`.
    pub source_kind: String,
    /// Channel id / feed URL.
    pub source_key: String,
    /// Display name of the source (`{{vars.source}}`).
    pub source_name: String,
    /// Video id / post id.
    pub item_id: String,
    /// Item title (raw; escaped for the line).
    pub title: String,
    /// Item URL.
    pub url: String,
    /// Duration in seconds (YouTube), when known.
    pub duration_s: Option<i64>,
    /// Body word count (blogs with a full body), when known.
    pub word_count: Option<i64>,
}

/// Records `c` with `outcome` (first sighting wins). Returns whether it was newly inserted.
pub fn record(
    ctx: &StepCtx<'_>,
    c: &Candidate,
    outcome: TrackerOutcome,
) -> Result<bool, StepError> {
    record_with(ctx, c, outcome, None, None, None)
}

/// [`record`] with the blog automatable evidence (`full_body`, WP-8), the importance `score`
/// (WP-10) and a captured note's vault-relative path (`Note` outcomes, round-5 WP-11).
pub(crate) fn record_with(
    ctx: &StepCtx<'_>,
    c: &Candidate,
    outcome: TrackerOutcome,
    full_body: Option<bool>,
    score: Option<f64>,
    note_path: Option<String>,
) -> Result<bool, StepError> {
    lock(ctx)
        .insert_tracker_item(&TrackerItem {
            source_kind: c.source_kind.clone(),
            source_key: c.source_key.clone(),
            item_id: c.item_id.clone(),
            title: c.title.clone(),
            url: c.url.clone(),
            first_seen_ms: now_ms(),
            day: local_day(),
            outcome,
            score,
            full_body,
            note_path,
            source_name: Some(c.source_name.clone()).filter(|n| !n.is_empty()),
            duration_s: c.duration_s,
            word_count: c.word_count,
        })
        .map_err(state_err)
}

/// Name self-heal (plan `intake-view-source-length` WP-1): fills `source_name` on every row of
/// each configured `(source_key, source_name)` of `kind` where it is still NULL (rows recorded
/// before state.db v7). Runs for every configured source on every tracker run — independent of
/// fetch results and fresh items — and never counts as a new item. The store raises the
/// `tracker_items` change signal only when rows changed.
pub(crate) fn heal_source_names<'k>(
    ctx: &StepCtx<'_>,
    kind: &str,
    sources: impl IntoIterator<Item = (&'k str, &'k str)>,
) -> Result<usize, StepError> {
    let mut db = lock(ctx);
    let mut n = 0;
    for (key, name) in sources {
        if key.is_empty() || name.is_empty() {
            continue;
        }
        n += db
            .fill_tracker_source_name(kind, key, name)
            .map_err(state_err)?;
    }
    Ok(n)
}

/// The tracker seen-set of one source kind.
pub struct SeenSet {
    ids: HashSet<String>,
    prop: String,
    index: Option<cv_index::Reader>,
}

impl SeenSet {
    /// Loads the seen-set of `kind`: every `tracker_items` id + every `ignored_ids` id of `kind`,
    /// plus index notes whose `prop` equals an id (checked per candidate). On the kind's first run
    /// (no `tracker_items` rows) `seed` supplies the ids the vault / Crucible already surfaced;
    /// they are persisted as `Skipped` rows (`source_key = "seed"`), so the seeding happens once.
    pub fn load(
        ctx: &StepCtx<'_>,
        kind: &str,
        prop: &str,
        seed: impl FnOnce(&StepCtx<'_>) -> BTreeSet<String>,
    ) -> Result<Self, StepError> {
        let first = !lock(ctx).has_tracker_items(kind).map_err(state_err)?;
        if first {
            let seeds = seed(ctx);
            tracing::info!(
                kind,
                seeded = seeds.len(),
                "tracker first run: seen-set seeded"
            );
            for id in seeds {
                let c = Candidate {
                    source_kind: kind.to_owned(),
                    source_key: "seed".into(),
                    source_name: String::new(),
                    item_id: id,
                    title: String::new(),
                    url: String::new(),
                    duration_s: None,
                    word_count: None,
                };
                record(ctx, &c, TrackerOutcome::Skipped)?;
            }
        }
        let (mut ids, ignored) = {
            let s = lock(ctx);
            (
                s.tracker_item_ids(kind).map_err(state_err)?,
                s.ignored_ids(kind).map_err(state_err)?,
            )
        };
        ids.extend(ignored);
        let index = match cv_index::Reader::open_current(ctx.index_db) {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!(error = %e, "tracker: index unavailable; seen-set without props");
                None
            }
        };
        Ok(Self {
            ids: ids.into_iter().collect(),
            prop: prop.to_owned(),
            index,
        })
    }

    /// Whether `id` was already seen (state, ignored, or a vault note carrying the prop).
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        if self.ids.contains(id) {
            return true;
        }
        self.index.as_ref().is_some_and(|r| {
            r.query()
                .files_with_prop(&self.prop, &json!(id))
                .is_ok_and(|rows| !rows.is_empty())
        })
    }

    /// Marks `id` seen for the rest of the run.
    pub fn insert(&mut self, id: &str) {
        self.ids.insert(id.to_owned());
    }
}

/// Runs `f` over `inputs` with at most [`MAX_IN_FLIGHT`] futures in flight on the current
/// runtime; results come back in input order.
pub(crate) fn fetch_bounded<I, T, F, Fut>(inputs: Vec<I>, f: F) -> Result<Vec<T>, StepError>
where
    F: Fn(I) -> Fut,
    Fut: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let n = inputs.len();
    block_on(async move {
        let mut set = JoinSet::new();
        let mut out: Vec<Option<T>> = (0..n).map(|_| None).collect();
        let mut it = inputs.into_iter().enumerate();
        loop {
            while set.len() < MAX_IN_FLIGHT {
                let Some((i, input)) = it.next() else { break };
                let fut = f(input);
                set.spawn(async move { (i, fut.await) });
            }
            let Some(r) = set.join_next().await else {
                break;
            };
            let (i, v) = r.map_err(|e| StepError::failed(format!("fetch task: {e}")))?;
            out[i] = Some(v);
        }
        out.into_iter()
            .map(|v| v.ok_or_else(|| StepError::failed("fetch task lost")))
            .collect()
    })?
}

/// The `capture` step that writes intake lines: the `tracker-intake` preset when
/// `captures.toml` defines it, else the same settings inline.
fn intake_step(config: &ConfigSet, cancelled: bool) -> Result<Step, StepError> {
    let preset = match config.table("captures.toml").get("capture") {
        Some(toml::Value::Array(a)) => a
            .iter()
            .filter_map(toml::Value::as_table)
            .find(|e| e.get("name").and_then(toml::Value::as_str) == Some(INTAKE_PRESET))
            .cloned(),
        _ => None,
    };
    let has_preset = preset.is_some();
    let mut t = toml::Table::new();
    if cancelled {
        // Round-8 WP-1: an ignored short's line is a cancelled task (`- [-] …`).
        let format = preset
            .as_ref()
            .and_then(|p| p.get("format"))
            .and_then(toml::Value::as_str)
            .unwrap_or(INTAKE_FORMAT);
        t.insert("format".into(), cancelled_format(format).into());
    }
    t.insert("type".into(), "capture".into());
    if has_preset {
        t.insert("capture".into(), INTAKE_PRESET.into());
    } else {
        t.insert("target".into(), "daily".into());
        t.insert("mode".into(), "section".into());
        t.insert("section".into(), INTAKE_SECTION.into());
        t.insert("format".into(), INTAKE_FORMAT.into());
    }
    Step::from_table(t).map_err(StepError::failed)
}

/// `format` with its first open task box (`[ ]`) turned cancelled (`[-]`).
fn cancelled_format(format: &str) -> String {
    format.replacen("[ ]", "[-]", 1)
}

/// Today's daily note path (`captures.toml` `[periods.day]`).
fn daily_path(ctx: &StepCtx<'_>) -> Result<String, StepError> {
    let per = materialize::period(ctx.config, "day", None).map_err(StepError::failed)?;
    per.note_path(materialize::now().date())
}

/// What [`emit_line`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emitted {
    /// Already recorded: nothing written.
    AlreadyRecorded,
    /// The URL was already in the daily note (an interrupted run): recorded, nothing written.
    Recovered,
    /// The line was written and recorded.
    Written,
    /// `[intake] mode = "view"`: recorded only, nothing written.
    Recorded,
}

/// Writes one intake line for `c` into today's daily note via the `tracker-intake` capture and
/// records it as `Line`, in the replay-safe order (audit E2 pin): (a) recorded → skip; (b) the
/// daily note already contains `c.url` → record, skip; (c) write, then record. Written paths are
/// appended to `out_paths`.
pub fn emit_line(
    ctx: &mut StepCtx<'_>,
    c: &Candidate,
    out_paths: &mut Vec<String>,
) -> Result<Emitted, StepError> {
    emit_line_with(ctx, c, None, out_paths)
}

/// [`emit_line`] for an ignored item (round-8 WP-1): recorded as `Ignored` with task status `-`
/// (cancelled); in `[intake] mode = "lines"` its line is written as `- [-] …`. Never scored.
pub fn emit_cancelled(
    ctx: &mut StepCtx<'_>,
    c: &Candidate,
    out_paths: &mut Vec<String>,
) -> Result<Emitted, StepError> {
    emit_inner(ctx, c, None, true, out_paths)
}

/// [`emit_line`] recording the blog automatable evidence `full_body` on the row (WP-8).
pub(crate) fn emit_line_with(
    ctx: &mut StepCtx<'_>,
    c: &Candidate,
    full_body: Option<bool>,
    out_paths: &mut Vec<String>,
) -> Result<Emitted, StepError> {
    emit_inner(ctx, c, full_body, false, out_paths)
}

/// Records `c` as `Ignored` with status `-` (cancelled) or as `Line`.
fn record_emitted(
    ctx: &StepCtx<'_>,
    c: &Candidate,
    cancelled: bool,
    full_body: Option<bool>,
    score: Option<f64>,
) -> Result<(), StepError> {
    if !cancelled {
        record_with(ctx, c, TrackerOutcome::Line, full_body, score, None)?;
        return Ok(());
    }
    if record_with(ctx, c, TrackerOutcome::Ignored, full_body, None, None)? {
        lock(ctx)
            .set_tracker_item_status(&c.source_kind, &c.source_key, &c.item_id, Some('-'))
            .map_err(state_err)?;
    }
    Ok(())
}

fn emit_inner(
    ctx: &mut StepCtx<'_>,
    c: &Candidate,
    full_body: Option<bool>,
    cancelled: bool,
    out_paths: &mut Vec<String>,
) -> Result<Emitted, StepError> {
    let known = lock(ctx)
        .tracker_item(&c.source_kind, &c.source_key, &c.item_id)
        .map_err(state_err)?
        .is_some();
    if known {
        return Ok(Emitted::AlreadyRecorded);
    }
    if ctx.config.pipelines().intake.mode == IntakeMode::View {
        // `[intake] mode = "view"`: record only; the `daily-intake` view surfaces the item.
        let score = if cancelled { None } else { score_item(ctx, c) };
        record_emitted(ctx, c, cancelled, full_body, score)?;
        return Ok(Emitted::Recorded);
    }
    let daily = daily_path(ctx)?;
    if let Some((bytes, _)) = ctx.read(&daily)?
        && !c.url.is_empty()
        && String::from_utf8_lossy(&bytes).contains(&c.url)
    {
        record_emitted(ctx, c, cancelled, full_body, None)?;
        return Ok(Emitted::Recovered);
    }
    let score = if cancelled { None } else { score_item(ctx, c) };
    let step = intake_step(ctx.config, cancelled)?;
    let mut vars = ctx.vars.clone();
    vars.insert(
        "title".into(),
        Value::String(cv_ingest::tracker::line_title(&c.title)),
    );
    vars.insert("url".into(), Value::String(c.url.clone()));
    vars.insert("source".into(), Value::String(c.source_name.clone()));
    vars.insert(
        "score".into(),
        Value::String(score.map_or_else(String::new, score_text)),
    );
    let mut sub = StepCtx {
        job_id: ctx.job_id,
        action: ctx.action,
        step_index: ctx.step_index,
        params: ctx.params,
        context: ctx.context,
        vars: &vars,
        config: ctx.config,
        vault_root: ctx.vault_root,
        index_db: ctx.index_db,
        search: ctx.search,
        state: ctx.state,
        writer: ctx.writer.clone(),
        children: ctx.children,
        written: Vec::new(),
    };
    let o = capture::execute(&step, &mut sub)?;
    let written = std::mem::take(&mut sub.written);
    ctx.written.extend(written);
    out_paths.extend(o.output_paths);
    #[cfg(test)]
    if hook::fail_now() {
        return Err(StepError::failed("injected failure after line write"));
    }
    record_emitted(ctx, c, cancelled, full_body, score)?;
    Ok(Emitted::Written)
}

/// `{{vars.score}}` text: integral scores without a fraction.
fn score_text(s: f64) -> String {
    if s.fract() == 0.0 && s.abs() < 1e15 {
        format!("{}", s as i64)
    } else {
        format!("{s}")
    }
}

/// Scores one **new** item with the `[scorer]` of `pipelines.toml` (WP-10). Never fails: any
/// scorer error (transport, non-2xx, bad body, timeout) is one `warn!` and `None`, so intake
/// proceeds unscored (invariant: a scorer failure never blocks intake; no breaker feed).
pub(crate) fn score_item(ctx: &StepCtx<'_>, c: &Candidate) -> Option<f64> {
    use cv_providers::web::scorer::{ImportanceScorer, ScoreItem, StubReason};
    static MISSING_MODEL_WARNED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    let cfg = &ctx.config.pipelines().scorer;
    let (scorer, reason) = ImportanceScorer::from_config(cfg.url.as_deref(), cfg.model.as_deref());
    if reason == Some(StubReason::MissingModel)
        && !MISSING_MODEL_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        tracing::warn!("scorer: [scorer] url is set without `model`; using the stub scorer");
    }
    if scorer == ImportanceScorer::Stub {
        return None;
    }
    let item = ScoreItem {
        kind: &c.source_kind,
        source_name: &c.source_name,
        title: &c.title,
        url: &c.url,
    };
    let client = match super::web::client() {
        Ok(cl) => cl,
        Err(e) => {
            tracing::warn!(error = %e, "scorer: no http client; item unscored");
            return None;
        }
    };
    match block_on(scorer.score(client, &item)) {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, item = %c.item_id, "scorer failed; item unscored");
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, item = %c.item_id, "scorer failed; item unscored");
            None
        }
    }
}

/// Every `.md` file under `<vault_root>/<rel>` (recursive, read-only, no symlinks), as
/// vault-relative paths.
pub(crate) fn md_files_under(ctx: &StepCtx<'_>, rel: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![rel.trim_matches('/').to_owned()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(ctx.vault_root.join(&dir)) else {
            continue;
        };
        for e in rd.filter_map(Result::ok) {
            let Ok(name) = e.file_name().into_string() else {
                continue;
            };
            let Ok(ft) = e.file_type() else { continue };
            let p = format!("{dir}/{name}");
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() && name.ends_with(".md") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Test hook: fail the run right after the n-th line write (before its record).
#[cfg(test)]
pub(crate) mod hook {
    use std::cell::Cell;

    thread_local! {
        static LEFT: Cell<Option<usize>> = const { Cell::new(None) };
    }

    /// Fail after `n` more line writes (`None` = never).
    pub(crate) fn fail_after(n: Option<usize>) {
        LEFT.with(|l| l.set(n));
    }

    pub(super) fn fail_now() -> bool {
        LEFT.with(|l| match l.get() {
            Some(1) => {
                l.set(None);
                true
            }
            Some(n) => {
                l.set(Some(n.saturating_sub(1)));
                false
            }
            None => false,
        })
    }
}
