//! WP-1 spike (plan abstract-removal-confidence-deletion): does a span-confidence threshold delete
//! only what the user removed? Throwaway. Aggregates only: no note text, names or model replies
//! are printed or written. Modes: `list`, `dry`, `run <out.jsonl>`, `report <out.jsonl>`.

#![allow(
    clippy::needless_range_loop,
    clippy::type_complexity,
    clippy::map_identity
)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cv_providers::manager::WireExtras;
use cv_providers::{
    CompletionRequest, FinishReason, ProviderManager, ProvidersConfig, SystemSecrets,
    parse_model_ref,
};
use serde_json::{Value, json};

const VAULT: &str = "~/vault";
const CONF: &str = "~/vault/_custom/crucible-vault";
const MAILDIR: &str = "~/Downloads";
const THR: [f64; 4] = [0.7, 0.8, 0.9, 0.95];
const CAP: usize = 40;
const MAX_RETRIES: usize = 5;

fn squeeze(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Cand {
    note_id: i64,
    pubi: usize,
    pre_body: String,
    cur_body: String,
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap())
}

/// Message-ID -> eml path over `<maildir>/cur` and `<maildir>/processed` (first wins).
fn message_map() -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    for d in ["cur", "processed"] {
        let mut files: Vec<PathBuf> = std::fs::read_dir(Path::new(MAILDIR).join(d))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        files.sort();
        for f in files {
            if let Some(post) = std::fs::read(&f)
                .ok()
                .and_then(|b| cv_ingest::mail::parse_eml(&b))
            {
                map.entry(post.message_id).or_insert(f);
            }
        }
    }
    map
}

/// The same composition `track-mail` uses: raw markdown then `clean_body`.
fn pre_body(post: &cv_ingest::mail::MailPost) -> String {
    let html = post
        .html
        .clone()
        .or_else(|| post.text.clone())
        .unwrap_or_default();
    let raw = if post.html.is_some() {
        cv_ingest::html_to_markdown(&cv_ingest::mail::strip_html_chrome(&html))
    } else {
        html.trim().to_owned()
    };
    cv_ingest::mail::clean_body(&raw)
}

struct Qual {
    cands: Vec<Cand>,
    labelled_notes: usize,
    unmapped: usize,
    not_mail: usize,
    no_eml: usize,
    pubs: usize,
}

