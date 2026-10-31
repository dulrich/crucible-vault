//! Step module `intake_enrich` (plan `intake-auto-enrich` WP-3; measurements and the design in
//! `docs/spikes/auto-intake-llm/FINDINGS.md`, decision D29): one LLM structure + cleanup pass over
//! an intake note, applied by `cv_ingest::enrich` — a collapsed top block, chapter headings in
//! heading-less bodies, and **one folded `[!removed]-` callout** for what the cleanup marks. The
//! model never rewrites text and nothing is deleted.
//!
//! | key | values | default |
//! |---|---|---|
//! | `scope` | `"note"` (the run's active note) \| `"backfill"` (enqueue `intake-enrich` per unenriched intake note) | `"note"` |
//! | `force` | re-run on a note that already carries `enriched`; a job param `force` wins | `false` |
//! | `agent` | the `agents.toml` id returning the JSON answer | `"intake-enrich"` |
//! | `limit` | backfill only: notes enqueued per run, newest first (> 0) | `20` |
//!
//! **Note scope**, in order: read the note and its CAS hash; a non-empty `enriched` frontmatter
//! value skips (no model call) unless forced; call the agent through the `agent` step's provider
//! path (the same failure mapping: transport/5xx/429 → `Deferred`, missing key → `Failed{NoApiKey}`,
//! truncation → `Failed`); parse the answer (unparseable → `Failed`, nothing written); apply
//! [`cv_ingest::enrich::enrich`] (bodies under [`SHORT_WORDS`] words get the fold but no top
//! block); set `enriched: <date>` and `enrich-model: <provider:model>`; write with the hash read
//! *before* the model call (I2: an edit during the call is a conflict, never a clobber).
//!
//! There is no hold check: a note's hold is the claimed job's own (`runner/holds.rs`), which
//! only refuses *client* writes; steps are the holders.
//!
//! **Backfill scope** enqueues one `intake-enrich` child per note (`spawn_child_at`), newest first.
//! Every child shares the `clip::enrich` resource key, so enrichment runs one at a time and the
//! backfill job itself (no resource keys) never occupies a second slot of the same key. An
//! *intake note* is a markdown note outside `.trash/` and `_`/`.`-prefixed top folders that
//! carries `post-id` (tracker blog), `message-id` (mail) or a YouTube `source` URL (clipped
//! video), and no non-empty `enriched`.
//!
//! Outcome vars: `skipped` (`"already enriched"`), `enriched`, `anchors_resolved`,
//! `anchors_dropped`, `lines_folded`, `banners_folded`, `fold_refused_over_half`, `top_block`,
//! `headings_inserted`, `spans_folded`, `words`; backfill: `enqueued`, `enqueue_failed`.

use std::collections::{BTreeMap, BTreeSet};

use cv_actions::{ConfigSet, StepKind};
use cv_ingest::enrich::{self, BannerRef, EnrichOpts, EnrichReport};
use cv_note::{FrontmatterDoc, JsValue};
use cv_providers::ProviderManager;
use serde::Serialize;
use serde_json::Value;

use super::agent;
use super::params::{check_keys, get_bool, get_str};
use super::{StepCtx, StepError, StepModule, StepOutcome, job_flag};

/// The action a single note is enriched by (also the backfill's child).
pub(crate) const ENRICH_ACTION: &str = "intake-enrich";
const DEFAULT_AGENT: &str = "intake-enrich";
const DEFAULT_LIMIT: usize = 20;
/// Bodies under this many words are cleaned (fold) but get no top block or headings.
pub(crate) const SHORT_WORDS: usize = 150;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Note,
    Backfill,
}

#[derive(Debug, Clone)]
pub(crate) struct Params {
    scope: Scope,
    force: bool,
    agent: String,
    limit: usize,
}

pub(crate) fn parse(t: &toml::Table) -> Result<Params, String> {
    check_keys(t, StepKind::IntakeEnrich, "intake_enrich")?;
    let scope = match get_str(t, "scope")?.as_deref() {
        None | Some("note") => Scope::Note,
        Some("backfill") => Scope::Backfill,
        Some(s) => {
            return Err(format!(
                "intake_enrich step: `scope` {s:?} is not note|backfill"
            ));
        }
    };
    let limit = match t.get("limit") {
        None => DEFAULT_LIMIT,
        Some(toml::Value::Integer(n)) if *n > 0 => usize::try_from(*n).unwrap_or(usize::MAX),
        Some(_) => return Err("intake_enrich step: `limit` must be an integer > 0".into()),
    };
    let agent = get_str(t, "agent")?.unwrap_or_else(|| DEFAULT_AGENT.to_owned());
    if agent.trim().is_empty() {
        return Err("intake_enrich step: `agent` is empty".into());
    }
    Ok(Params {
        scope,
        force: get_bool(t, "force")?.unwrap_or(false),
        agent: agent.trim().to_owned(),
        limit,
    })
}

/// The `intake_enrich` step. Parsing checks only the step's own keys: the agent and provider
/// resolve at run time, so a config without the `router` provider (an unmigrated vault) never
/// disarms `clip-enrich` — the step fails visibly instead.
pub(crate) struct IntakeEnrich;

