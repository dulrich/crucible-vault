//! `image_describe` (plan `v0-automation-pipelines` WP-6): describe localized images into the
//! MD5-keyed `image_descriptions` store the search feed folds into note chunks.
//!
//! Port of Crucible `describeOneImage` (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/utils/imageDescribe.ts:340-447`), the two-pass prompts (`src/providers/shared.ts:96,98`), the
//! vision wire (`src/providers/openaiCompatible.ts:277-296`), `extractSvgText`
//! (`src/search/imageTranscode.ts:44-77`), the record shape and `fnv1a` description hash
//! (`src/search/imageDescriptionStore.ts:36-60,114-121,178-201`) and the backfill prunes
//! (`:239-277`). Keyed by the md5 parsed from the `_MD5` file name (`utils/imageMetadata.ts:16-28`
//! → [`cv_search::prepare::localized_image_md5`]). Changes: failure classification follows the
//! queue's typed disposition (audit M1) — a transient provider failure or a missing key never
//! writes a record (Crucible only spared connection-class errors); every raster is downscaled
//! shrink-only to a 1568 px long edge (Crucible transcoded only webp/avif). Dropped
//! (Obsidian-induced): the create-replay guard, the in-renderer `OffscreenCanvas` transcode and
//! its timeout, the per-pass `withTimeout` (the queue's deferral owns retries).
//!
//! | key | values | default |
//! |---|---|---|
//! | `scope` | `"note"` (the run's active note's image links) \| `"vault"` (every indexed `_MD5` image) | `"note"` |
//! | `limit` | images described per run (> 0); the rest wait for the next run | `100` |
//! | `prune_transient` | before scanning, drop transient `failed` and degenerate `vision` records | `false` |
//!
//! Config: `pipelines.toml [image_describe]` (`enabled`, `provider` (default `local`), `model`,
//! `max_tokens` (an override of the per-pass cap, see below)).
//!
//! Describe wire (plan `round-16-version-describe-intake` WP-2; settled numbers from
//! `docs/spikes/describe-budget/FINDINGS.md` §1–3, not re-derived here). Crucible gates its describe
//! settings on `isLocal` (`https://github.com/dulrich/obsidian-crucible/blob/master/src/providers/openaiCompatible.ts:271-296`, `src/providers/shared.ts:99-108`); the same gate here is
//! [`cv_providers::limiter::is_local`] (explicit `local`, else a loopback `openai-compatible`).
//! For a **local** provider both passes send `reasoning_effort: "none"` (reasoning-on extraction
//! returns empty content at any practical cap and is no more accurate), the model family's
//! recommended sampler `temperature` 1.0 / `top_p` 0.95 / `top_k` 64 plus `repeat_penalty` 1.15 /
//! `repeat_last_n` 256 (Crucible's temperature 0 loops on ~26% of extractions), and per-pass caps
//! narrative 512 / extraction 2048 unless `max_tokens` is set (then that value caps both). A
//! **remote** provider gets none of the extra fields and the single cap `max_tokens` (default
//! 4096), exactly as before this round. Dropped from Crucible: temperature 0 (loops).
//!
//! `finish reason "length"`: a pass cut off at its cap with a non-degenerate body is stored
//! truncated (a long transcription is useful); a repetition loop ([`is_repetition_loop`]) or an
//! empty body stays a recorded permanent failure. Retry: a `failed` record whose text is the old
//! `finish reason "length"; output discarded` (every record the pre-round-16 wire produced) is
//! treated as pending again by the scan (re-described and overwritten); the new failure texts
//! differ, so a re-failed image is poison-skipped as before. Plan remote-media-sprint WP-4 extends
//! the retry to a `failed` record with **no `providerId`** in `raw` (a Crucible-era failure, e.g.
//! "API returned 500" or ENOENT, never produced by this wire): pending again, once. Any record
//! this module writes for a failure carries `providerId`, so a re-failure is poison-skipped.
//!
//! Outcome vars (counts only): `described`, `svg`, `failed`, `skipped`, `pruned`, `remaining`.

use std::collections::BTreeSet;
use std::io::Cursor;
use std::sync::OnceLock;

use base64::Engine as _;
use cv_actions::{ConfigSet, StepKind};
use cv_providers::limiter::is_local;
use cv_providers::manager::WireExtras;
use cv_providers::{CompletionRequest, FinishReason, ImagePart, ProviderError, ProviderManager};
use cv_search::prepare::localized_image_md5;
use cv_state::ImageDescription;
use regex::Regex;
use serde::Serialize;
use serde_json::json;

