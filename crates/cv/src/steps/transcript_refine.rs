//! Step module `transcript_refine` (plan `youtube-transcript-auto-refine` WP-1, decision D30;
//! measurements in `docs/spikes/transcript-refine/FINDINGS.md`): refines a YouTube clip's
//! `## Transcript` section — timestamps and filler out, readable paragraphs, `###` chapter
//! headings — through the `transcript-refine` agent, ahead of `intake_enrich`.
//!
//! | key | values | default |
//! |---|---|---|
//! | `agent` | the `agents.toml` id that refines one chunk | `"transcript-refine"` |
//!
//! Runs on the run's active note only. In order: read the note and its CAS hash *before* the
//! model call; skip (`skipped` outcome var, no model call, no write) when `source` is not a
//! YouTube URL (`"not youtube"`), the note carries the `refined` tag (`"already refined"` — the
//! manual `refine-transcript` action's convention), or it has no `## Transcript` text outside the
//! managed enrich blocks (`"no transcript"`). Otherwise [`cv_ingest::transcript`] chunks the
//! section (at most 12,000 words per call), each chunk goes through the `agent` step's provider
//! path (the same failure mapping: transport/5xx/429 → `Deferred`, truncation or an empty answer
//! → `Failed`) as the agent's `{{value}}` — the agent sees only the chunk text — and every answer
//! must keep [`cv_ingest::transcript::MIN_RETENTION`] of its chunk's words. Nothing is written
//! before every chunk passed.
//!
//! The write set, in order: the original note is copied to `_raw_transcript/<name>.md` (the
//! `copy` step's naming; a different file already there gets a `-2`…`-9` suffix, never an
//! overwrite), then the section body is replaced (the `## Transcript` heading line, the managed
//! enrich blocks and everything else stay byte-identical), the `raw-transcript` link,
//! `refine-model` and the `refined` tag are set, and the note is written with the hash read
//! *before* the model call (I2: an edit during the call is a conflict, never a clobber; the hash
//! is re-checked before the raw copy is created so a conflicted run leaves nothing behind).
//!
//! There is no editor hold for a custom step (holds only classify `agent` + replace `capture`);
//! the CAS hash is the protection.
//!
//! Outcome vars: `skipped`, `refined`, `transcript_refined` (what `intake_enrich` ORs into
//! `force`), `words_in`, `words_out`, `chunks`, `headings`.

use cv_actions::{ConfigSet, StepKind};
use cv_ingest::transcript;
use cv_note::{FrontmatterDoc, JsValue};
use cv_protocol::ActionContext;
use cv_providers::ProviderManager;
use serde::Serialize;
use serde_json::Value;

use super::agent;
use super::intake_enrich::is_youtube_source;
use super::params::{check_keys, get_str};
use super::{StepCtx, StepError, StepModule, StepOutcome};

const DEFAULT_AGENT: &str = "transcript-refine";
/// The folder of raw copies (the manual `refine-transcript` action's `copy to`).
const RAW_FOLDER: &str = "_raw_transcript";
/// The tag a refined note carries (the manual action's `not_tag` guard).
pub(crate) const REFINED_TAG: &str = "refined";
/// The outcome var `intake_enrich` ORs into `force` when this step rewrote the note.
pub(crate) const REFINED_VAR: &str = "transcript_refined";

/// Parsed step params.
#[derive(Debug, Clone)]
pub(crate) struct Params {
    agent: String,
}

pub(crate) fn parse(t: &toml::Table) -> Result<Params, String> {
    check_keys(t, StepKind::TranscriptRefine, "transcript_refine")?;
    let agent = get_str(t, "agent")?.unwrap_or_else(|| DEFAULT_AGENT.to_owned());
    if agent.trim().is_empty() {
        return Err("transcript_refine step: `agent` is empty".into());
    }
    Ok(Params {
        agent: agent.trim().to_owned(),
    })
}

