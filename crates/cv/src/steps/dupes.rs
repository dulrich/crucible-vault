//! `dupes_scan` (plan `round-12-feedback-todos` WP-6, Decisions 1, 2, 4, 5): the on-demand
//! duplicate scan behind the `dupes-scan` action. It labels candidate pairs and never removes
//! anything. It is note-mutating (`mutates_note`).
//!
//! **Signals (Decision 4).** A note's identity is recomputed from `source` with the daemon's
//! configured `PostIdOptions` (`post_id_from_url`, which falls back to the trimmed URL); the
//! stored `post-id` is used only when there is no `source` (16.4% of stored ids are stale, spike
//! `docs/spikes/postid-dupes/`). Containment is `cv_text::similar` shingle containment, both
//! directions. [`score`] is the one-pair scorer WP-8 reuses; [`PairScore::keep`] is the scan's
//! aggressive threshold (identity match, or max directional containment >= [`KEEP_CONTAINMENT`]).
//!
//! **Exclusions (Decision 1).** Notes under `.trash/`, `initiatives/` and `_yt_metadata/` are not
//! scanned; `daily × daily` is never a pair; a `_raw_transcript × daily` pair with matching
//! identity (the designed pairing) is dropped; a pair already carrying each other in
//! `not-duplicate-of` (WP-7) is `suppressed`.
//!
//! **Candidates, exact and uncapped.** Containment pairs are found by prefix filtering, which is
//! exact for a threshold: if `|A ∩ B| >= ceil(t·|A|)` then `B` shares at least one of `A`'s
//! `|A| − ceil(t·|A|) + 1` rarest shingles. Every note probes the inverted index with that prefix
//! only, and every probed pair is verified exactly, so no candidate at containment >= t is lost
//! (no df or bucket cap, unlike the round-11 spike). Notes with fewer than [`MIN_SHINGLES`]
//! shingles take part through identity only.
//!
//! **Memory.** Shingles are hashed to `u32`. Pass 1 collects every note's hashes once to find the
//! ones occurring in two or more notes (a shingle in one note can never be shared); pass 2
//! re-reads each note and keeps only those shared hashes (as indices) plus its full count, in
//! one flat array; the inverted index is a CSR over the shared hashes. Nothing per-note is a
//! `HashSet`.
//!
//! **Output.** Each kept pair is written into both notes' `possible-duplicate` (a list of
//! `[[path]]` wikilinks; an entry is matched by resolved target, so a rerun writes nothing new)
//! through [`StepCtx::modify`]. `dry_run` (step param, or job param `dry_run = true`) computes
//! every count and writes nothing. Vars are aggregates only: `notes`, `notes_shingled`,
//! `verified`, `pairs`, `pairs_identity`, `pairs_containment`, `pairs_c90`,
//! `pairs_c90_identity`, `written` (notes changed), `links_added`, `already_labelled`,
//! `suppressed`, `conflicts`, `dry_run`, and `by_folder` (kept pairs by top-folder pair).

use std::collections::{BTreeMap, HashMap};

use cv_actions::{ConfigSet, StepKind};
use cv_note::{FrontmatterDoc, JsValue};
use cv_text::resolver::Resolver;
use cv_text::similar::Shingles;
use cv_text::url_canon::{PostIdOptions, build_blog_canon_host_map, post_id_from_url};
use serde::Serialize;
use serde_json::Value;

use super::params::{check_keys, get_bool};
use super::{StepCtx, StepError, StepModule, StepOutcome};

/// The scan keeps a pair at or above this max directional containment (aggressive, Decision 2).
pub(crate) const KEEP_CONTAINMENT: f64 = 0.7;
/// Notes with fewer shingles are compared by identity only (short texts match too easily).
pub(crate) const MIN_SHINGLES: usize = 20;
/// The frontmatter key the scan writes.
pub(crate) const POSSIBLE_KEY: &str = "possible-duplicate";
/// The frontmatter key a "not duplicates" verdict writes (WP-7); it suppresses the pair.
pub(crate) const NOT_DUP_KEY: &str = "not-duplicate-of";