fn qualify() -> Qual {
    let db = home().join(".local/state/crucible-vault/state.db");
    let sql = "SELECT l.note_id, COALESCE(r.path, n.path), l.publication \
        FROM removal_labels l JOIN note_ids n ON n.id = l.note_id \
        LEFT JOIN note_ids r ON r.id = n.replaced_by AND r.retired_ms IS NULL \
        WHERE l.origin = 'fold' AND (n.retired_ms IS NULL OR r.id IS NOT NULL) \
        GROUP BY l.note_id ORDER BY l.note_id";
    let out = std::process::Command::new("sqlite3")
        .args(["-readonly", "-separator", "\t"])
        .arg(&db)
        .arg(sql)
        .output()
        .expect("sqlite3");
    assert!(out.status.success(), "sqlite3 failed");
    let all_ids = std::process::Command::new("sqlite3")
        .args(["-readonly"])
        .arg(&db)
        .arg("SELECT COUNT(DISTINCT note_id) FROM removal_labels WHERE origin='fold'")
        .output()
        .expect("sqlite3");
    let labelled_notes: usize = String::from_utf8_lossy(&all_ids.stdout)
        .trim()
        .parse()
        .unwrap();
    let rows = String::from_utf8_lossy(&out.stdout).into_owned();
    let map = message_map();
    let mut q = Qual {
        cands: vec![],
        labelled_notes,
        unmapped: 0,
        not_mail: 0,
        no_eml: 0,
        pubs: 0,
    };
    q.unmapped = labelled_notes;
    let mut by_pub: BTreeMap<String, Vec<(i64, String, String)>> = BTreeMap::new();
    for line in rows.lines() {
        let mut it = line.splitn(3, '\t');
        let (Some(id), Some(path), Some(pubn)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        q.unmapped -= 1;
        let Ok(text) = std::fs::read_to_string(Path::new(VAULT).join(path)) else {
            q.unmapped += 1;
            continue;
        };
        let Some(r) = cv_note::span(&text) else {
            q.not_mail += 1;
            continue;
        };
        let fm = &text[..r.end];
        let via_email = fm.lines().any(|l| l.trim() == "via: email");
        let mid = fm.lines().find_map(|l| {
            l.strip_prefix("message-id:").and_then(|v| {
                serde_json::from_str::<String>(v.trim())
                    .ok()
                    .or_else(|| Some(v.trim().to_owned()))
            })
        });
        if !via_email {
            q.not_mail += 1;
            continue;
        }
        let Some(mid) = mid else {
            q.no_eml += 1;
            continue;
        };
        let Some(eml) = map.get(&mid) else {
            q.no_eml += 1;
            continue;
        };
        let Some(post) = std::fs::read(eml)
            .ok()
            .and_then(|b| cv_ingest::mail::parse_eml(&b))
        else {
            q.no_eml += 1;
            continue;
        };
        by_pub.entry(pubn.to_owned()).or_default().push((
            id.parse().unwrap(),
            pre_body(&post),
            text[r.end..].to_owned(),
        ));
    }
    q.pubs = by_pub.len();
    // Deterministic spread: publications in name order, notes in note_id order, round-robin.
    let mut groups: Vec<(usize, Vec<(i64, String, String)>)> = by_pub
        .into_values()
        .enumerate()
        .map(|(i, v)| (i, v))
        .collect();
    let mut round = 0;
    loop {
        let mut any = false;
        for (pi, g) in &mut groups {
            if q.cands.len() >= CAP {
                break;
            }
            if round < g.len() {
                any = true;
                let (id, pre, cur) = g[round].clone();
                q.cands.push(Cand {
                    note_id: id,
                    pubi: *pi,
                    pre_body: pre,
                    cur_body: cur,
                });
            }
        }
        if !any || q.cands.len() >= CAP {
            break;
        }
        round += 1;
    }
    q.cands.sort_by_key(|c| c.note_id);
    q
}

#[derive(Clone, Copy, PartialEq)]
enum Truth {
    Kept,
    Removed,
    Undecided,
}

fn truth(pre: &[String], cur_body: &str) -> Vec<Truth> {
    let cur: BTreeSet<String> = cur_body
        .lines()
        .map(squeeze)
        .filter(|s| !s.is_empty())
        .collect();
    // Lines inside a still-present `[!removed]` fold: not yet decided by the user.
    let mut folded: BTreeSet<String> = BTreeSet::new();
    let mut in_fold = false;
    for l in cur_body.lines() {
        let t = l.trim_start();
        if t.starts_with('>') {
            let inner = t.trim_start_matches(['>', ' ']);
            if inner.starts_with("[!removed]") {
                in_fold = true;
            } else if in_fold {
                folded.insert(squeeze(inner));
            }
        } else {
            in_fold = false;
        }
    }
    pre.iter()
        .map(|s| {
            if cur.contains(s) {
                Truth::Kept
            } else if folded.contains(s) {
                Truth::Undecided
            } else {
                Truth::Removed
            }
        })
        .collect()
}

struct ParsedSpan {
    first: String,
    last: String,
    conf: Option<f64>,
}

fn parse_answer(text: &str) -> Option<(Vec<ParsedSpan>, Option<String>)> {
    let a = text.find('{')?;
    let z = text.rfind('}')?;
    if z < a {
        return None;
    }
    let v: Value = serde_json::from_str(&text[a..=z]).ok()?;
    let obj = v.as_object()?;
    let mut spans = Vec::new();
    if let Some(Value::Array(arr)) = obj.get("spans") {
        for it in arr {
            let Some(o) = it.as_object() else { continue };
            let (Some(_k), Some(f), Some(l)) = (
                o.get("kind").and_then(Value::as_str),
                o.get("first_line").and_then(Value::as_str),
                o.get("last_line").and_then(Value::as_str),
            ) else {
                continue;
            };
            let conf = o
                .get("confidence")
                .and_then(Value::as_f64)
                .filter(|c| c.is_finite() && (0.0..=1.0).contains(c));
            spans.push(ParsedSpan {
                first: f.to_owned(),
                last: l.to_owned(),
                conf,
            });
        }
    }
    let ce = match obj.get("content_ends_after") {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        _ => None,
    };
    Some((spans, ce))
}

fn bucket(c: Option<f64>) -> usize {
    match c {
        None => 5,
        Some(c) if c < 0.7 => 0,
        Some(c) if c < 0.8 => 1,
        Some(c) if c < 0.9 => 2,
        Some(c) if c < 0.95 => 3,
        Some(_) => 4,
    }
}

/// `enrich/pass.rs::resolve_fold`, with per-line claim bookkeeping. Returns resolved/unresolved
/// span counts and per-line (min span confidence, claimed by content_ends_after).
fn resolve(
    spans: &[ParsedSpan],
    ce: &Option<String>,
    sq: &[String],
) -> (usize, usize, Vec<Option<f64>>, Vec<bool>, BTreeSet<usize>) {
    let mut fold: BTreeSet<usize> = BTreeSet::new();
    let mut minc: Vec<Option<f64>> = vec![None; sq.len()];
    let mut ce_claim = vec![false; sq.len()];
    let (mut ok, mut bad) = (0, 0);
    for span in spans {
        let (a, b) = (squeeze(&span.first), squeeze(&span.last));
        let found = (!a.is_empty() && !b.is_empty())
            .then(|| {
                let pair = |s: usize| (s..sq.len()).find(|&e| sq[e] == b).map(|e| (s, e));
                let starts = || (0..sq.len()).filter(|&i| sq[i] == a);
                starts()
                    .filter(|i| !fold.contains(i))
                    .find_map(pair)
                    .or_else(|| starts().find_map(pair))
            })
            .flatten();
        match found {
            Some((s, e)) => {
                ok += 1;
                fold.extend(s..=e);
                let c = span.conf.unwrap_or(-1.0);
                for i in s..=e {
                    minc[i] = Some(minc[i].map_or(c, |m: f64| m.min(c)));
                }
            }
            None => bad += 1,
        }
    }
    if let Some(end) = ce {
        let a = squeeze(end);
        let at = (!a.is_empty())
            .then(|| {
                (0..sq.len())
                    .rev()
                    .find(|&i| sq[i] == a)
                    .or_else(|| (0..sq.len()).rev().find(|&i| sq[i].starts_with(&a)))
            })
            .flatten();
        if let Some(i) = at {
            let nb: Vec<usize> = (i + 1..sq.len()).filter(|&j| !sq[j].is_empty()).collect();
            if let (Some(&f), Some(&l)) = (nb.first(), nb.last()) {
                for j in f..=l {
                    fold.insert(j);
                    ce_claim[j] = true;
                }
            }
        }
    }
    (ok, bad, minc, ce_claim, fold)
}

fn record(idx: usize, c: &Cand, status: &str, answer: Option<&str>) -> Value {
    let sq: Vec<String> = c.pre_body.lines().map(squeeze).collect();
    let tr = truth(&sq, &c.cur_body);
    let nonblank: Vec<usize> = (0..sq.len()).filter(|&i| !sq[i].is_empty()).collect();
    let count = |t: Truth| nonblank.iter().filter(|&&i| tr[i] == t).count();
    let rem_link = nonblank
        .iter()
        .filter(|&&i| tr[i] == Truth::Removed && (sq[i].contains("](") || sq[i].contains("![")))
        .count();
    let mut rec = json!({
        "idx": idx, "pubi": c.pubi, "status": status,
        "nb": nonblank.len(), "kept": count(Truth::Kept), "removed": count(Truth::Removed),
        "undecided": count(Truth::Undecided), "rem_link": rem_link,
    });
    let parsed = answer.and_then(parse_answer);
    if let Some((spans, ce)) = parsed {
        let mut buckets = [0usize; 6];
        for s in &spans {
            buckets[bucket(s.conf)] += 1;
        }
        let (ok, bad, minc, ce_claim, fold) = resolve(&spans, &ce, &sq);
        let fold_nb = fold.iter().filter(|&&i| !sq[i].is_empty()).count();
        let guard = fold_nb * 2 > nonblank.len();
        // [thr][variant: 0 = content_ends_after vetoes, 1 = excluded] -> [lines, false, removed]
        let mut over = vec![];
        for t in THR {
            let mut row = vec![];
            for veto in [true, false] {
                let (mut n, mut f, mut r) = (0, 0, 0);
                for &i in &nonblank {
                    let hit = minc[i].is_some_and(|m| m >= t) && !(veto && ce_claim[i]);
                    if hit {
                        n += 1;
                        match tr[i] {
                            Truth::Kept => f += 1,
                            Truth::Removed => r += 1,
                            Truth::Undecided => {}
                        }
                    }
                }
                row.push(json!([n, f, r]));
            }
            over.push(Value::Array(row));
        }
        rec["spans_total"] = json!(spans.len());
        rec["resolved"] = json!(ok);
        rec["unresolved"] = json!(bad);
        rec["buckets"] = json!(buckets);
        rec["guard"] = json!(guard);
        rec["over"] = Value::Array(over);
    }
    rec
}

struct Live {
    provs: ProvidersConfig,
    model_ref: String,
    max_tokens: u32,
    input_t: String,
    prompt: String,
}

fn live(prompt_path: &Path) -> Live {
    let agents: toml::Table =
        toml::from_str(&std::fs::read_to_string(format!("{CONF}/agents.toml")).unwrap()).unwrap();
    let def = agents
        .get("intake-enrich")
        .and_then(|v| v.as_table())
        .unwrap();
    let provs = ProvidersConfig::from_toml(
        &std::fs::read_to_string(format!("{CONF}/providers.toml")).unwrap(),
    )
    .unwrap();
    Live {
        provs,
        model_ref: def["model"].as_str().unwrap().to_owned(),
        max_tokens: u32::try_from(def["max_tokens"].as_integer().unwrap()).unwrap(),
        input_t: def["input"].as_str().unwrap().to_owned(),
        // The daemon reads `prompt_file`; the spike substitutes the pinned release prompt.
        prompt: std::fs::read_to_string(prompt_path).unwrap(),
    }
}

/// The daemon's rendering (`steps/agent.rs::call`): `input` rendered with `{{content}}` = the note
/// body after the frontmatter, then the prompt with `{{value}}` = that; system empty.
fn render_user(l: &Live, body: &str) -> String {
    let render = |t: &str, lookup: &dyn Fn(&str) -> Option<String>| {
        cv_text::template::render(t, |k| Ok(lookup(k.trim()))).unwrap_or_default()
    };
    let value = render(&l.input_t, &|k| match k {
        "content" => Some(body.to_owned()),
        _ => None,
    });
    render(&l.prompt, &|k| match k {
        "value" | "input" => Some(value.clone()),
        "content" => Some(body.to_owned()),
        _ => None,
    })
}

fn daemon_body(pre: &str) -> String {
    // `captured_note`: closing fence line, one blank line, the body, a newline.
    format!("\n{pre}\n")
}

fn hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map_or("", String::as_str);
    let prompt_path =
        PathBuf::from(std::env::var("PROMPT").unwrap_or_else(|_| "intake-enrich-prompt.md".into()));
    match mode {
        "list" => {
            let q = qualify();
            println!(
                "labelled_fold_notes={} unreadable_or_unmapped={} not_mail={} no_eml={} qualifying_pubs={} sample={}",
                q.labelled_notes,
                q.unmapped,
                q.not_mail,
                q.no_eml,
                q.pubs,
                q.cands.len()
            );
            for c in &q.cands {
                let sq: Vec<String> = c.pre_body.lines().map(squeeze).collect();
                let tr = truth(&sq, &c.cur_body);
                let nb = sq.iter().filter(|s| !s.is_empty()).count();
                let k = (0..sq.len())
                    .filter(|&i| !sq[i].is_empty() && tr[i] == Truth::Kept)
                    .count();
                println!("pub#{} nonblank={nb} kept={k}", c.pubi);
            }
        }
        "dry" => {
            let l = live(&prompt_path);
            let user = render_user(&l, &daemon_body("PLACEHOLDER BODY"));
            let r = parse_model_ref(Some(&l.model_ref)).unwrap();
            let p = l.provs.get(&r.provider_id).unwrap();
            let w = cv_providers::wire::build_completion(
                p,
                Some("DUMMY"),
                &r.model_id,
                "",
                &user,
                &[],
                l.max_tokens,
            )
            .unwrap();
            let keys: Vec<&String> = w.body.as_object().unwrap().keys().collect();
            let hnames: Vec<&String> = w.headers.iter().map(|(k, _)| k).collect();
            println!(
                "provider={} kind={:?} model={} max_tokens={} url={}",
                p.id, p.kind, r.model_id, l.max_tokens, w.url
            );
            println!("body_keys={keys:?} headers={hnames:?}");
            println!(
                "system_len={} user_prefix_ok={} user_suffix_ok={}",
                w.body
                    .get("system")
                    .map_or(0, |s| s.as_str().map_or(1, str::len)),
                user.starts_with(&l.prompt[..40]),
                user.ends_with("PLACEHOLDER BODY\n\n")
            );
        }
        "run" => {
            let out = PathBuf::from(&args[2]);
            let l = live(&prompt_path);
            let q = qualify();
            if q.cands.len() < 10 {
                println!("HOLD: only {} notes qualify", q.cands.len());
                return;
            }
            let done: BTreeSet<usize> = std::fs::read_to_string(&out)
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .filter_map(|v| v["idx"].as_u64().map(|x| x as usize))
                .collect();
            let secrets = SystemSecrets {
                dir: cv_providers::secrets::default_secrets_dir().unwrap(),
            };
            let mgr = ProviderManager::new(Arc::new(secrets)).unwrap();
            let r = parse_model_ref(Some(&l.model_ref)).unwrap();
            let p = l.provs.get(&r.provider_id).unwrap().clone();
            let rt = tokio::runtime::Runtime::new().unwrap();
            let mut retries = 0usize;
            let mut calls = 0usize;
            for (idx, c) in q.cands.iter().enumerate() {
                if done.contains(&idx) {
                    continue;
                }
                let user = render_user(&l, &daemon_body(&c.pre_body));
                let mut status = "call_failed".to_owned();
                let mut answer: Option<String> = None;
                loop {
                    if calls >= CAP + MAX_RETRIES {
                        break;
                    }
                    calls += 1;
                    let req = CompletionRequest {
                        system: String::new(),
                        user: user.clone(),
                        max_tokens: Some(l.max_tokens),
                        images: Vec::new(),
                        extras: WireExtras::default(),
                    };
                    match rt.block_on(mgr.complete(&p, &r.model_id, req)) {
                        Ok(c) if c.finish_reason == FinishReason::Stop => {
                            status = "ok".into();
                            eprintln!(
                                "note {idx}: reply len={} hash={:x}",
                                c.text.len(),
                                hash(&c.text)
                            );
                            answer = Some(c.text);
                            break;
                        }
                        Ok(_) => {
                            eprintln!("note {idx}: non-stop finish");
                            break;
                        }
                        Err(e) => {
                            eprintln!("note {idx}: call error {:?}", e.failure());
                            if retries < MAX_RETRIES {
                                retries += 1;
                                continue;
                            }
                            break;
                        }
                    }
                }
                if status == "ok" && answer.as_deref().and_then(parse_answer).is_none() {
                    status = "unparseable".into();
                }
                let rec = record(idx, c, &status, answer.as_deref());
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&out)
                    .unwrap();
                writeln!(f, "{rec}").unwrap();
                eprintln!("note {idx}: {status} calls={calls} retries={retries}");
            }
            println!("done calls={calls} retries={retries}");
        }
        "report" => report(&PathBuf::from(&args[2])),
        _ => eprintln!("usage: conf-spike list|dry|run <out>|report <out>"),
    }
}

