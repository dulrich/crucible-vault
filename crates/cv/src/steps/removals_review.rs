//! Step module `removals_review` (plan `round-23-daemon-memory-mail-enrich-fanfic` WP-6): collects
//! the user's labels on enrichment removals and scores them. The removed text is note content an
//! agent must not read, so the user ticks a sampled review note and cv reports aggregates only.
//!
//! | key | values | default |
//! |---|---|---|
//! | `mode` | `"review"` (sample the folds, write the review note) \| `"score"` (read the ticks) | `"review"` |
//! | `limit` | review: spans sampled (> 0; a job param `limit` wins) | `100` |
//! | `seed` | review: sampling seed (> 0; a job param `seed` wins) | `1` |
//! | `overwrite` | review: replace an existing note of the same date (a job param `overwrite` wins) | `false` |
//! | `path` | score: the review note (a job param `path` wins) | the newest `_crucible/reviews/removals-*.md` |
//!
//! **Review.** The population is every removed span (`cv_ingest::enrich::removed_spans`, the
//! reader of the fold `cv_ingest::enrich` writes) of every enriched note (non-empty `enriched`)
//! outside `.trash/` and `_`/`.`-prefixed top folders, typed by [`classify`]: `post-id` is a blog,
//! `message-id` mail, a YouTube `source` youtube, anything else a clip (the same predicates as
//! `intake_enrich`'s backfill). [`allocate`] splits `limit` over the types proportionally (at least
//! one per non-empty type while the limit allows) and [`sample`] picks each type's spans by a
//! seeded hash (FNV-1a over the seed, path and span index, finished with splitmix64; no `rand`), so
//! the same seed over the same corpus gives the same sample. One note is created at
//! `_crucible/reviews/removals-<YYYY-MM-DD>.md` (job clock date); an existing one is an error unless
//! `overwrite` (the user's ticks are never clobbered silently). Items read
//! `- [ ] [[<path>]] (<type>) %%rm <type>%%` followed by the span as indented `> ` lines (capped at
//! [`MAX_QUOTE_LINES`], then `> …`). Outcome vars: `review_path`, `open_path`, `sampled`,
//! `sampled_<type>`, `population`, `population_<type>`, `seed`.
//!
//! **Score.** Counts the `%%rm <type>%%` items and the ticked (`[x]`/`[X]`) ones;
//! precision = 1 − ticked/items. Outcome vars only: `items`, `ticked`, `precision` (3 decimals),
//! `items_<type>`, `ticked_<type>`, `precision_<type>`. Nothing is written.

use std::collections::{BTreeMap, BTreeSet};

use cv_actions::{ConfigSet, StepKind};
use cv_note::FrontmatterDoc;
use cv_text::url_canon::is_youtube_video;
use cv_text::vpath;
use serde_json::{Value, json};

use super::materialize::now;
use super::params::{check_keys, get_bool, get_pos_int, get_str};
use super::{StepCtx, StepError, StepModule, StepOutcome, job_flag, job_pos_int};

/// Where review notes live.
const REVIEW_DIR: &str = "_crucible/reviews";
/// Filename stem of a review note.
const REVIEW_PREFIX: &str = "removals-";
const DEFAULT_LIMIT: usize = 100;
const DEFAULT_SEED: u64 = 1;
/// Lines of a span quoted in the review note before `> …`.
pub(crate) const MAX_QUOTE_LINES: usize = 8;
/// Instruction line of a review note.
const INSTRUCTION: &str = "Tick = this text should NOT have been removed.";

/// The intake types, in report order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    Blog,
    Mail,
    Youtube,
    Clip,
}

impl Kind {
    pub(crate) const ALL: [Kind; 4] = [Kind::Blog, Kind::Mail, Kind::Youtube, Kind::Clip];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Kind::Blog => "blog",
            Kind::Mail => "mail",
            Kind::Youtube => "youtube",
            Kind::Clip => "clip",
        }
    }
}

/// The pure intake classifier: `post-id` → blog, `message-id` → mail, a YouTube `source` URL →
/// youtube, anything else → clip.
pub(crate) fn classify(has_post_id: bool, has_message_id: bool, source: &str) -> Kind {
    if has_post_id {
        Kind::Blog
    } else if has_message_id {
        Kind::Mail
    } else if is_youtube_video(source) {
        Kind::Youtube
    } else {
        Kind::Clip
    }
}