/// Top folders never scanned (Decision 1).
const EXCLUDED_TOPS: [&str; 3] = [".trash", "initiatives", "_yt_metadata"];

/// The `dupes_scan` step.
pub(crate) struct DupesScan;

impl StepModule for DupesScan {
    type Params = bool;

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<bool, String> {
        check_keys(t, kind, "dupes_scan")?;
        Ok(get_bool(t, "dry_run")?.unwrap_or(false))
    }

    fn run(dry_run: bool, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        let job_dry = match ctx.params.get("dry_run") {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
            _ => false,
        };
        run(dry_run || job_dry, ctx)
    }
}

// ---- scorer (reused by WP-8) ------------------------------------------------------------------

/// One pair's signals: identity match plus both directional containments.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub(crate) struct PairScore {
    /// Both notes have an identity and they are equal.
    pub identity_match: bool,
    /// |A ∩ B| / |A|.
    pub containment_a_in_b: f64,
    /// |A ∩ B| / |B|.
    pub containment_b_in_a: f64,
}

impl PairScore {
    /// The larger directional containment (the scan's symmetric labelling score).
    pub(crate) fn max_containment(&self) -> f64 {
        self.containment_a_in_b.max(self.containment_b_in_a)
    }

    /// The scan's keep rule: identity match, or max containment >= [`KEEP_CONTAINMENT`].
    pub(crate) fn keep(&self) -> bool {
        self.identity_match || self.max_containment() >= KEEP_CONTAINMENT
    }
}

/// The daemon's configured post-id options (the blog `canon` host rules, as `signals.rs`).
pub(crate) fn post_id_options(config: &ConfigSet) -> PostIdOptions {
    PostIdOptions {
        method: None,
        host_rules: build_blog_canon_host_map(
            config
                .sources()
                .blog
                .iter()
                .map(|b| (b.link.as_str(), b.canon.method())),
        ),
    }
}

/// A note's identity: `post_id_from_url(source)` when it has a `source`, else its stored
/// `post-id`, else none. The stored id never wins over `source` (it may be stale).
pub(crate) fn identity(doc: &FrontmatterDoc, opts: &PostIdOptions) -> Option<String> {
    let nonempty = |k: &str| {
        doc.get_str(k)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    if let Some(src) = nonempty("source") {
        return Some(post_id_from_url(&src, opts)).filter(|s| !s.is_empty());
    }
    nonempty("post-id")
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "WP-8 (the _blog_metadata backfill) calls it")
)]
/// Scores one pair of notes from their full texts (frontmatter is split off; containment is
/// over the bodies, exact `u64` shingles).
pub(crate) fn score(a_text: &str, b_text: &str, opts: &PostIdOptions) -> PairScore {
    let (a, b) = (FrontmatterDoc::new(a_text), FrontmatterDoc::new(b_text));
    let (ia, ib) = (identity(&a, opts), identity(&b, opts));
    let (sa, sb) = (Shingles::from_text(a.body()), Shingles::from_text(b.body()));
    PairScore {
        identity_match: ia.is_some() && ia == ib,
        containment_a_in_b: sa.containment_in(&sb),
        containment_b_in_a: sb.containment_in(&sa),
    }
}

// ---- scan --------------------------------------------------------------------------------------

/// Outcome vars (aggregates only).
#[derive(Default, Serialize)]
struct Vars {
    notes: u32,
    notes_shingled: u32,
    verified: u64,
    pairs: u32,
    pairs_identity: u32,
    pairs_containment: u32,
    pairs_c90: u32,
    pairs_c90_identity: u32,
    written: u32,
    links_added: u32,
    already_labelled: u32,
    suppressed: u32,
    conflicts: u32,
    dry_run: bool,
    by_folder: BTreeMap<String, u32>,
}

