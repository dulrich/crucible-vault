//! Step module `intake_enrich` (plan `intake-auto-enrich` WP-3; measurements and the design in
//! `docs/spikes/auto-intake-llm/FINDINGS.md`, decision D29): one LLM structure + cleanup pass over
//! an intake note, applied by `cv_ingest::enrich` — chapter headings in heading-less bodies and
//! **one folded `[!removed]-` callout** for what the cleanup marks. The model never rewrites text
//! and, unless `delete_confidence` is set, nothing is deleted. No top block is written (plan
//! abstract-removal-confidence-deletion WP-2, Decision 1); every write this step makes removes a
//! legacy `[!abstract]` one.
//!
//! | key | values | default |
//! |---|---|---|
//! | `scope` | `"note"` (the run's active note) \| `"backfill"` (enqueue `intake-enrich` per unenriched intake note) \| `"strip-top"` (legacy top-block strip, below) | `"note"` |
//! | `force` | re-run on a note that already carries `enriched`; a job param `force` wins | `false` |
//! | `agent` | the `agents.toml` id returning the JSON answer | `"intake-enrich"` |
//! | `limit` | backfill only: notes enqueued per run, newest first (> 0) | `20` |
//! | `delete_confidence` | deletion threshold, a number in 0.0–1.0 (out of range, NaN or a non-number is a parse error); `0` = off | `0` |
//!
//! **Deletion tier** (plan abstract-removal-confidence-deletion WP-3, Decision 3; ships off): with
//! `delete_confidence` > 0, a fold run claimed only by model spans whose valid confidence is at or
//! over it ([`enrich::deletes`]) is removed from the body with no fold copy
//! ([`enrich::enrich_with`]); a missing or malformed confidence, `content_ends_after`, banners,
//! code fences and lines Keep-labelled for the note's `publication` (masked key) always fold. When
//! lines are deleted from a mail note, a daemon-derived `description` is recomputed; an authored
//! one is left alone. Labels are read only when deletion is on.
//!
//! **Note scope**, in order: read the note and its CAS hash; a non-empty `enriched` frontmatter
//! value skips (no model call) unless forced; call the agent through the `agent` step's provider
//! path (the same failure mapping: transport/5xx/429 → `Deferred`, missing key → `Failed{NoApiKey}`,
//! truncation → `Failed`); parse the answer (unparseable → `Failed`, nothing written); apply
//! [`cv_ingest::enrich::enrich`] (bodies under [`SHORT_WORDS`] words get the fold but no
//! headings); set `enriched: <date>` and `enrich-model: <provider:model>` and remove `enrich-miss`;
//! write with the hash read *before* the model call (I2: an edit during the call is a conflict,
//! never a clobber).
//!
//! **Miss rule** (plan `intake-removals-sources-rescan` WP-7, restated by plan
//! abstract-removal-confidence-deletion WP-2 (b)): a *miss* is a body of [`SHORT_WORDS`] words or
//! more whose answer requested anchors (span `first_line`/`last_line` pairs, `content_ends_after`,
//! chapter `starts_at` the pass evaluated) and resolved none of them. An answer that requested no
//! anchors, or resolved at least one (a span whose anchors are lines an earlier run already folded
//! counts, `anchors_already_folded`), stamps. A miss stamps nothing and changes the body only by
//! removing a legacy top block ([`enrich::strip_top`]); `enrich-miss` = previous count + 1 (absent
//! or non-numeric reads as 0) is written, with the pre-call hash. Short notes are never a miss. A
//! deleted span resolved, so it counts as resolved. With deletion on, a run on a note that already
//! carries `enriched` (a forced re-run) is never a miss: the lines an earlier run deleted are gone,
//! their anchors drop, and the stamp is the evidence of the earlier success (no deleted text is
//! stored to recognise them). An
//! unparseable answer or a failed call stays a step failure with nothing written.
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
//! **Strip-top scope** (`scope = "strip-top"`; no model call, nothing enqueued; plan
//! abstract-removal-confidence-deletion WP-2 (d)): for every user markdown note carrying a legacy
//! top block ([`enrich::has_legacy_top`]: a managed `%% cv-enrich %%` block outside code fences
//! whose callout is `> [!abstract]`), a YouTube clip first gets back a `## Transcript` heading the
//! block's ToC still links ([`cv_note::restore_transcript_heading`]; the link is gone after the
//! strip), then the block is removed ([`enrich::strip_top`]) with a CAS write (a lost CAS is
//! counted). `enriched`, inserted headings, folds and fold metadata are untouched; a second run
//! writes nothing.
//!
//! **Recovery is retired** (same plan, (e)): `scope = "recover"`, a `recover` step key and a
//! `recover` job param are refused. After the strip a correctly enriched note can carry no
//! generated block, which is exactly what recovery un-stamped.
//!
//! Outcome vars: `skipped` (`"already enriched"`), `enriched`, `anchors_resolved`,
//! `anchors_dropped`, `anchors_already_folded`, `lines_folded`, `banners_folded`, `fold_refused_over_half`,
//! `headings_inserted`, `spans_folded`, `lines_deleted`, `spans_deleted` (counts only), `words`;
//! on a miss `enriched: false`, `miss: true`,
//! `enrich_miss`; backfill: `candidates`, `enqueued`, `enqueue_failed`, `miss_capped`; strip-top:
//! `candidates` (notes scanned), `stripped`, `heading_restored`, `strip_conflict`.