/// The `transcript_refine` step. Parsing checks only the step's own keys: the agent and provider
/// resolve at run time, so an unmigrated vault never disarms `clip-enrich`.
pub(crate) struct TranscriptRefine;

impl StepModule for TranscriptRefine {
    type Params = Params;

    fn parse(_: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<Params, String> {
        parse(t)
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run_note(&p, ctx, agent::manager()?)
    }
}

/// Outcome vars.
#[derive(Serialize, Default)]
struct Vars {
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped: Option<&'static str>,
    refined: bool,
    transcript_refined: bool,
    words_in: usize,
    words_out: usize,
    chunks: usize,
    headings: usize,
}

fn skip(why: &'static str) -> StepOutcome {
    let mut out = StepOutcome::default();
    out.set_vars(&Vars {
        skipped: Some(why),
        ..Vars::default()
    });
    out
}

/// Whether `text` carries the `refined` tag (frontmatter or inline, the index's reading; a
/// nested `refined/x` counts like the manual guard's).
fn is_refined(text: &str) -> bool {
    cv_index::note_tags(text).iter().any(|t| {
        let t = t.trim().trim_start_matches('#').to_lowercase();
        t == REFINED_TAG || t.starts_with("refined/")
    })
}

/// The step body, with the provider manager injected (tests pass one with a test secret source).
pub(crate) fn run_note(
    p: &Params,
    ctx: &mut StepCtx<'_>,
    mgr: &ProviderManager,
) -> Result<StepOutcome, StepError> {
    let path = ctx
        .context
        .active_path
        .clone()
        .ok_or_else(|| StepError::failed("transcript_refine: needs an active note"))?;
    // The hash is taken before the slow model call and used for the write (I2).
    let (bytes, hash) = ctx.read_required(&path)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| StepError::failed("transcript_refine: target note is not UTF-8"))?;
    let doc = FrontmatterDoc::new(text.as_str());
    let youtube = doc
        .get_str_trimmed("source")
        .is_some_and(|s| is_youtube_source(&s));
    if !youtube {
        return Ok(skip("not youtube"));
    }
    if is_refined(&text) {
        return Ok(skip("already refined"));
    }
    let body = doc.body();
    let body_offset = text.len() - body.len();
    let Some(section) = transcript::locate(body) else {
        return Ok(skip("no transcript"));
    };
    let range = match section.segments.as_slice() {
        [] => return Ok(skip("no transcript")),
        [one] => one.clone(),
        _ => {
            return Err(StepError::failed(
                "transcript_refine: the transcript section is split by managed enrich blocks; not refined",
            ));
        }
    };
    let original = &body[range.clone()];
    let words_in = transcript::words(original);

    // One call per chunk; every answer is validated before anything is written.
    let chunks = transcript::chunks(original);
    let mut refined: Vec<String> = Vec::with_capacity(chunks.len());
    let mut model_ref = String::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let (answer, m) = refine_chunk(p, ctx, mgr, chunk)?;
        let (w_in, w_out) = (transcript::words(chunk), transcript::words(&answer));
        if !transcript::retention_ok(w_in, w_out) {
            return Err(StepError::failed(format!(
                "transcript_refine: agent {:?} answer for chunk {}/{} kept {w_out} of {w_in} words (floor {}); nothing written",
                p.agent,
                i + 1,
                chunks.len(),
                transcript::MIN_RETENTION
            )));
        }
        if model_ref.is_empty() {
            model_ref = m;
        }
        refined.push(answer);
    }
    let joined = refined.join("\n\n");
    let words_out = transcript::words(&joined);
    let headings = transcript::chapter_headings(&joined);

    let new_body = transcript::replace(body, &range, &joined);
    let new_text = format!("{}{new_body}", &text[..body_offset]);
    let mut doc = FrontmatterDoc::new(new_text);

    // The note must still be the one we read before the raw copy exists (a conflicted run
    // leaves nothing behind); the final CAS write below is the real guard.
    let (_, now) = ctx.read_required(&path)?;
    if now != hash {
        return Err(StepError::conflict());
    }
    let raw_path = save_raw(ctx, &path, text.as_bytes())?;
    let stem = raw_path.strip_suffix(".md").unwrap_or(&raw_path);
    let fm_err =
        |e: cv_note::FmError| StepError::failed(format!("transcript_refine: frontmatter: {e}"));
    doc.set("raw-transcript", &JsValue::Str(format!("[[{stem}]]")))
        .map_err(fm_err)?;
    if !model_ref.is_empty() {
        doc.set("refine-model", &JsValue::Str(model_ref))
            .map_err(fm_err)?;
    }
    doc.append_to_list("tags", &JsValue::Str(REFINED_TAG.to_owned()))
        .map_err(fm_err)?;
    ctx.write(&path, doc.into_text().into_bytes(), Some(hash))?;

    let mut out = StepOutcome::default();
    out.push_output(path);
    out.push_output(raw_path);
    out.set_vars(&Vars {
        skipped: None,
        refined: true,
        transcript_refined: true,
        words_in,
        words_out,
        chunks: chunks.len(),
        headings,
    });
    Ok(out)
}