impl StepModule for IntakeEnrich {
    type Params = Params;

    fn parse(_: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<Params, String> {
        parse(t)
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        match p.scope {
            Scope::Note => run_note(&p, ctx, agent::manager()?),
            Scope::Backfill => backfill(&p, ctx),
        }
    }
}

/// Outcome vars of a note run.
#[derive(Serialize, Default)]
struct Vars {
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped: Option<&'static str>,
    enriched: bool,
    words: usize,
    anchors_resolved: usize,
    anchors_dropped: usize,
    spans_folded: usize,
    lines_folded: usize,
    banners_folded: usize,
    fold_refused_over_half: bool,
    top_block: bool,
    headings_inserted: usize,
}

impl Vars {
    fn from_report(words: usize, r: &EnrichReport) -> Self {
        Self {
            skipped: None,
            enriched: true,
            words,
            anchors_resolved: r.anchors_resolved,
            anchors_dropped: r.anchors_dropped,
            spans_folded: r.spans_folded,
            lines_folded: r.lines_folded,
            banners_folded: r.banners_folded,
            fold_refused_over_half: r.fold_refused_over_half,
            top_block: r.top_block,
            headings_inserted: r.headings_inserted,
        }
    }
}

/// Whether a frontmatter value counts as "set" (null and blank strings do not).
fn is_set(v: Option<&JsValue>) -> bool {
    match v {
        None | Some(JsValue::Null) => false,
        Some(JsValue::Str(s)) => !s.trim().is_empty(),
        Some(_) => true,
    }
}

/// Whether a note's `enriched` marker is set.
pub(crate) fn already_enriched(text: &str) -> bool {
    is_set(
        FrontmatterDoc::new(text)
            .get("enriched")
            .ok()
            .flatten()
            .as_ref(),
    )
}

/// The note run, with the provider manager injected (tests pass one with a test secret source).
pub(crate) fn run_note(
    p: &Params,
    ctx: &mut StepCtx<'_>,
    mgr: &ProviderManager,
) -> Result<StepOutcome, StepError> {
    // A preceding `transcript_refine` that rewrote the note forces a re-enrich on the refined
    // text; its outcome var rides the checkpointed vars, so a replayed run sees it too.
    let refined_now = ctx
        .vars
        .get(super::transcript_refine::REFINED_VAR)
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let force = job_flag(ctx, "force").unwrap_or(p.force) || refined_now;
    let path = ctx
        .context
        .active_path
        .clone()
        .ok_or_else(|| StepError::failed("intake_enrich: needs an active note"))?;
    // The hash is taken before the slow model call and used for the write (I2).
    let (bytes, hash) = ctx.read_required(&path)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| StepError::failed("intake_enrich: target note is not UTF-8"))?;
    let mut out = StepOutcome::default();
    if !force && already_enriched(&text) {
        out.set_vars(&Vars {
            skipped: Some("already enriched"),
            ..Vars::default()
        });
        return Ok(out);
    }
    let words = FrontmatterDoc::new(text.as_str())
        .body()
        .split_whitespace()
        .count();

    let mut answer = agent::run(&agent::var_params(&p.agent), ctx, mgr)?;
    let reply = match answer.vars.remove(agent::DEFAULT_OUTPUT_VAR) {
        Some(Value::String(s)) => s,
        _ => {
            return Err(StepError::failed(
                "intake_enrich: the agent returned no text",
            ));
        }
    };
    let model_ref = match (
        answer.vars.get("agent_provider"),
        answer.vars.get("agent_model"),
    ) {
        (Some(Value::String(p)), Some(Value::String(m))) => format!("{p}:{m}"),
        _ => String::new(),
    };
    let parsed = enrich::parse(&reply).map_err(|e| {
        StepError::failed(format!(
            "intake_enrich: agent {:?} answer unusable ({e}); nothing written",
            p.agent
        ))
    })?;
    let banners = banner_refs(ctx, &path, &text);
    let edit = enrich::enrich(
        &text,
        &parsed,
        &banners,
        EnrichOpts {
            top_block: words >= SHORT_WORDS,
        },
    );
    let mut doc = FrontmatterDoc::new(edit.text);
    let date = super::materialize::now().format("%Y-%m-%d").to_string();
    let fm_err =
        |e: cv_note::FmError| StepError::failed(format!("intake_enrich: frontmatter: {e}"));
    doc.set("enriched", &JsValue::Str(date)).map_err(fm_err)?;
    if !model_ref.is_empty() {
        doc.set("enrich-model", &JsValue::Str(model_ref))
            .map_err(fm_err)?;
    }
    let new = doc.into_text();
    if new != text {
        ctx.write(&path, new.into_bytes(), Some(hash))?;
    }
    out.push_output(path);
    out.set_vars(&Vars::from_report(words, &edit.report));
    Ok(out)
}