use super::agent;
use super::params::{check_keys, get_bool, get_pos_int, get_str};
use crate::steps::{StepCtx, StepError, StepModule, StepOutcome};

const DEFAULT_LIMIT: usize = 100;
/// Crucible `IMAGE_DESCRIBE_MAX_LONG_EDGE` (`imageDescribe.ts:43`).
pub(crate) const MAX_LONG_EDGE: u32 = 1568;
/// Crucible `IMAGE_DESCRIBE_DEGENERATE_MAX_EXTRACTION_CHARS` (`imageDescribe.ts:61`).
const DEGENERATE_MAX_EXTRACTION_CHARS: usize = 20_000;
/// Crucible `FAILURE_MESSAGE_MAX_CHARS` (`imageDescribe.ts:65`).
const FAILURE_MAX_CHARS: usize = 500;
const DEFAULT_MAX_TOKENS: u32 = 4096;
/// Local per-pass caps (Crucible `shared.ts:99-108`; FINDINGS §3: narrative max 219 tokens).
const LOCAL_NARRATIVE_CAP: u32 = 512;
const LOCAL_EXTRACTION_CAP: u32 = 2048;
/// The old (pre-round-16) failure text of a pass cut off at its cap; such records are retried.
const LEGACY_LENGTH_FAILURE: &str = "finish reason \"length\"; output discarded";
const DEFAULT_PROVIDER: &str = "local";
/// Media a note may embed as an image (localize's image set plus svg/avif).
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "avif"];

/// `IMAGE_DESCRIPTION_NARRATIVE_PROMPT` (`shared.ts:96`).
const NARRATIVE_PROMPT: &str = "In one dense paragraph, describe what this image is (chart, diagram, photo, table, screenshot, etc.), its title or subject, and the trend or point it makes. Plain prose only, no JSON, no markdown formatting.";
/// `IMAGE_DESCRIPTION_EXTRACTION_PROMPT` (`shared.ts:98`).
const EXTRACTION_PROMPT: &str = "Transcribe this image as structured plain text: its title, subtitle, axis labels and ranges, series names, data values, annotations, table content, and component labels, in that order where present. Plain text only, no JSON, no markdown formatting.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Note,
    Vault,
}

pub(crate) struct Params {
    scope: Scope,
    limit: usize,
    prune_transient: bool,
}

pub(crate) fn parse(t: &toml::Table) -> Result<Params, String> {
    check_keys(t, StepKind::ImageDescribe, "image_describe")?;
    let scope = match get_str(t, "scope")?.as_deref() {
        None | Some("note") => Scope::Note,
        Some("vault") => Scope::Vault,
        Some(s) => {
            return Err(format!(
                "image_describe step: `scope` {s:?} is not note|vault"
            ));
        }
    };
    let limit = get_pos_int(t, "limit")
        .map_err(|e| format!("image_describe step: {e}"))?
        .unwrap_or(DEFAULT_LIMIT);
    Ok(Params {
        scope,
        limit,
        prune_transient: get_bool(t, "prune_transient")?.unwrap_or(false),
    })
}

/// The `image_describe` step (unknown keys disarm, I5), run through the shared provider manager.
pub(crate) struct ImageDescribe;

impl StepModule for ImageDescribe {
    type Params = Params;