/// How many of each type's spans to sample: `pops[i]` is type i's population. At most `limit` in
/// total; everything when the population fits; otherwise proportional (largest remainder) with at
/// least one per non-empty type while `limit` allows (largest populations first when it does
/// not); never above a population.
pub(crate) fn allocate(pops: &[usize], limit: usize) -> Vec<usize> {
    let total: usize = pops.iter().sum();
    if total <= limit {
        return pops.to_vec();
    }
    let mut quota = vec![0usize; pops.len()];
    let mut order: Vec<usize> = (0..pops.len()).filter(|&i| pops[i] > 0).collect();
    if limit < order.len() {
        order.sort_by_key(|&i| (std::cmp::Reverse(pops[i]), i));
        for &i in order.iter().take(limit) {
            quota[i] = 1;
        }
        return quota;
    }
    let (t, l) = (
        i128::try_from(total).unwrap_or(0),
        i128::try_from(limit).unwrap_or(0),
    );
    for &i in &order {
        let p = i128::try_from(pops[i]).unwrap_or(0);
        quota[i] = usize::try_from(l * p / t).unwrap_or(1).clamp(1, pops[i]);
    }
    // Deficit of type i: its exact share minus its quota, scaled by `total`.
    let deficit = |i: usize, q: usize| -> i128 {
        i128::try_from(pops[i]).unwrap_or(0) * l - i128::try_from(q).unwrap_or(0) * t
    };
    loop {
        let sum: usize = quota.iter().sum();
        if sum == limit {
            return quota;
        }
        if sum < limit {
            let pick = order
                .iter()
                .copied()
                .filter(|&i| quota[i] < pops[i])
                .max_by_key(|&i| (deficit(i, quota[i]), std::cmp::Reverse(i)));
            match pick {
                Some(i) => quota[i] += 1,
                None => return quota,
            }
        } else {
            let pick = order
                .iter()
                .copied()
                .filter(|&i| quota[i] > 1)
                .min_by_key(|&i| (deficit(i, quota[i]), i));
            match pick {
                Some(i) => quota[i] -= 1,
                None => return quota,
            }
        }
    }
}

/// splitmix64 finalizer.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The deterministic pick key of span `idx` of `path` under `seed`: FNV-1a 64 over
/// `seed ‖ path ‖ idx`, finished with splitmix64.
fn pick_key(seed: u64, path: &str, idx: usize) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let bytes = seed
        .to_le_bytes()
        .into_iter()
        .chain(path.bytes())
        .chain([0u8])
        .chain((idx as u64).to_le_bytes());
    for b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    mix(h)
}

/// One removed span of one note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Span {
    pub path: String,
    pub idx: usize,
    pub text: String,
}

/// `k` of `spans` (given in path order), chosen by [`pick_key`] under `seed` and returned in the
/// input order.
pub(crate) fn sample(spans: &[Span], k: usize, seed: u64) -> Vec<Span> {
    let mut keyed: Vec<(u64, usize)> = spans
        .iter()
        .enumerate()
        .map(|(n, s)| (pick_key(seed, &s.path, s.idx), n))
        .collect();
    keyed.sort_unstable();
    let mut chosen: Vec<usize> = keyed.into_iter().take(k).map(|(_, n)| n).collect();
    chosen.sort_unstable();
    chosen.into_iter().map(|n| spans[n].clone()).collect()
}

/// A span quoted as indented `> ` lines, capped at [`MAX_QUOTE_LINES`] (then `> …`).
fn quote(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = lines
        .iter()
        .take(MAX_QUOTE_LINES)
        .map(|l| format!("  > {l}"))
        .collect();
    if lines.len() > MAX_QUOTE_LINES {
        out.push("  > …".to_owned());
    }
    out.join("\n")
}

/// The review note text for the sampled spans (`(type, span)` pairs, in report order).
pub(crate) fn render_review(date: &str, items: &[(Kind, Span)]) -> String {
    let mut out = format!("# Removals review {date}\n\n{INSTRUCTION}\n\n");
    for (kind, s) in items {
        let note = s.path.strip_suffix(".md").unwrap_or(&s.path);
        let t = kind.label();
        out.push_str(&format!(
            "- [ ] [[{note}]] ({t}) %%rm {t}%%\n{}\n",
            quote(&s.text)
        ));
    }
    out
}