/// The recurring-banner embeds of `path` from the index (`cv_index::queries::banners`); an index
/// failure is logged and yields none — enrichment never fails on it.
fn banner_refs(ctx: &StepCtx<'_>, path: &str, text: &str) -> Vec<BannerRef> {
    let body_offset = text.len() - FrontmatterDoc::new(text).body().len();
    let found = ctx.index().map_err(|e| e.to_string()).and_then(|reader| {
        cv_index::queries::banners::banner_refs(&reader.query(), path, text, body_offset)
            .map_err(|e| e.to_string())
    });
    match found {
        Ok(v) => v.into_iter().map(BannerRef).collect(),
        Err(e) => {
            tracing::warn!(path, error = %e, "banner lookup failed; none folded");
            Vec::new()
        }
    }
}

/// Enqueues [`ENRICH_ACTION`] on `path` (a tracker note that just reached its final form); a
/// failure to enqueue is logged, never fatal — the note exists, and the backfill covers it.
pub(crate) fn enqueue_for(ctx: &StepCtx<'_>, path: &str) -> Option<String> {
    match ctx
        .children
        .spawn_child_at(ENRICH_ACTION, serde_json::Map::new(), path)
    {
        Ok(id) => Some(id),
        Err(e) => {
            tracing::warn!(path, error = %e, "could not enqueue intake-enrich");
            None
        }
    }
}

/// Outcome vars of a backfill run.
#[derive(Serialize)]
struct BackfillVars {
    candidates: usize,
    enqueued: usize,
    enqueue_failed: usize,
}

/// Whether a `source` value is a YouTube video URL (a clipped video).
pub(crate) fn is_youtube_source(source: &str) -> bool {
    let s = source.trim();
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    matches!(
        host.strip_prefix("www.")
            .or_else(|| host.strip_prefix("m."))
            .unwrap_or(host),
        "youtube.com" | "youtu.be"
    )
}

/// Whether `path` is outside the system folders (`.trash/`, `.`/`_`-prefixed top folders such as
/// `_custom`, `_yt_metadata`).
fn is_user_path(path: &str) -> bool {
    !path.starts_with(['.', '_'])
}

/// The newest-first (`ctime` desc, then path) unenriched candidates, at most `limit`.
pub(crate) fn pick_backfill(
    candidates: &BTreeMap<String, i64>,
    enriched: &BTreeSet<String>,
    limit: usize,
) -> Vec<String> {
    let mut v: Vec<(&String, i64)> = candidates
        .iter()
        .filter(|(p, _)| !enriched.contains(*p))
        .map(|(p, c)| (p, *c))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    v.into_iter().take(limit).map(|(p, _)| p.clone()).collect()
}

/// Intake notes (candidates with their `ctime`) and the set of already-enriched paths.
fn intake_notes(ctx: &StepCtx<'_>) -> Result<(BTreeMap<String, i64>, BTreeSet<String>), StepError> {
    let reader = ctx.index()?;
    let q = reader.query();
    let ctimes: BTreeMap<String, i64> = q
        .md_files_under("")?
        .into_iter()
        .map(|f| (f.path, f.ctime))
        .collect();
    let mut cands = BTreeSet::new();
    for key in ["post-id", "message-id"] {
        cands.extend(q.md_paths_with_prop_key(key)?);
    }
    for r in q.md_props_with_key("source")? {
        if is_youtube_source(&cv_index::prop_text(&r.value)) {
            cands.insert(r.path);
        }
    }
    let enriched: BTreeSet<String> = q
        .md_props_with_key("enriched")?
        .into_iter()
        .filter(|r| match &r.value {
            Value::Null => false,
            Value::String(s) => !s.trim().is_empty(),
            _ => true,
        })
        .map(|r| r.path)
        .collect();
    let cands = cands
        .into_iter()
        .filter(|p| is_user_path(p))
        .map(|p| {
            let c = ctimes.get(&p).copied().unwrap_or(0);
            (p, c)
        })
        .collect();
    Ok((cands, enriched))
}

/// The backfill batch size: a job param `limit` (a positive integer or its decimal text — the
/// action's param kinds have no integer) wins over the step key; anything else is rejected, never
/// rounded or coerced.
pub(crate) fn effective_limit(
    job: &serde_json::Map<String, Value>,
    step_limit: usize,
) -> Result<usize, String> {
    let bad = |v: &Value| format!("intake_enrich: job param `limit` {v} is not a positive integer");
    match job.get("limit") {
        None | Some(Value::Null) => Ok(step_limit),
        Some(v @ Value::Number(n)) => n
            .as_u64()
            .filter(|n| *n > 0)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| bad(v)),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(step_limit),
        Some(v @ Value::String(s)) => s
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| bad(v)),
        Some(v) => Err(bad(v)),
    }
}

fn backfill(p: &Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let limit = effective_limit(ctx.params, p.limit).map_err(StepError::failed)?;
    let (cands, enriched) = intake_notes(ctx)?;
    let picks = pick_backfill(&cands, &enriched, limit);
    let (mut ok, mut failed) = (0, 0);
    for path in &picks {
        if enqueue_for(ctx, path).is_some() {
            ok += 1;
        } else {
            failed += 1;
        }
    }
    let mut out = StepOutcome::default();
    out.set_vars(&BackfillVars {
        candidates: cands.len(),
        enqueued: ok,
        enqueue_failed: failed,
    });
    Ok(out)
}