    fn parse(_: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<Params, String> {
        parse(t)
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        if !ctx.config.pipelines().image_describe.enabled {
            return Ok(disabled());
        }
        run(&p, ctx, agent::manager()?)
    }
}

fn disabled() -> StepOutcome {
    let mut out = StepOutcome::default();
    out.set_vars(&Disabled {
        skipped: "disabled",
    });
    out
}

/// `image_describe` outcome vars when the pipeline is disabled.
#[derive(Serialize)]
struct Disabled {
    skipped: &'static str,
}

/// Crucible `fnv1a` (`imageDescriptionStore.ts:114-121`): 32-bit FNV-1a over UTF-16 code units,
/// lowercase hex padded to 8.
pub(crate) fn fnv1a32_hex(s: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for u in s.encode_utf16() {
        h ^= u32::from(u);
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

fn static_re(cell: &'static OnceLock<Regex>, src: &str) -> &'static Regex {
    cell.get_or_init(|| match Regex::new(src) {
        Ok(r) => r,
        // Literal patterns covered by tests; an invalid one is a programming error.
        Err(e) => panic!("invalid built-in regex {src:?}: {e}"),
    })
}

fn decode_entities(s: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = static_re(&R, r"&#x([0-9a-fA-F]+);|&#(\d+);");
    let s = s
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'");
    let s = re.replace_all(&s, |c: &regex::Captures<'_>| -> String {
        let n = match (c.get(1), c.get(2)) {
            (Some(h), _) => u32::from_str_radix(h.as_str(), 16).ok(),
            (_, Some(d)) => d.as_str().parse().ok(),
            _ => None,
        };
        n.and_then(char::from_u32)
            .map_or_else(|| c[0].to_owned(), String::from)
    });
    s.replace("&amp;", "&")
}

/// Crucible `extractSvgText`: `<title>`, `<desc>`, `<text>` contents in document order, nested
/// tags stripped, entities decoded, whitespace collapsed, one line per element.
pub(crate) fn extract_svg_text(svg: &str) -> String {
    static ELEM: OnceLock<Regex> = OnceLock::new();
    static TAGS: OnceLock<Regex> = OnceLock::new();
    // No backreference in `regex`: the closing tag matches any of the three names.
    let re = static_re(
        &ELEM,
        r"(?is)<(title|desc|text)\b[^>]*>(.*?)</(?:title|desc|text)>",
    );
    let tags = static_re(&TAGS, r"<[^>]*>");
    re.captures_iter(svg)
        .filter_map(|c| {
            let inner = tags.replace_all(c.get(2).map_or("", |m| m.as_str()), " ");
            let line = decode_entities(&inner)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            (!line.is_empty()).then_some(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Shrink-only fit of `(w, h)` to a [`MAX_LONG_EDGE`] long edge, aspect preserved (rounded,
/// never below 1 px).
pub(crate) fn fit_long_edge(w: u32, h: u32) -> (u32, u32) {
    let long = w.max(h);
    if long <= MAX_LONG_EDGE {
        return (w, h);
    }
    let scale = |x: u32| {
        let v = (u64::from(x) * u64::from(MAX_LONG_EDGE) + u64::from(long) / 2) / u64::from(long);
        u32::try_from(v).unwrap_or(MAX_LONG_EDGE).max(1)
    };
    (scale(w), scale(h))
}

/// The bytes and mime sent to the model: shrink-only downscale to a [`MAX_LONG_EDGE`] long edge
/// (aspect preserved), re-encoded as PNG when resized or when the source is not png/jpeg.
/// `Err` = undecodable or unsupported (avif included): an image-specific, permanent failure.
pub(crate) fn prepare_image(bytes: &[u8]) -> Result<(String, Vec<u8>), String> {
    let format = image::guess_format(bytes).map_err(|e| format!("unsupported image: {e}"))?;
    let img = image::load_from_memory_with_format(bytes, format)
        .map_err(|e| format!("image decode failed: {e}"))?;
    let target = fit_long_edge(img.width(), img.height());
    let resize = target != (img.width(), img.height());
    let passthrough = match format {
        image::ImageFormat::Png => Some("image/png"),
        image::ImageFormat::Jpeg => Some("image/jpeg"),
        _ => None,
    };
    if !resize && let Some(mime) = passthrough {
        return Ok((mime.to_owned(), bytes.to_vec()));
    }
    let img = if resize {
        img.resize_exact(target.0, target.1, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)
        .map_err(|e| format!("image re-encode failed: {e}"))?;
    Ok(("image/png".to_owned(), out.into_inner()))
}

fn truncate(s: &str) -> String {
    s.chars().take(FAILURE_MAX_CHARS).collect()
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Provenance for vision/failed records.
struct Model<'a> {
    provider_id: &'a str,
    model_id: &'a str,
}

/// Builds a record with Crucible's field names in `raw` (`imageDescriptionStore.ts:36-60`).
fn record(
    md5: &str,
    kind: &str,
    narrative: &str,
    extraction: &str,
    model: Option<&Model<'_>>,
    failure: Option<&str>,
) -> ImageDescription {
    let described_at = now_iso();
    let description_hash = fnv1a32_hex(&format!("{narrative}\n{extraction}"));
    let mut raw = json!({
        "md5": md5,
        "narrative": narrative,
        "extraction": extraction,
        "kind": kind,
        "describedAt": described_at,
        "schemaVersion": 1,
        "descriptionHash": description_hash,
    });
    if let Some(m) = model {
        raw["providerId"] = json!(m.provider_id);
        raw["modelId"] = json!(m.model_id);
    }
    if let Some(f) = failure {
        raw["failure"] = json!(truncate(f));
        raw["failureClass"] = json!("permanent");
    }
    ImageDescription {
        md5: md5.to_owned(),
        kind: kind.to_owned(),
        narrative: narrative.to_owned(),
        extraction: extraction.to_owned(),
        description_hash,
        described_at,
        raw: raw.to_string(),
    }
}

fn is_image(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, e)| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Candidate image paths in deterministic order.
fn candidates(ctx: &StepCtx<'_>, scope: Scope) -> Result<Vec<String>, StepError> {
    let reader = ctx.index()?;
    let q = reader.query();
    let mut seen = BTreeSet::new();
    let paths: Vec<String> = match scope {
        Scope::Note => {
            let note = ctx.context.active_path.clone().ok_or_else(|| {
                StepError::failed("image_describe step: scope = note needs an active note")
            })?;
            q.outgoing(&note)?
                .into_iter()
                .filter_map(|l| l.resolved_path)
                .collect()
        }
        Scope::Vault => q.file_paths()?.into_iter().map(|(_, p)| p).collect(),
    };
    Ok(paths
        .into_iter()
        .filter(|p| is_image(p) && seen.insert(p.clone()))
        .collect())
}

/// Outcome of one vision pass that completed or failed image-specifically.
enum Pass {
    Text(String),
    ImageFailure(String),
}

/// One completion; provider-level failures map through [`agent::disposition`] (audit M1) and
/// abort the step without a record; only image-specific failures come back as
/// [`Pass::ImageFailure`].
fn pass(
    mgr: &ProviderManager,
    provider: &cv_providers::ProviderConfig,
    model: &str,
    prompt: &str,
    image: &ImagePart,
    max_tokens: u32,
    extras: &WireExtras,
) -> Result<Pass, StepError> {
    let handle =
        tokio::runtime::Handle::try_current().map_err(|_| StepError::failed("no async runtime"))?;
    let req = CompletionRequest {
        system: String::new(),
        user: prompt.to_owned(),
        max_tokens: Some(max_tokens),
        images: vec![image.clone()],
        extras: extras.clone(),
    };
    match handle.block_on(mgr.complete(provider, model, req)) {
        Ok(c) if c.finish_reason == FinishReason::Length => Ok(length_pass(&c.text)),
        Ok(c) if c.finish_reason != FinishReason::Stop => Ok(Pass::ImageFailure(format!(
            "finish reason {:?}; output discarded",
            c.raw_finish_reason.as_deref().unwrap_or("missing")
        ))),
        Ok(c) if c.text.trim().is_empty() => {
            Ok(Pass::ImageFailure("model returned an empty answer".into()))
        }
        Ok(c) => Ok(Pass::Text(c.text.trim().to_owned())),
        Err(ProviderError::Http { api, status, .. })
            if (400..500).contains(&status) && status != 408 && status != 429 =>
        {
            // The body is not kept: a server may echo the request (image bytes).
            Ok(Pass::ImageFailure(format!("{api} returned HTTP {status}")))
        }
        Err(e) => Err(agent::provider_failure(&provider.id, &e)),
    }
}

/// A pass that ended at its token cap: a non-degenerate body is kept truncated; an empty body
/// (reasoning ate the budget) or a repetition loop is an image failure. These texts deliberately
/// differ from [`LEGACY_LENGTH_FAILURE`] so a re-failed image is not retried forever.
fn length_pass(text: &str) -> Pass {
    let t = text.trim();
    if t.is_empty() {
        Pass::ImageFailure("token cap reached with no content".into())
    } else if is_repetition_loop(t) {
        Pass::ImageFailure("token cap reached in a repetition loop; output discarded".into())
    } else {
        tracing::info!(
            chars = t.len(),
            "image_describe: storing a cap-truncated pass"
        );
        Pass::Text(t.to_owned())
    }
}

/// Minimum line / word counts before a repetition verdict (short bodies cannot loop).
const LOOP_MIN_LINES: usize = 8;
const LOOP_MIN_WORDS: usize = 64;
/// Unique-fraction ceiling for a loop. FINDINGS §1: looping extractions had 0–16% unique lines,
/// genuinely long output >= 56%; 35% sits in the gap.
const LOOP_MAX_UNIQUE_PERCENT: usize = 35;

/// Whether a cap-truncated body is a degenerate repetition loop. Two signals, either suffices:
/// the share of unique non-empty lines (multi-line loops) and, for bodies without enough lines,
/// the share of unique word 4-grams (a loop inside one paragraph). Both need a minimum size so a
/// short answer is never judged.
pub(crate) fn is_repetition_loop(text: &str) -> bool {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() >= LOOP_MIN_LINES {
        let unique: BTreeSet<&str> = lines.iter().copied().collect();
        return unique.len() * 100 < lines.len() * LOOP_MAX_UNIQUE_PERCENT;
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < LOOP_MIN_WORDS {
        return false;
    }
    let grams: Vec<&[&str]> = words.windows(4).collect();
    let unique: BTreeSet<&[&str]> = grams.iter().copied().collect();
    unique.len() * 100 < grams.len() * LOOP_MAX_UNIQUE_PERCENT
}

/// A `failed` record that the pre-round-16 wire produced by a `length` finish: describable again.
fn is_legacy_length_failure(rec: &ImageDescription) -> bool {
    rec.kind == "failed"
        && serde_json::from_str::<serde_json::Value>(&rec.raw)
            .ok()
            .and_then(|v| v.get("failure")?.as_str().map(str::to_owned))
            .is_some_and(|f| f.contains(LEGACY_LENGTH_FAILURE))
}

/// A `failed` record without cv provenance (`providerId` absent from `raw`): a Crucible-era
/// failure, retried once under the current wire. A re-failure is recorded with `providerId`.
fn is_unattributed_failure(rec: &ImageDescription) -> bool {
    rec.kind == "failed"
        && serde_json::from_str::<serde_json::Value>(&rec.raw)
            .ok()
            .is_some_and(|v| v.get("providerId").is_none_or(serde_json::Value::is_null))
}

/// Whether the pending scan describes an existing record again (it never overwrites any other).
pub(crate) fn is_retryable(rec: &ImageDescription) -> bool {
    is_legacy_length_failure(rec) || is_unattributed_failure(rec)
}

/// The per-provider describe wire: extra body fields and the two per-pass caps.
#[derive(Debug, Clone, PartialEq)]
struct DescribeWire {
    extras: WireExtras,
    narrative_cap: u32,
    extraction_cap: u32,
}

/// Local providers get Crucible's settings plus the measured sampler (module header); remote
/// providers get no extras and the single configured cap. `override_cap` is
/// `[image_describe] max_tokens` (> 0).
fn describe_wire(
    provider: &cv_providers::ProviderConfig,
    override_cap: Option<u32>,
) -> DescribeWire {
    if is_local(provider) {
        DescribeWire {
            extras: WireExtras {
                reasoning_effort: Some("none".into()),
                temperature: Some(1.0),
                top_p: Some(0.95),
                top_k: Some(64),
                repeat_penalty: Some(1.15),
                repeat_last_n: Some(256),
            },
            narrative_cap: override_cap.unwrap_or(LOCAL_NARRATIVE_CAP),
            extraction_cap: override_cap.unwrap_or(LOCAL_EXTRACTION_CAP),
        }
    } else {
        let cap = override_cap.unwrap_or(DEFAULT_MAX_TOKENS);
        DescribeWire {
            extras: WireExtras::default(),
            narrative_cap: cap,
            extraction_cap: cap,
        }
    }
}

/// `image_describe` outcome vars.
#[derive(Default, Serialize)]
struct Counts {
    described: u64,
    svg: u64,
    failed: u64,
    skipped: u64,
    pruned: u64,
    remaining: u64,
}

/// The step body, with the manager injected (tests pass one with a test `SecretSource`).
pub(crate) fn run(
    p: &Params,
    ctx: &mut StepCtx<'_>,
    mgr: &ProviderManager,
) -> Result<StepOutcome, StepError> {
    let cfg = ctx.config.pipelines().image_describe.clone();
    if !cfg.enabled {
        return Ok(disabled());
    }
    let model = cfg
        .model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .ok_or_else(|| {
            StepError::failed("image_describe: pipelines.toml [image_describe] `model` is not set")
        })?;
    let provider_id = cfg
        .provider
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_PROVIDER.to_owned());
    let provs = agent::providers(ctx.config).map_err(StepError::failed)?;
    let provider = provs
        .get(&provider_id)
        .ok_or_else(|| {
            StepError::failed(format!("image_describe: unknown provider {provider_id:?}"))
        })?
        .clone();
    let wire = describe_wire(&provider, cfg.max_tokens.filter(|n| *n > 0));
    let prov = Model {
        provider_id: &provider.id,
        model_id: &model,
    };

    let mut n = Counts::default();
    if p.prune_transient {
        let mut st = ctx.state();
        let a = st.prune_transient_failed_image_descriptions()?;
        let b = st.prune_degenerate_image_descriptions(DEGENERATE_MAX_EXTRACTION_CHARS)?;
        n.pruned = (a + b) as u64;
    }

    // Pending images: `_MD5`-named, not yet recorded (any kind — poison-skip), deduped by md5.
    let mut pending: Vec<(String, String)> = Vec::new();
    {
        let paths = candidates(ctx, p.scope)?;
        let st = ctx.state();
        let mut md5s = BTreeSet::new();
        for path in paths {
            let Some((md5, _)) = localized_image_md5(&path) else {
                n.skipped += 1;
                continue;
            };
            if !md5s.insert(md5.clone()) {
                n.skipped += 1;
                continue;
            }
            // Any record poison-skips, except an old `length` failure or a failure without cv
            // provenance (retried, overwritten).
            if st
                .image_description(&md5)?
                .is_some_and(|r| !is_retryable(&r))
            {
                n.skipped += 1;
                continue;
            }
            pending.push((md5, path));
        }
    }
    if pending.len() > p.limit {
        n.remaining = (pending.len() - p.limit) as u64;
        pending.truncate(p.limit);
    }

    for (md5, path) in pending {
        let Some((bytes, _)) = ctx.read(&path)? else {
            n.skipped += 1;
            continue;
        };
        let rec = if path.to_ascii_lowercase().ends_with(".svg") {
            let text = extract_svg_text(&String::from_utf8_lossy(&bytes));
            n.svg += 1;
            record(&md5, "svg-text", "", &text, None, None)
        } else {
            match describe(mgr, &provider, &model, &bytes, &wire)? {
                Ok((narrative, extraction)) => {
                    n.described += 1;
                    record(&md5, "vision", &narrative, &extraction, Some(&prov), None)
                }
                Err(failure) => {
                    n.failed += 1;
                    tracing::warn!(%failure, "image_describe: recording permanent failure");
                    record(&md5, "failed", "", "", Some(&prov), Some(&failure))
                }
            }
        };
        let mut st = ctx.state();
        st.put_image_description(&rec)?;
    }

    let mut out = StepOutcome::default();
    out.set_vars(&n);
    Ok(out)
}

/// Both passes for one raster. Outer `Err` aborts the step (no record); inner `Err` is an
/// image-specific failure to record.
fn describe(
    mgr: &ProviderManager,
    provider: &cv_providers::ProviderConfig,
    model: &str,
    bytes: &[u8],
    wire: &DescribeWire,
) -> Result<Result<(String, String), String>, StepError> {
    let (mime, data) = match prepare_image(bytes) {
        Ok(x) => x,
        Err(e) => return Ok(Err(e)),
    };
    let part = ImagePart {
        mime,
        base64: base64::engine::general_purpose::STANDARD.encode(data),
    };
    let narrative = match pass(
        mgr,
        provider,
        model,
        NARRATIVE_PROMPT,
        &part,
        wire.narrative_cap,
        &wire.extras,
    )? {
        Pass::Text(t) => t,
        Pass::ImageFailure(f) => return Ok(Err(format!("narrative: {f}"))),
    };
    let extraction = match pass(
        mgr,
        provider,
        model,
        EXTRACTION_PROMPT,
        &part,
        wire.extraction_cap,
        &wire.extras,
    )? {
        Pass::Text(t) => t,
        Pass::ImageFailure(f) => return Ok(Err(format!("extraction: {f}"))),
    };
    Ok(Ok((narrative, extraction)))
}

#[cfg(test)]
#[path = "image_describe_tests.rs"]
mod tests;