/// One parsed review item: its type label and whether it is ticked.
fn parse_item(line: &str) -> Option<(String, bool)> {
    let rest = line.trim_start().strip_prefix("- [")?;
    let mut chars = rest.chars();
    let ticked = match chars.next()? {
        ' ' => false,
        'x' | 'X' => true,
        _ => return None,
    };
    let rest = chars.as_str().strip_prefix(']')?;
    let tail = rest.trim_end().strip_suffix("%%")?;
    let at = tail.rfind("%%rm ")?;
    let label = tail[at + "%%rm ".len()..].trim();
    (!label.is_empty() && !label.contains(char::is_whitespace)).then(|| (label.to_owned(), ticked))
}

/// Per-type (items, ticked) of a review note's text.
pub(crate) fn tally(text: &str) -> BTreeMap<String, (u32, u32)> {
    let mut m: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for (label, ticked) in text.lines().filter_map(parse_item) {
        let e = m.entry(label).or_default();
        e.0 += 1;
        e.1 += u32::from(ticked);
    }
    m
}

/// `1 − ticked/items` with 3 decimals; `None` without items.
pub(crate) fn precision(items: u32, ticked: u32) -> Option<String> {
    (items > 0).then(|| format!("{:.3}", 1.0 - f64::from(ticked) / f64::from(items)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Review,
    Score,
}

/// Parsed step params.
#[derive(Debug, Clone)]
pub(crate) struct Params {
    mode: Mode,
    limit: usize,
    seed: u64,
    overwrite: bool,
    path: Option<String>,
}

/// The `removals_review` step.
pub(crate) struct RemovalsReview;

impl StepModule for RemovalsReview {
    type Params = Params;

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<Params, String> {
        check_keys(t, kind, "removals_review")?;
        let mode = match get_str(t, "mode")?.as_deref() {
            None | Some("review") => Mode::Review,
            Some("score") => Mode::Score,
            Some(s) => {
                return Err(format!(
                    "removals_review step: `mode` {s:?} is not review|score"
                ));
            }
        };
        let (limit, seed) = (get_pos_int(t, "limit")?, get_pos_int(t, "seed")?);
        let (overwrite, path) = (get_bool(t, "overwrite")?, get_str(t, "path")?);
        if mode == Mode::Score && (limit.is_some() || seed.is_some() || overwrite.is_some()) {
            return Err(
                "removals_review step: `limit`/`seed`/`overwrite` need mode = \"review\"".into(),
            );
        }
        if mode == Mode::Review && path.is_some() {
            return Err("removals_review step: `path` needs mode = \"score\"".into());
        }
        Ok(Params {
            mode,
            limit: limit.unwrap_or(DEFAULT_LIMIT),
            seed: seed.map_or(DEFAULT_SEED, |s| s as u64),
            overwrite: overwrite.unwrap_or(false),
            path,
        })
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        match p.mode {
            Mode::Review => review(&p, ctx),
            Mode::Score => score(&p, ctx),
        }
    }
}

fn is_blank(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        _ => false,
    }
}

/// Every removed span of every enriched user note, by type, in path order.
fn population(ctx: &StepCtx<'_>) -> Result<BTreeMap<Kind, Vec<Span>>, StepError> {
    let reader = ctx.index()?;
    let q = reader.query();
    let enriched: BTreeSet<String> = q
        .md_props_with_key("enriched")?
        .into_iter()
        .filter(|r| !is_blank(&r.value))
        .map(|r| r.path)
        .filter(|p| p.ends_with(".md") && vpath::is_user_path(p))
        .collect();
    let post: BTreeSet<String> = q.md_paths_with_prop_key("post-id")?.into_iter().collect();
    let mail: BTreeSet<String> = q
        .md_paths_with_prop_key("message-id")?
        .into_iter()
        .collect();
    let mut source: BTreeMap<String, String> = BTreeMap::new();
    for r in q.md_props_with_key("source")? {
        source.insert(r.path, cv_index::prop_text(&r.value));
    }
    let mut pop: BTreeMap<Kind, Vec<Span>> = BTreeMap::new();
    for path in &enriched {
        let Some((bytes, _)) = ctx.read(path)? else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let kind = classify(
            post.contains(path),
            mail.contains(path),
            source.get(path).map_or("", String::as_str),
        );
        let spans = cv_ingest::enrich::removed_spans(FrontmatterDoc::new(&text).body());
        pop.entry(kind)
            .or_default()
            .extend(spans.into_iter().enumerate().map(|(idx, text)| Span {
                path: path.clone(),
                idx,
                text,
            }));
    }
    Ok(pop)
}

fn job_u64(ctx: &StepCtx<'_>, key: &str, default: u64) -> Result<u64, StepError> {
    match job_pos_int(ctx.params, key)
        .map_err(|e| StepError::failed(format!("removals_review: {e}")))?
    {
        Some(n) => Ok(n as u64),
        None => Ok(default),
    }
}

fn review(p: &Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let limit = job_pos_int(ctx.params, "limit")
        .map_err(|e| StepError::failed(format!("removals_review: {e}")))?
        .unwrap_or(p.limit);
    let seed = job_u64(ctx, "seed", p.seed)?;
    let overwrite = job_flag(ctx, "overwrite").unwrap_or(p.overwrite);
    let date = now().date().format("%Y-%m-%d").to_string();
    let path = format!("{REVIEW_DIR}/{REVIEW_PREFIX}{date}.md");
    let existing = ctx.read(&path)?;
    if existing.is_some() && !overwrite {
        return Err(StepError::failed(format!(
            "removals_review: {path} already exists (it may hold ticks); run with overwrite = true to replace it"
        )));
    }
    let pop = population(ctx)?;
    let pops: Vec<usize> = Kind::ALL
        .iter()
        .map(|k| pop.get(k).map_or(0, Vec::len))
        .collect();
    let quota = allocate(&pops, limit);
    let mut items: Vec<(Kind, Span)> = Vec::new();
    let mut vars = serde_json::Map::new();
    for (n, kind) in Kind::ALL.iter().enumerate() {
        let spans = pop.get(kind).map_or(&[][..], Vec::as_slice);
        let picked = sample(spans, quota[n], seed);
        vars.insert(format!("population_{}", kind.label()), json!(pops[n]));
        vars.insert(format!("sampled_{}", kind.label()), json!(picked.len()));
        items.extend(picked.into_iter().map(|s| (*kind, s)));
    }
    vars.insert("population".into(), json!(pops.iter().sum::<usize>()));
    vars.insert("sampled".into(), json!(items.len()));
    vars.insert("seed".into(), json!(seed));
    vars.insert("review_path".into(), json!(path));
    vars.insert("open_path".into(), json!(path));
    let text = render_review(&date, &items).into_bytes();
    match existing {
        Some((_, hash)) => {
            ctx.write(&path, text, Some(hash))?;
        }
        None => {
            ctx.create(&path, text)?;
        }
    }
    let mut out = StepOutcome {
        vars,
        open_path: Some(path.clone()),
        ..StepOutcome::default()
    };
    out.push_output(path);
    Ok(out)
}

/// The newest review note by path (the date in the name sorts), from the index.
fn newest_review(ctx: &StepCtx<'_>) -> Result<Option<String>, StepError> {
    let reader = ctx.index()?;
    Ok(reader
        .query()
        .md_files_under(REVIEW_DIR)?
        .into_iter()
        .map(|f| f.path)
        .filter(|p| {
            vpath::parent(p) == REVIEW_DIR
                && vpath::stem(p).starts_with(REVIEW_PREFIX)
                && p.ends_with(".md")
        })
        .max())
}

fn score(p: &Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let job_path = ctx
        .params
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let path = match job_path.or_else(|| p.path.clone()) {
        Some(path) => path,
        None => newest_review(ctx)?.ok_or_else(|| {
            StepError::failed(format!(
                "removals_review: no {REVIEW_DIR}/{REVIEW_PREFIX}*.md note found"
            ))
        })?,
    };
    let Some((bytes, _)) = ctx.read(&path)? else {
        return Err(StepError::failed(format!(
            "removals_review: {path} not found"
        )));
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| StepError::failed(format!("removals_review: {path} is not UTF-8")))?;
    let tallies = tally(&text);
    let (items, ticked) = tallies
        .values()
        .fold((0, 0), |(i, t), (a, b)| (i + a, t + b));
    let Some(overall) = precision(items, ticked) else {
        return Err(StepError::failed(format!(
            "removals_review: {path} has no review items (`- [ ] … %%rm <type>%%`)"
        )));
    };
    let mut out = StepOutcome::default();
    out.vars.insert("review_path".into(), json!(path));
    out.vars.insert("items".into(), json!(items));
    out.vars.insert("ticked".into(), json!(ticked));
    out.vars.insert("precision".into(), json!(overall));
    for (label, (i, t)) in &tallies {
        out.vars.insert(format!("items_{label}"), json!(i));
        out.vars.insert(format!("ticked_{label}"), json!(t));
        if let Some(pr) = precision(*i, *t) {
            out.vars.insert(format!("precision_{label}"), json!(pr));
        }
    }
    Ok(out)
}
