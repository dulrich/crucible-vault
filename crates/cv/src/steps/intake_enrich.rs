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
//! | `recover` | backfill only: recovery mode (below); a job param `recover` wins | `false` |
//! | `agent` | the `agents.toml` id returning the JSON answer | `"intake-enrich"` |
//! | `limit` | backfill only: notes enqueued per run, newest first (> 0) | `20` |
//!
//! **Note scope**, in order: read the note and its CAS hash; a non-empty `enriched` frontmatter
//! value skips (no model call) unless forced; call the agent through the `agent` step's provider
//! path (the same failure mapping: transport/5xx/429 → `Deferred`, missing key → `Failed{NoApiKey}`,
//! truncation → `Failed`); parse the answer (unparseable → `Failed`, nothing written); apply
//! [`cv_ingest::enrich::enrich`] (bodies under [`SHORT_WORDS`] words get the fold but no top
//! block); set `enriched: <date>` and `enrich-model: <provider:model>` and remove `enrich-miss`;
//! write with the hash read *before* the model call (I2: an edit during the call is a conflict,
//! never a clobber).
//!
//! **Miss rule** (plan `intake-removals-sources-rescan` WP-7): a *miss* is a body of
//! [`SHORT_WORDS`] words or more whose edit produced neither a top block nor a fold. A miss
//! stamps nothing and writes no body: only `enrich-miss` = previous count + 1 (absent or
//! non-numeric reads as 0) is written, with the pre-call hash. Short notes are never a miss.
//! The backfill holds back notes whose `enrich-miss` is at or over [`ENRICH_MISS_CAP`] (counted
//! as `miss_capped`; `refine_pending` picks are left alone); `force` runs a note regardless.
//!
//! There is no hold check: a note's hold is the claimed job's own (`runner/holds.rs`), which
//! only refuses *client* writes; steps are the holders.
//!
//! **Backfill scope** enqueues one `intake-enrich` child per note (`spawn_child_at`), newest first.
//! Every child shares the `clip::enrich` resource key, so enrichment runs one at a time and the
//! backfill job itself (no resource keys) never occupies a second slot of the same key. An
//! *intake note* is a markdown note outside `.trash/` and `_`/`.`-prefixed top folders that
//! carries `post-id` (tracker blog), `message-id` (mail) or a YouTube `source` URL (clipped
//! video), and no non-empty `enriched` — plus (plan `youtube-transcript-auto-refine` WP-2, D30)
//! every YouTube clip whose `## Transcript` section the step would refine (one segment of at least
//! `MIN_REFINE_WORDS` words, no `refined` tag), enriched or
//! not: the child's `transcript_refine` step refines it and clears `enriched`, so the re-enrich
//! follows. The batch is
//! newest first over the union, at most `limit`, no duplicates.
//!
//! **Recovery** (`scope = "backfill"`, `recover = true`; nothing is enqueued): for every intake
//! candidate with a non-blank `enriched`, a body of [`SHORT_WORDS`] words or more and neither
//! generated block, remove `enriched`, `enrich-model` and `enrich-miss` with a CAS write (a lost
//! CAS is counted). A second run recovers 0.
//!
//! Outcome vars: `skipped` (`"already enriched"`), `enriched`, `anchors_resolved`,
//! `anchors_dropped`, `lines_folded`, `banners_folded`, `fold_refused_over_half`, `top_block`,
//! `headings_inserted`, `spans_folded`, `words`; on a miss `enriched: false`, `miss: true`,
//! `enrich_miss`; backfill: `candidates`, `enqueued`, `enqueue_failed`, `miss_capped`; recovery:
//! `candidates`, `recovered`, `recover_conflict`.

use std::collections::{BTreeMap, BTreeSet};

use cv_actions::{ConfigSet, StepKind};
use cv_ingest::enrich::{self, BannerRef, EnrichOpts, EnrichReport};
use cv_note::{FrontmatterDoc, JsValue};
use cv_providers::ProviderManager;
use serde::Serialize;

use super::agent;
use super::intake_kind::{IntakeKinds, is_blank};
use super::params::{
    Scope, check_keys, get_bool, get_pos_int, get_scope, get_str, job_flag, job_pos_int_in,
};
use super::{StepCtx, StepError, StepModule, StepOutcome};