use std::collections::{BTreeMap, BTreeSet};

use cv_actions::{ConfigSet, StepKind};
use cv_ingest::enrich::{self, BannerRef, Deletion, EnrichOpts, EnrichReport};
use cv_note::{FrontmatterDoc, JsValue};
use cv_providers::ProviderManager;
use serde::Serialize;

use super::agent;
use super::intake_kind::{IntakeKinds, is_blank};
use super::params::{check_keys, get_bool, get_pos_int, get_str, job_flag, job_pos_int_in};
use super::{StepCtx, StepError, StepModule, StepOutcome};

/// The action a single note is enriched by (also the backfill's child).
pub(crate) const ENRICH_ACTION: &str = "intake-enrich";
const DEFAULT_AGENT: &str = "intake-enrich";
const DEFAULT_LIMIT: usize = 20;
/// Bodies under this many words are cleaned (fold) but get no headings, and are never a miss.
pub(crate) const SHORT_WORDS: usize = 150;
/// The backfill never picks a note whose `enrich-miss` count is at or over this.
pub(crate) const ENRICH_MISS_CAP: u32 = 3;

/// The step's `scope`: the shared `note|backfill` grammar plus `strip-top`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Note,
    Backfill,
    StripTop,
}

/// The refusal of the retired recovery (`what` names the step key, the job param or the scope
/// value).
fn obsolete_recover(what: &str) -> String {
    format!(
        "intake_enrich: {what} `recover` is no longer accepted; recovery is retired (enrich writes \
         no top block, so a stamped note without one is not a recovery target); use \
         `scope = \"strip-top\"` to remove legacy top blocks"
    )
}

#[derive(Debug, Clone)]
pub(crate) struct Params {
    scope: Scope,
    force: bool,
    agent: String,
    limit: usize,
    /// The deletion threshold (0 = off).
    pub(crate) delete_confidence: f64,
}

/// The `delete_confidence` key: absent = 0 (off); an integer or float in 0.0–1.0, else an error
/// naming the key and the range (never clamped).
fn get_delete_confidence(t: &toml::Table) -> Result<f64, String> {
    let err = || "intake_enrich step: `delete_confidence` must be a number in 0.0–1.0".to_owned();
    #[allow(clippy::cast_precision_loss)]
    let v = match t.get("delete_confidence") {
        None => return Ok(0.0),
        Some(toml::Value::Float(f)) => *f,
        Some(toml::Value::Integer(n)) => *n as f64,
        Some(_) => return Err(err()),
    };
    if v.is_finite() && (0.0..=1.0).contains(&v) {
        Ok(v)
    } else {
        Err(err())
    }
}