/// One scanned note.
struct Note {
    id: i64,
    path: String,
    top: String,
    identity: Option<String>,
    not_dup: Vec<i64>,
    labelled: Vec<i64>,
    /// Full shingle count (0 = below [`MIN_SHINGLES`], identity only).
    n: u32,
}

/// The top folder of `path` (`""` at the vault root).
fn top_of(path: &str) -> &str {
    path.split_once('/').map_or("", |(t, _)| t)
}

/// A wikilink (or bare path) entry's target: `[[a/b|alias]]` → `a/b`.
fn link_target(s: &str) -> &str {
    let t = s.trim();
    let t = t.strip_prefix("[[").unwrap_or(t);
    let t = t.strip_suffix("]]").unwrap_or(t);
    let t = t.split('|').next().unwrap_or(t);
    t.split('#').next().unwrap_or(t).trim()
}

/// The resolved ids of `key`'s wikilink entries in the note `path` (`id`).
fn linked_ids(doc: &FrontmatterDoc, key: &str, r: &Resolver, path: &str, id: i64) -> Vec<i64> {
    let mut v: Vec<i64> = doc
        .get_strings(key)
        .iter()
        .map(|s| link_target(s))
        .filter(|t| !t.is_empty())
        .filter_map(|t| r.resolve(path, id, t))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// The note's body shingle hashes, truncated to `u32` (`None` below [`MIN_SHINGLES`]).
fn hashes(text: &str) -> Option<Vec<u32>> {
    let doc = FrontmatterDoc::new(text);
    let sh = Shingles::from_text(doc.body());
    if sh.len() < MIN_SHINGLES {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    let mut v: Vec<u32> = sh.hashes().map(|h| (h ^ (h >> 32)) as u32).collect();
    v.sort_unstable();
    v.dedup();
    Some(v)
}

/// Decision 1 pair exclusions (folder-level ones are applied before scanning).
fn excluded(a: &str, b: &str, identity_match: bool) -> bool {
    if a == "daily" && b == "daily" {
        return true;
    }
    identity_match
        && matches!(
            (a, b),
            ("_raw_transcript", "daily") | ("daily", "_raw_transcript")
        )
}

fn read_text(ctx: &StepCtx<'_>, path: &str) -> Result<Option<String>, StepError> {
    Ok(ctx.read(path)?.and_then(|(b, _)| String::from_utf8(b).ok()))
}

/// Compact shingle store: per note, sorted indices into `shared` (hashes in two or more notes).
struct Store {
    off: Vec<usize>,
    data: Vec<u32>,
}

impl Store {
    fn sig(&self, i: usize) -> &[u32] {
        &self.data[self.off[i]..self.off[i + 1]]
    }

    /// |sig(a) ∩ sig(b)| by sorted merge.
    fn inter(&self, a: usize, b: usize) -> u32 {
        let (x, y) = (self.sig(a), self.sig(b));
        let (mut i, mut j, mut n) = (0, 0, 0u32);
        while i < x.len() && j < y.len() {
            match x[i].cmp(&y[j]) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    n += 1;
                    i += 1;
                    j += 1;
                }
            }
        }
        n
    }
}

#[allow(clippy::cast_precision_loss)]
fn ratio(num: u32, den: u32) -> f64 {
    if den == 0 {
        0.0
    } else {
        f64::from(num) / f64::from(den)
    }
}