/// The action a single note is enriched by (also the backfill's child).
pub(crate) const ENRICH_ACTION: &str = "intake-enrich";
const DEFAULT_AGENT: &str = "intake-enrich";
const DEFAULT_LIMIT: usize = 20;
/// Bodies under this many words are cleaned (fold) but get no top block or headings.
pub(crate) const SHORT_WORDS: usize = 150;
/// The backfill never picks a note whose `enrich-miss` count is at or over this.
pub(crate) const ENRICH_MISS_CAP: u32 = 3;

#[derive(Debug, Clone)]
pub(crate) struct Params {
    scope: Scope,
    force: bool,
    recover: bool,
    agent: String,
    limit: usize,
}

pub(crate) fn parse(t: &toml::Table) -> Result<Params, String> {
    check_keys(t, StepKind::IntakeEnrich, "intake_enrich")?;
    let scope = get_scope(t, "intake_enrich")?;
    let limit = get_pos_int(t, "limit")
        .map_err(|e| format!("intake_enrich step: {e}"))?
        .unwrap_or(DEFAULT_LIMIT);
    let agent = get_str(t, "agent")?.unwrap_or_else(|| DEFAULT_AGENT.to_owned());
    if agent.trim().is_empty() {
        return Err("intake_enrich step: `agent` is empty".into());
    }
    Ok(Params {
        scope,
        force: get_bool(t, "force")?.unwrap_or(false),
        recover: get_bool(t, "recover")?.unwrap_or(false),
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
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    miss: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    enrich_miss: Option<u32>,
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
            miss: false,
            enrich_miss: None,
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

/// Whether a note's `enriched` marker is set (not blank under [`cv_note::is_blank`]; malformed
/// frontmatter reads as unset).
pub(crate) fn already_enriched(text: &str) -> bool {
    !FrontmatterDoc::new(text)
        .is_blank("enriched")
        .unwrap_or(true)
}

/// The `enrich-miss` count of a frontmatter value: a non-negative integer number or its decimal
/// text; absent or anything else reads as 0 (never "at the cap").
pub(crate) fn miss_count(v: Option<&JsValue>) -> u32 {
    match v {
        Some(JsValue::Num(n)) if n.fract() == 0.0 && *n >= 0.0 && *n <= f64::from(u32::MAX) => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let c = *n as u32;
            c
        }
        Some(JsValue::Str(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// The `enrich-miss` count of an indexed property value (same rule as [`miss_count`]).
fn miss_count_json(v: &serde_json::Value) -> u32 {
    match v {
        serde_json::Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()).unwrap_or(0),
        serde_json::Value::String(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// Whether an edit of a body of `words` words is a miss: long enough for a top block, and the
/// edit produced neither a top block nor a fold.
pub(crate) fn is_miss(words: usize, r: &EnrichReport) -> bool {
    words >= SHORT_WORDS && !r.top_block && r.lines_folded == 0
}

/// The note run, with the provider manager injected (tests pass one with a test secret source).
pub(crate) fn run_note(
    p: &Params,
    ctx: &mut StepCtx<'_>,
    mgr: &ProviderManager,
) -> Result<StepOutcome, StepError> {
    // A preceding `transcript_refine` that rewrote the note cleared its `enriched` stamp in the
    // same write (Decision 5), so the refined text is re-enriched without a side channel.
    let force = job_flag(ctx, "force").unwrap_or(p.force);
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
    let source = FrontmatterDoc::new(text.as_str());
    let body = source.body();
    let words = body.split_whitespace().count();

    let answer = agent::complete(ctx, mgr, &agent::var_params(&p.agent), None)?;
    let model_ref = answer.model_ref();
    let parsed = enrich::parse(&answer.text).map_err(|e| {
        StepError::failed(format!(
            "intake_enrich: agent {:?} answer unusable ({e}); nothing written",
            p.agent
        ))
    })?;
    let banners = banner_refs(ctx, &path, body);
    let edit = enrich::enrich(
        body,
        &parsed,
        &banners,
        EnrichOpts {
            top_block: words >= SHORT_WORDS,
        },
    );
    let fm_err =
        |e: cv_note::FmError| StepError::failed(format!("intake_enrich: frontmatter: {e}"));
    if is_miss(words, &edit.report) {
        // Nothing usable came back: keep the body and the unstamped state, count the miss.
        let mut doc = FrontmatterDoc::new(text.as_str());
        let n = miss_count(doc.get("enrich-miss").map_err(fm_err)?.as_ref()).saturating_add(1);
        doc.set("enrich-miss", &JsValue::Num(f64::from(n)))
            .map_err(fm_err)?;
        ctx.write(&path, doc.into_text().into_bytes(), Some(hash))?;
        let mut vars = Vars::from_report(words, &edit.report);
        vars.enriched = false;
        vars.miss = true;
        vars.enrich_miss = Some(n);
        out.push_output(path);
        out.set_vars(&vars);
        return Ok(out);
    }
    let mut doc = reattach(&text, body, &edit.text);
    let date = super::materialize::now().format("%Y-%m-%d").to_string();
    super::enrich_stamp::set(&mut doc, date, model_ref).map_err(fm_err)?;
    let new = doc.into_text();
    if new != text {
        ctx.write(&path, new.into_bytes(), Some(hash))?;
    }
    out.push_output(path);
    out.set_vars(&Vars::from_report(words, &edit.report));
    Ok(out)
}

/// The note `text` with its body (`body`, the [`FrontmatterDoc::body`] of `text`) replaced by `new_body`: the frontmatter block is kept byte-exact.
pub(crate) fn reattach(text: &str, body: &str, new_body: &str) -> FrontmatterDoc {
    let fm = &text[..text.len() - body.len()];
    FrontmatterDoc::new(format!("{fm}{new_body}"))
}

/// The recurring-banner embeds of `path` from the index (`cv_index::queries::banners`); an index
/// failure is logged and yields none — enrichment never fails on it.
fn banner_refs(ctx: &StepCtx<'_>, path: &str, body: &str) -> Vec<BannerRef> {
    let found = ctx.index().map_err(|e| e.to_string()).and_then(|reader| {
        cv_index::queries::banners::banner_refs(&reader.query(), path, body)
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
    miss_capped: usize,
}

/// Outcome vars of a recovery run.
#[derive(Serialize)]
struct RecoverVars {
    candidates: usize,
    recovered: usize,
    recover_conflict: usize,
}

#[cfg(test)]
/// [`pick_capped`] with no recorded misses: the picks only.
pub(crate) fn pick_backfill(
    candidates: &BTreeMap<String, i64>,
    enriched: &BTreeSet<String>,
    refine_pending: &BTreeSet<String>,
    limit: usize,
) -> Vec<String> {
    pick_capped(
        candidates,
        enriched,
        refine_pending,
        &BTreeMap::new(),
        limit,
    )
    .0
}

/// The newest-first (`ctime` desc, then path) candidates, at most `limit`: the unenriched ones plus
/// every path in `refine_pending` (a YouTube clip whose transcript is still unrefined, enriched
/// or not). An unenriched note whose `enrich-miss` count (`misses`, absent = 0) is at or over
/// [`ENRICH_MISS_CAP`] is held back and counted (the second value); `refine_pending` picks are
/// left alone (the refine rewrites the body, so the retry is a different input).
pub(crate) fn pick_capped(
    candidates: &BTreeMap<String, i64>,
    enriched: &BTreeSet<String>,
    refine_pending: &BTreeSet<String>,
    misses: &BTreeMap<String, u32>,
    limit: usize,
) -> (Vec<String>, usize) {
    let mut capped = 0;
    let mut v: Vec<(&String, i64)> = Vec::new();
    for (p, c) in candidates {
        if refine_pending.contains(p) {
            v.push((p, *c));
        } else if !enriched.contains(p) {
            if misses.get(p).copied().unwrap_or(0) >= ENRICH_MISS_CAP {
                capped += 1;
            } else {
                v.push((p, *c));
            }
        }
    }
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let picks = v.into_iter().take(limit).map(|(p, _)| p.clone()).collect();
    (picks, capped)
}

/// The backfill's view of the vault's intake notes.
struct IntakeNotes {
    /// Candidates with their `ctime`.
    ctimes: BTreeMap<String, i64>,
    /// The already-enriched paths.
    enriched: BTreeSet<String>,
    /// The `enrich-miss` count per path (only paths carrying the key).
    misses: BTreeMap<String, u32>,
    /// YouTube clips with a pending transcript refine (enriched or not): no `refined` tag and a
    /// [`Ready`](cv_ingest::transcript::Eligibility::Ready) section — exactly the notes the step
    /// would refine, so a note it skips or rejects (short, or split by managed blocks) is never
    /// re-picked by every backfill pass.
    refine: BTreeSet<String>,
}

fn intake_notes(ctx: &StepCtx<'_>) -> Result<IntakeNotes, StepError> {
    let reader = ctx.index()?;
    let q = reader.query();
    let ctimes: BTreeMap<String, i64> = q
        .md_files_under("")?
        .into_iter()
        .map(|f| (f.path, f.ctime))
        .collect();
    let kinds = IntakeKinds::load(ctx)?;
    let youtube = kinds.youtube();
    let cands = kinds.candidates();
    let enriched: BTreeSet<String> = q
        .md_props_with_key("enriched")?
        .into_iter()
        .filter(|r| !is_blank(&r.value))
        .map(|r| r.path)
        .collect();
    let misses: BTreeMap<String, u32> = q
        .md_props_with_key("enrich-miss")?
        .into_iter()
        .map(|r| (r.path, miss_count_json(&r.value)))
        .collect();
    let mut refine = BTreeSet::new();
    for p in youtube.iter().filter(|p| cv_text::vpath::is_user_path(p)) {
        if let Some((bytes, _)) = ctx.read(p)?
            && let Ok(text) = String::from_utf8(bytes)
            && !super::transcript_refine::is_refined(&text)
            && matches!(
                cv_ingest::transcript::eligibility(FrontmatterDoc::new(text.as_str()).body()),
                cv_ingest::transcript::Eligibility::Ready { .. }
            )
        {
            refine.insert(p.clone());
        }
    }
    let cands = cands
        .into_iter()
        .filter(|p| cv_text::vpath::is_user_path(p))
        .map(|p| {
            let c = ctimes.get(&p).copied().unwrap_or(0);
            (p, c)
        })
        .collect();
    Ok(IntakeNotes {
        ctimes: cands,
        enriched,
        misses,
        refine,
    })
}

fn backfill(p: &Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let limit = job_pos_int_in(ctx, "intake_enrich", "limit")?.unwrap_or(p.limit);
    let notes = intake_notes(ctx)?;
    if job_flag(ctx, "recover").unwrap_or(p.recover) {
        return recover(ctx, &notes);
    }
    let (picks, miss_capped) = pick_capped(
        &notes.ctimes,
        &notes.enriched,
        &notes.refine,
        &notes.misses,
        limit,
    );
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
        candidates: notes.ctimes.len(),
        enqueued: ok,
        enqueue_failed: failed,
        miss_capped,
    });
    Ok(out)
}

/// Whether a stamped note is a recovery target: a body of [`SHORT_WORDS`] words or more with
/// neither generated block (the stamp was set over a body that was never enriched).
pub(crate) fn needs_recovery(text: &str) -> bool {
    let doc = FrontmatterDoc::new(text);
    let body = doc.body();
    already_enriched(text)
        && body.split_whitespace().count() >= SHORT_WORDS
        && !enrich::has_generated_block(body)
}

/// Recovery mode: clears the enrich stamp ([`super::enrich_stamp::clear`]) of every stamped
/// candidate that [`needs_recovery`], CAS-written; nothing is enqueued.
fn recover(ctx: &mut StepCtx<'_>, notes: &IntakeNotes) -> Result<StepOutcome, StepError> {
    let (mut recovered, mut conflict) = (0, 0);
    for path in notes.ctimes.keys().filter(|p| notes.enriched.contains(*p)) {
        let Some((bytes, hash)) = ctx.read(path)? else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        if !needs_recovery(&text) {
            continue;
        }
        let mut doc = FrontmatterDoc::new(text.as_str());
        let fm_err =
            |e: cv_note::FmError| StepError::failed(format!("intake_enrich: frontmatter: {e}"));
        super::enrich_stamp::clear(&mut doc).map_err(fm_err)?;
        match ctx.try_write(path, doc.into_text().into_bytes(), Some(hash))? {
            Ok(_) => recovered += 1,
            Err(cv_vault::VaultError::Conflict { .. }) => conflict += 1,
            Err(e) => return Err(e.into()),
        }
    }
    let mut out = StepOutcome::default();
    out.set_vars(&RecoverVars {
        candidates: notes.ctimes.len(),
        recovered,
        recover_conflict: conflict,
    });
    Ok(out)
}