/// One agent call whose `{{value}}` is `chunk` (the agent's `input` template is `{{value}}`):
/// the call runs on a sub-context whose [`ActionContext::value`] is the chunk, so the model sees
/// only that text. Returns the trimmed answer and `provider:model`.
fn refine_chunk(
    p: &Params,
    ctx: &mut StepCtx<'_>,
    mgr: &ProviderManager,
    chunk: &str,
) -> Result<(String, String), StepError> {
    let sub_context = ActionContext {
        value: Some(chunk.to_owned()),
        ..ctx.context.clone()
    };
    let mut sub = StepCtx {
        action: ctx.action,
        step_index: ctx.step_index,
        params: ctx.params,
        context: &sub_context,
        vars: ctx.vars,
        config: ctx.config,
        vault_root: ctx.vault_root,
        index_db: ctx.index_db,
        search: ctx.search,
        system_trash: ctx.system_trash,
        state: ctx.state,
        writer: ctx.writer.clone(),
        children: ctx.children,
        written: Vec::new(),
    };
    let mut answer = agent::run(&agent::var_params(&p.agent), &mut sub, mgr)?;
    let reply = match answer.vars.remove(agent::DEFAULT_OUTPUT_VAR) {
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_owned(),
        _ => {
            return Err(StepError::failed(format!(
                "transcript_refine: agent {:?} returned no text; nothing written",
                p.agent
            )));
        }
    };
    let model_ref = match (
        answer.vars.get("agent_provider"),
        answer.vars.get("agent_model"),
    ) {
        (Some(Value::String(p)), Some(Value::String(m))) => format!("{p}:{m}"),
        _ => String::new(),
    };
    Ok((reply, model_ref))
}

/// Copies the original note bytes to `_raw_transcript/<name>.md` (the `copy` step's naming) and
/// returns the vault path. An existing identical copy is reused; a different file there (an
/// earlier manual refine or an abandoned attempt) is never overwritten — the next free
/// `<stem>-2.md` … `<stem>-9.md` is used instead.
fn save_raw(ctx: &mut StepCtx<'_>, path: &str, original: &[u8]) -> Result<String, StepError> {
    let name = cv_text::vpath::basename(path);
    let stem = name.strip_suffix(".md").unwrap_or(name);
    for n in 1..=9 {
        let candidate = if n == 1 {
            format!("{RAW_FOLDER}/{stem}.md")
        } else {
            format!("{RAW_FOLDER}/{stem}-{n}.md")
        };
        match ctx.read(&candidate)? {
            Some((existing, _)) if existing == original => return Ok(candidate),
            Some(_) => {}
            None => {
                ctx.create(&candidate, original.to_vec())?;
                return Ok(candidate);
            }
        }
    }
    Err(StepError::failed(
        "transcript_refine: no free raw-transcript name",
    ))
}