/// The scan.
fn run(dry: bool, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let opts = post_id_options(ctx.config);
    let files = ctx
        .index()?
        .query()
        .file_paths()
        .map_err(|e| StepError::failed(format!("index: {e}")))?;
    let mut resolver = Resolver::default();
    for (id, p) in &files {
        resolver.add(*id, p);
    }
    let eligible: Vec<(i64, String)> = files
        .into_iter()
        .filter(|(_, p)| {
            p.to_ascii_lowercase().ends_with(".md") && !EXCLUDED_TOPS.contains(&top_of(p))
        })
        .collect();

    // Pass 1: metadata + every shingle hash once (to find the shared ones).
    let mut notes: Vec<Note> = Vec::with_capacity(eligible.len());
    let mut all: Vec<u32> = Vec::new();
    for (id, path) in eligible {
        let Some(text) = read_text(ctx, &path)? else {
            continue;
        };
        let doc = FrontmatterDoc::new(text.as_str());
        let h = hashes(&text);
        #[allow(clippy::cast_possible_truncation)]
        let n = h.as_ref().map_or(0, |v| v.len() as u32);
        if let Some(v) = h {
            all.extend(v);
        }
        notes.push(Note {
            id,
            top: top_of(&path).to_owned(),
            identity: identity(&doc, &opts),
            not_dup: linked_ids(&doc, NOT_DUP_KEY, &resolver, &path, id),
            labelled: linked_ids(&doc, POSSIBLE_KEY, &resolver, &path, id),
            path,
            n,
        });
    }
    all.sort_unstable();
    let mut shared: Vec<u32> = Vec::new();
    for w in all.chunk_by(|a, b| a == b) {
        if w.len() >= 2 {
            shared.push(w[0]);
        }
    }
    drop(all);

    // Pass 2: per note, the sorted shared-hash indices (its full count stays `n`).
    let mut store = Store {
        off: vec![0],
        data: Vec::new(),
    };
    for note in &mut notes {
        if note.n > 0 {
            match read_text(ctx, &note.path)?.as_deref().and_then(hashes) {
                Some(v) => {
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        note.n = v.len() as u32;
                    }
                    let start = store.data.len();
                    #[allow(clippy::cast_possible_truncation)]
                    store.data.extend(
                        v.iter()
                            .filter_map(|h| shared.binary_search(h).ok().map(|i| i as u32)),
                    );
                    store.data[start..].sort_unstable();
                }
                None => note.n = 0,
            }
        }
        store.off.push(store.data.len());
    }

    // Inverted index (CSR) over shared hashes: note indices per hash.
    let mut post_off = vec![0usize; shared.len() + 1];
    for &s in &store.data {
        post_off[s as usize + 1] += 1;
    }
    for i in 0..shared.len() {
        post_off[i + 1] += post_off[i];
    }
    let mut fill = post_off.clone();
    #[allow(clippy::cast_possible_truncation)]
    let mut postings = vec![0u32; store.data.len()];
    for i in 0..notes.len() {
        for &s in store.sig(i) {
            postings[fill[s as usize]] = i as u32;
            fill[s as usize] += 1;
        }
    }
    drop(fill);
    let df = |s: u32| post_off[s as usize + 1] - post_off[s as usize];

    let mut vars = Vars {
        dry_run: dry,
        ..Vars::default()
    };
    #[allow(clippy::cast_possible_truncation)]
    {
        vars.notes = notes.len() as u32;
    }
    let ident_eq = |a: &Note, b: &Note| a.identity.is_some() && a.identity == b.identity;
    let mut kept: BTreeMap<(u32, u32), PairScore> = BTreeMap::new();
    let consider =
        |a: usize, b: usize, kept: &mut BTreeMap<(u32, u32), PairScore>, v: &mut Vars| {
            let (a, b) = (a.min(b), a.max(b));
            #[allow(clippy::cast_possible_truncation)]
            let key = (a as u32, b as u32);
            if kept.contains_key(&key) {
                return;
            }
            let (na, nb) = (&notes[a], &notes[b]);
            let identity_match = ident_eq(na, nb);
            if excluded(&na.top, &nb.top, identity_match) {
                return;
            }
            let inter = if na.n > 0 && nb.n > 0 {
                v.verified += 1;
                store.inter(a, b)
            } else {
                0
            };
            let s = PairScore {
                identity_match,
                containment_a_in_b: ratio(inter, na.n),
                containment_b_in_a: ratio(inter, nb.n),
            };
            if s.keep() {
                kept.insert(key, s);
            }
        };

    // Containment candidates: each note probes its rarest-shingle prefix.
    let mut cand: Vec<u32> = Vec::new();
    for (a, note) in notes.iter().enumerate() {
        let n = note.n as usize;
        if n == 0 {
            continue;
        }
        vars.notes_shingled += 1;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let need = ((KEEP_CONTAINMENT * n as f64) - 1e-9).ceil().max(1.0) as usize;
        let prefix = n - need + 1;
        let sig = store.sig(a);
        let unique = n - sig.len();
        if unique >= prefix {
            continue;
        }
        let mut by_df: Vec<u32> = sig.to_vec();
        by_df.sort_unstable_by_key(|&s| df(s));
        cand.clear();
        for &s in &by_df[..prefix - unique] {
            cand.extend(
                postings[post_off[s as usize]..post_off[s as usize + 1]]
                    .iter()
                    .copied()
                    .filter(|&b| b as usize != a),
            );
        }
        cand.sort_unstable();
        cand.dedup();
        for &b in &cand {
            consider(a, b as usize, &mut kept, &mut vars);
        }
    }

    // Identity groups.
    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, n) in notes.iter().enumerate() {
        if let Some(id) = &n.identity {
            groups.entry(id.as_str()).or_default().push(i);
        }
    }
    let groups: Vec<Vec<usize>> = groups.into_values().filter(|g| g.len() >= 2).collect();
    for g in &groups {
        for (k, &a) in g.iter().enumerate() {
            for &b in &g[k + 1..] {
                consider(a, b, &mut kept, &mut vars);
            }
        }
    }

    // Suppression, labelling plan, counts.
    let mut additions: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (&(a, b), s) in &kept {
        let (a, b) = (a as usize, b as usize);
        let (na, nb) = (&notes[a], &notes[b]);
        if na.not_dup.binary_search(&nb.id).is_ok() || nb.not_dup.binary_search(&na.id).is_ok() {
            vars.suppressed += 1;
            continue;
        }
        vars.pairs += 1;
        if s.identity_match {
            vars.pairs_identity += 1;
        } else {
            vars.pairs_containment += 1;
        }
        if s.max_containment() >= 0.9 {
            vars.pairs_c90 += 1;
            if s.identity_match {
                vars.pairs_c90_identity += 1;
            }
        }
        let (fa, fb) = if na.top <= nb.top {
            (&na.top, &nb.top)
        } else {
            (&nb.top, &na.top)
        };
        let label = |t: &str| {
            if t.is_empty() {
                "(root)".to_owned()
            } else {
                t.to_owned()
            }
        };
        *vars
            .by_folder
            .entry(format!("{} × {}", label(fa), label(fb)))
            .or_default() += 1;
        let a_has = na.labelled.binary_search(&nb.id).is_ok();
        let b_has = nb.labelled.binary_search(&na.id).is_ok();
        if a_has && b_has {
            vars.already_labelled += 1;
        }
        if !a_has {
            additions.entry(a).or_default().push(b);
        }
        if !b_has {
            additions.entry(b).or_default().push(a);
        }
    }

    for (i, others) in additions {
        #[allow(clippy::cast_possible_truncation)]
        let added = others.len() as u32;
        if dry {
            vars.written += 1;
            vars.links_added += added;
            continue;
        }
        let links: Vec<JsValue> = others
            .iter()
            .map(|&o| {
                let p = &notes[o].path;
                JsValue::Str(format!("[[{}]]", p.strip_suffix(".md").unwrap_or(p)))
            })
            .collect();
        let path = notes[i].path.clone();
        match ctx.modify(&path, |old| {
            let mut d = FrontmatterDoc::new(old);
            let mut changed = false;
            for l in &links {
                changed |= d.append_to_list(POSSIBLE_KEY, l)?;
            }
            Ok(changed.then(|| d.into_text()))
        }) {
            Ok(Some(_)) => {
                vars.written += 1;
                vars.links_added += added;
            }
            Ok(None) => {}
            Err(e) if e.is_conflict() => vars.conflicts += 1,
            Err(e) => return Err(e),
        }
    }

    let mut out = StepOutcome::default();
    out.set_vars(&vars);
    Ok(out)
}