pub(crate) fn parse(t: &toml::Table) -> Result<Params, String> {
    if t.contains_key("recover") {
        return Err(obsolete_recover("step key"));
    }
    check_keys(t, StepKind::IntakeEnrich, "intake_enrich")?;
    let scope = match get_str(t, "scope")?.as_deref() {
        None | Some("note") => Scope::Note,
        Some("backfill") => Scope::Backfill,
        Some("strip-top") => Scope::StripTop,
        Some("recover") => return Err(obsolete_recover("scope")),
        Some(s) => {
            return Err(format!(
                "intake_enrich step: `scope` {s:?} is not note|backfill|strip-top"
            ));
        }
    };
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
        agent: agent.trim().to_owned(),
        limit,
        delete_confidence: get_delete_confidence(t)?,
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
        if ctx.params.contains_key("recover") {
            return Err(StepError::failed(obsolete_recover("job param")));
        }
        match p.scope {
            Scope::Note => run_note(&p, ctx, agent::manager()?),
            Scope::Backfill => backfill(&p, ctx),
            Scope::StripTop => strip_top(ctx),
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
    anchors_already_folded: usize,
    spans_folded: usize,
    lines_folded: usize,
    banners_folded: usize,
    fold_refused_over_half: bool,
    headings_inserted: usize,
    lines_deleted: usize,
    spans_deleted: usize,
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
            anchors_already_folded: r.anchors_already_folded,
            spans_folded: r.spans_folded,
            lines_folded: r.lines_folded,
            banners_folded: r.banners_folded,
            fold_refused_over_half: r.fold_refused_over_half,
            headings_inserted: r.headings_inserted,
            lines_deleted: r.lines_deleted,
            spans_deleted: r.spans_deleted,
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

/// Whether an edit of a body of `words` words is a miss: at least [`SHORT_WORDS`] words, and the
/// answer's anchors the pass evaluated all failed to resolve (none requested is no miss; a span
/// an earlier run already folded counts as resolved).
pub(crate) fn is_miss(words: usize, r: &EnrichReport) -> bool {
    words >= SHORT_WORDS
        && r.anchors_dropped > 0
        && r.anchors_resolved == 0
        && r.anchors_already_folded == 0
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
    // The heading restore precedes every write that drops a legacy top block (invariant (c)); the
    // write still checks the hash of the bytes read (`orig`).
    let orig = text;
    let text = restore_legacy_heading(&orig).unwrap_or_else(|| orig.clone());
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
    // Labels are looked up only when deletion is on: with it off nothing here touches state.db.
    let labels = if p.delete_confidence > 0.0 {
        match source.get_str("publication") {
            Some(publ) if !publ.trim().is_empty() => {
                Some(super::reconcile_boilerplate::load_labels(ctx, publ.trim())?)
            }
            _ => None,
        }
    } else {
        None
    };
    let edit = enrich::enrich_with(
        body,
        &parsed,
        &banners,
        EnrichOpts {
            headings: words >= SHORT_WORDS,
        },
        Deletion {
            threshold: p.delete_confidence,
            labels: labels.as_ref(),
        },
    );
    let fm_err =
        |e: cv_note::FmError| StepError::failed(format!("intake_enrich: frontmatter: {e}"));
    // Miss rule with deletion on: a forced re-run of a stamped note finds the lines an earlier
    // run deleted gone, so their anchors drop. The `enriched` stamp is the evidence that pass
    // succeeded (no deleted text is kept to recognise them), so it is not a miss then.
    let redone = p.delete_confidence > 0.0 && already_enriched(&orig);
    if is_miss(words, &edit.report) && !redone {
        // Nothing usable came back: keep the body (minus a legacy top block, invariant (a)) and
        // the unstamped state, count the miss.
        let mut doc = match enrich::strip_top(body) {
            Some(stripped) => reattach(&text, body, &stripped),
            None => FrontmatterDoc::new(text.as_str()),
        };
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
    if edit.report.lines_deleted > 0 {
        redescribe_mail(&mut doc, body).map_err(fm_err)?;
    }
    let date = super::materialize::now().format("%Y-%m-%d").to_string();
    super::enrich_stamp::set(&mut doc, date, model_ref).map_err(fm_err)?;
    let new = doc.into_text();
    if new != orig {
        ctx.write(&path, new.into_bytes(), Some(hash))?;
    }
    out.push_output(path);
    out.set_vars(&Vars::from_report(words, &edit.report));
    Ok(out)
}

/// After a deletion from a mail note (`via: email`): its `description` is recomputed from the new
/// body the way capture and the boilerplate strip derive it ([`super::mail::mail_description`])
/// when it is daemon-derived — equal to that derivation of the pre-edit body `old_body`; any other
/// (authored) `description` is left alone.
fn redescribe_mail(doc: &mut FrontmatterDoc, old_body: &str) -> Result<(), cv_note::FmError> {
    let mail = doc
        .get_str("via")
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("email"));
    let Some(cur) = doc.get_str("description") else {
        return Ok(());
    };
    if !mail || cur != super::mail::mail_description(old_body) {
        return Ok(());
    }
    let fresh = super::mail::mail_description(doc.body());
    if fresh != cur {
        doc.set("description", &JsValue::Str(fresh))?;
    }
    Ok(())
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

/// Outcome vars of a strip-top run.
#[derive(Serialize)]
struct StripVars {
    candidates: usize,
    stripped: usize,
    heading_restored: usize,
    strip_conflict: usize,
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

/// The vault's intake notes as both scopes see them.
struct IntakeNotes {
    /// Candidates with their `ctime`.
    ctimes: BTreeMap<String, i64>,
    /// The already-enriched paths.
    enriched: BTreeSet<String>,
    /// The YouTube clips among the intake kinds.
    youtube: BTreeSet<String>,
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
    let enriched: BTreeSet<String> = q
        .md_props_with_key("enriched")?
        .into_iter()
        .filter(|r| !is_blank(&r.value))
        .map(|r| r.path)
        .collect();
    let cands = kinds
        .candidates()
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
        youtube: kinds.youtube(),
    })
}

/// The `enrich-miss` count per path (only paths carrying the key).
fn miss_counts(ctx: &StepCtx<'_>) -> Result<BTreeMap<String, u32>, StepError> {
    let reader = ctx.index()?;
    Ok(reader
        .query()
        .md_props_with_key("enrich-miss")?
        .into_iter()
        .map(|r| {
            let v = match r.value {
                serde_json::Value::Number(n) => n.as_f64().map(JsValue::Num),
                serde_json::Value::String(s) => Some(JsValue::Str(s)),
                _ => None,
            };
            (r.path, miss_count(v.as_ref()))
        })
        .collect())
}

/// YouTube clips with a pending transcript refine (enriched or not): no `refined` tag and a
/// [`Ready`](cv_ingest::transcript::Eligibility::Ready) section — exactly the notes the step
/// would refine, so a note it skips or rejects (short, or split by managed blocks) is never
/// re-picked by every backfill pass.
fn refine_pending(
    ctx: &StepCtx<'_>,
    youtube: &BTreeSet<String>,
) -> Result<BTreeSet<String>, StepError> {
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
    Ok(refine)
}

fn backfill(p: &Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let limit = job_pos_int_in(ctx, "intake_enrich", "limit")?.unwrap_or(p.limit);
    let notes = intake_notes(ctx)?;
    let misses = miss_counts(ctx)?;
    let refine = refine_pending(ctx, &notes.youtube)?;
    let (picks, miss_capped) = pick_capped(&notes.ctimes, &notes.enriched, &refine, &misses, limit);
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

/// The legacy top-block strip of one note's `text`: `None` when it carries none; else the new
/// text and whether a `## Transcript` heading was restored first (YouTube clips only, while the
/// block's ToC link still says the heading belongs).
pub(crate) fn strip_top_text(text: &str) -> Option<(String, bool)> {
    let restored = restore_legacy_heading(text);
    let heading = restored.is_some();
    let cur = restored.unwrap_or_else(|| text.to_owned());
    let body = FrontmatterDoc::new(cur.as_str()).body().to_owned();
    let stripped = enrich::strip_top(&body)?;
    let new = reattach(&cur, &body, &stripped).into_text();
    Some((new, heading))
}

/// Invariant (c): `text` with a `## Transcript` heading re-inserted when it is a YouTube clip
/// whose legacy top block's ToC still links the missing heading; `None` otherwise. Every path that
/// removes a legacy top block (strip-top, the note scope's success and miss writes) applies it
/// first, because the link is the only evidence and leaves with the block.
pub(crate) fn restore_legacy_heading(text: &str) -> Option<String> {
    (cv_note::is_youtube_clip(text) && enrich::has_legacy_top(FrontmatterDoc::new(text).body()))
        .then(|| cv_note::restore_transcript_heading(text))
        .flatten()
}

/// The strip-top scope: removes the legacy top block of every user markdown note carrying one
/// ([`strip_top_text`]), CAS-written; no model call, nothing enqueued.
fn strip_top(ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let paths: Vec<String> = ctx
        .index()?
        .query()
        .md_files_under("")?
        .into_iter()
        .map(|f| f.path)
        .filter(|p| cv_text::vpath::is_user_path(p))
        .collect();
    let (mut stripped, mut restored, mut conflict) = (0, 0, 0);
    for path in &paths {
        let Some((bytes, hash)) = ctx.read(path)? else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let Some((new, heading)) = strip_top_text(&text) else {
            continue;
        };
        match ctx.try_write(path, new.into_bytes(), Some(hash))? {
            Ok(_) => {
                stripped += 1;
                restored += usize::from(heading);
            }
            Err(cv_vault::VaultError::Conflict { .. }) => conflict += 1,
            Err(e) => return Err(e.into()),
        }
    }
    let mut out = StepOutcome::default();
    out.set_vars(&StripVars {
        candidates: paths.len(),
        stripped,
        heading_restored: restored,
        strip_conflict: conflict,
    });
    Ok(out)
}

/// The breaker key of an `intake_enrich` step: its agent's default-model provider for the note
/// scope (what [`agent::complete`] defers under); the backfill and strip-top scopes call no
/// provider themselves (plan intake-stage-jobs WP-1).
pub(crate) fn service_key(t: &toml::Table, config: &ConfigSet) -> Result<Option<String>, String> {
    let p = parse(t)?;
    match p.scope {
        Scope::Note => agent::default_service_key(config, &p.agent).map(Some),
        Scope::Backfill | Scope::StripTop => Ok(None),
    }
}