fn report(out: &Path) {
    let recs: Vec<Value> = std::fs::read_to_string(out)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let u = |v: &Value, k: &str| v[k].as_u64().unwrap_or(0);
    let run = recs.len();
    let failed = recs.iter().filter(|r| r["status"] == "call_failed").count();
    let unparse = recs.iter().filter(|r| r["status"] == "unparseable").count();
    let ok: Vec<&Value> = recs.iter().filter(|r| r.get("over").is_some()).collect();
    println!(
        "notes_run={run} call_failed={failed} unparseable={unparse} analysed={}",
        ok.len()
    );
    let sum = |k: &str| ok.iter().map(|r| u(r, k)).sum::<u64>();
    println!(
        "nonblank={} kept={} removed={} undecided={} removed_with_link_or_image={}",
        sum("nb"),
        sum("kept"),
        sum("removed"),
        sum("undecided"),
        sum("rem_link")
    );
    println!(
        "spans={} resolved={} unresolved={} guard_refused_notes={}",
        sum("spans_total"),
        sum("resolved"),
        sum("unresolved"),
        ok.iter().filter(|r| r["guard"] == true).count()
    );
    let mut b = [0u64; 6];
    for r in &ok {
        for (i, x) in b.iter_mut().enumerate() {
            *x += r["buckets"][i].as_u64().unwrap_or(0);
        }
    }
    println!(
        "buckets <0.7={} 0.7-0.8={} 0.8-0.9={} 0.9-0.95={} >=0.95={} missing/malformed={}",
        b[0], b[1], b[2], b[3], b[4], b[5]
    );
    let removed_total = sum("removed") as f64;
    for (variant, vname) in [
        (0usize, "content_ends_after vetoes"),
        (1, "content_ends_after excluded"),
    ] {
        for guarded in [false, true] {
            println!(
                "-- variant: {vname}; half-body guard {}",
                if guarded { "applied" } else { "not applied" }
            );
            println!(
                "thr  lines  false  false_notes  removed_deleted  share_removed  false/deleted  rule_ok"
            );
            for (ti, t) in THR.iter().enumerate() {
                let (mut n, mut f, mut r, mut fnotes) = (0u64, 0u64, 0u64, 0u64);
                for rec in &ok {
                    if guarded && rec["guard"] == true {
                        continue;
                    }
                    let cell = &rec["over"][ti][variant];
                    n += cell[0].as_u64().unwrap_or(0);
                    let fl = cell[1].as_u64().unwrap_or(0);
                    f += fl;
                    r += cell[2].as_u64().unwrap_or(0);
                    fnotes += u64::from(fl > 0);
                }
                let share = if removed_total > 0.0 {
                    r as f64 / removed_total
                } else {
                    0.0
                };
                let decided = f + r;
                let rate = if decided > 0 {
                    f as f64 / decided as f64
                } else {
                    0.0
                };
                let rule = f * 200 <= decided && share >= 0.5;
                println!(
                    "{t:<4} {n:<6} {f:<6} {fnotes:<12} {r:<16} {share:<14.3} {rate:<14.4} {rule}"
                );
            }
        }
    }
}
