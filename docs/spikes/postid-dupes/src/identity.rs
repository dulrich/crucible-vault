//! Identity-group measurement. Aggregates only.
use crate::{split_fm, walk};
use cv_text::similar::containment;
use cv_text::url_canon::{canonicalize_url, post_id_from_url, PostIdOptions};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

const SYS: [&str; 8] = ["_blog_metadata", "_yt_metadata", "daily", "Clippings", "core", "initiatives", "_raw_transcript", "_x_metadata"];

pub fn kind(rel: &str) -> String {
    let t = rel.split('/').next().unwrap();
    if SYS.contains(&t) { t.to_string() } else { "other".into() }
}

struct N {
    rel: String,
    kind: String,
    fm: HashMap<String, String>,
    body: String,
    stored: Option<String>,
    recomputed: Option<String>,
    canon: Option<String>,
    ident: Option<String>, // stored first
    ident_re: Option<String>, // recomputed first
}

fn parse_fm(fm: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for l in fm.lines() {
        if l.starts_with(' ') || l.starts_with('-') || l.starts_with('#') { continue; }
        if let Some((k, v)) = l.split_once(':') {
            let v = v.trim().trim_matches(|c| c == '"' || c == '\'').trim();
            m.insert(k.trim().to_string(), v.to_string());
        }
    }
    m
}

fn shape(v: &str) -> String {
    v.chars().map(|c| if c.is_ascii_digit() { '9' } else { c }).take(30).collect()
}

fn date_of(n: &N) -> Option<String> {
    for k in ["created", "date", "published"] {
        if let Some(v) = n.fm.get(k) {
            if v.len() >= 10 && v.as_bytes()[4] == b'-' && v.as_bytes()[7] == b'-' { return Some(v[..10].to_string()); }
        }
    }
    None
}
fn day_num(d: &str) -> Option<i64> {
    let y: i64 = d.get(0..4)?.parse().ok()?; let m: i64 = d.get(5..7)?.parse().ok()?; let dd: i64 = d.get(8..10)?.parse().ok()?;
    // days from civil
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400); let yoe = y2 - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + dd - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe)
}
fn daily_folder(rel: &str) -> Option<String> {
    let p: Vec<&str> = rel.split('/').collect();
    if p.len() >= 4 && p[0] == "daily" && p[1] == "day" { Some(p[2].to_string()) } else { None }
}
fn pct(a: usize, b: usize) -> f64 { if b == 0 { 0.0 } else { 100.0 * a as f64 / b as f64 } }

pub fn run(vault: &str, a_tsv: &str, b_tsv: &str) {
    let root = Path::new(vault);
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files.sort();
    let opts = PostIdOptions::default();
    let mut notes: Vec<N> = Vec::new();
    let mut trash = 0usize;
    for f in &files {
        if f.starts_with(".trash/") { trash += 1; continue; }
        let Ok(t) = fs::read_to_string(root.join(f)) else { continue };
        let (fmt, body) = split_fm(&t);
        let fm = parse_fm(fmt);
        let stored = fm.get("post-id").filter(|s| !s.is_empty()).cloned();
        let src = fm.get("source").filter(|s| !s.is_empty()).cloned();
        let recomputed = src.as_ref().map(|s| post_id_from_url(s, &opts));
        let canon = src.as_ref().and_then(|s| canonicalize_url(s)).map(|c| c.canonical);
        let ident = stored.clone().or(recomputed.clone()).or(canon.clone());
        let ident_re = recomputed.clone().or(stored.clone()).or(canon.clone());
        notes.push(N { kind: kind(f), rel: f.clone(), fm, body: body.to_string(), stored, recomputed, canon, ident, ident_re });
    }
    println!("md_notes_excl_trash={} trash_md={}", notes.len(), trash);
    // key presence
    let keys = ["source", "post-id", "yt-video-id", "via", "duplicate-of", "created", "published", "date", "updated", "modified", "pub_date", "release-date"];
    println!("== key presence (count, %) overall");
    for k in keys { let c = notes.iter().filter(|n| n.fm.contains_key(k)).count(); println!("{k}: {c} ({:.1}%)", pct(c, notes.len())); }
    println!("== notes by kind");
    let mut kc: BTreeMap<&str, usize> = BTreeMap::new();
    for n in &notes { *kc.entry(&n.kind).or_default() += 1; }
    println!("{kc:?}");
    println!("== source / post-id presence by kind (n, source, post-id, yt-video-id, ident)");
    for (k, c) in &kc {
        let v: Vec<&N> = notes.iter().filter(|n| n.kind == *k).collect();
        let f = |g: &dyn Fn(&&N) -> bool| v.iter().filter(|n| g(n)).count();
        println!("{k}: n={c} source={} post-id={} yt-video-id={} ident={}", f(&|n| n.fm.contains_key("source")), f(&|n| n.stored.is_some()), f(&|n| n.fm.contains_key("yt-video-id")), f(&|n| n.ident.is_some()));
    }
    // 1 groups
    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, n) in notes.iter().enumerate() { if let Some(id) = &n.ident { groups.entry(id).or_default().push(i); } }
    let multi: Vec<&Vec<usize>> = groups.values().filter(|g| g.len() >= 2).collect();
    let nn: usize = multi.iter().map(|g| g.len()).sum();
    println!("== (1) identity groups (stored post-id first)\nnotes_with_ident={} groups_total={} groups>=2={} notes_in_them={} max_group={}", notes.iter().filter(|n| n.ident.is_some()).count(), groups.len(), multi.len(), nn, multi.iter().map(|g| g.len()).max().unwrap_or(0));
    let mut pairs_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut set_kind: BTreeMap<String, usize> = BTreeMap::new();
    for g in &multi {
        let mut ks: Vec<String> = g.iter().map(|&i| notes[i].kind.clone()).collect();
        ks.sort(); ks.dedup();
        *set_kind.entry(ks.join(" + ")).or_default() += 1;
        for x in 0..g.len() { for y in x + 1..g.len() {
            let (a, b) = (&notes[g[x]].kind, &notes[g[y]].kind);
            let (a, b) = if a <= b { (a, b) } else { (b, a) };
            *pairs_kind.entry(format!("{a} x {b}")).or_default() += 1;
        } }
    }
    let mut sv: Vec<_> = set_kind.into_iter().collect(); sv.sort_by(|a, b| b.1.cmp(&a.1));
    println!("group folder-kind sets (groups): {}", sv.iter().take(15).map(|(k, v)| format!("[{k}]={v}")).collect::<Vec<_>>().join(" "));
    let mut pv: Vec<_> = pairs_kind.into_iter().collect(); pv.sort_by(|a, b| b.1.cmp(&a.1));
    println!("within-group note pairs by folder pair: {}", pv.iter().take(15).map(|(k, v)| format!("[{k}]={v}")).collect::<Vec<_>>().join(" "));
    // stale
    let both: Vec<&N> = notes.iter().filter(|n| n.stored.is_some() && n.recomputed.is_some()).collect();
    let stale = both.iter().filter(|n| n.stored != n.recomputed).count();
    let stale_canon = both.iter().filter(|n| n.stored != n.recomputed && n.stored.as_ref() != n.canon.as_ref()).count();
    println!("notes_with_both_post-id_and_source={} stored!=recomputed={} ({:.1}%) (of those stored!=canonical-source too: {})", both.len(), stale, pct(stale, both.len()), stale_canon);
    let mut g2: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, n) in notes.iter().enumerate() { if let Some(id) = &n.ident_re { g2.entry(id).or_default().push(i); } }
    let m2: Vec<&Vec<usize>> = g2.values().filter(|g| g.len() >= 2).collect();
    let n2: usize = m2.iter().map(|g| g.len()).sum();
    println!("groups with recomputed-first identity: groups>=2={} notes_in_them={}", m2.len(), n2);
    // groups that differ: the set-of-members partition differs
    let canon_part = |gs: &Vec<&Vec<usize>>| -> HashSet<Vec<usize>> { gs.iter().map(|g| { let mut v = (*g).clone(); v.sort(); v }).collect() };
    let (p1, p2) = (canon_part(&multi), canon_part(&m2));
    println!("groups (member-sets) present under stored-first but not recomputed-first={} ; present under recomputed-first only={}", p1.difference(&p2).count(), p2.difference(&p1).count());
    // 2 blog metadata
    println!("== (2) _blog_metadata");
    let bm: Vec<usize> = (0..notes.len()).filter(|&i| notes[i].kind == "_blog_metadata").collect();
    let with_id = bm.iter().filter(|&&i| notes[i].ident.is_some()).count();
    let with_src = bm.iter().filter(|&&i| notes[i].fm.contains_key("source")).count();
    let with_pid = bm.iter().filter(|&&i| notes[i].stored.is_some()).count();
    println!("notes={} source={} post-id={} ident={} ({:.1}%)", bm.len(), with_src, with_pid, with_id, pct(with_id, bm.len()));
    let (mut twin, mut notwin, mut twin_bm_only) = (0, 0, 0);
    let (mut c9, mut c5, mut c0) = (0, 0, 0);
    let (mut d9, mut d5, mut d0, mut flip) = (0, 0, 0, 0);
    let mut twin_kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut bm_with_twin: HashSet<usize> = HashSet::new();
    let mut ncap_with_bm = 0usize;
    for &i in &bm {
        let Some(id) = &notes[i].ident else { continue };
        let caps: Vec<usize> = groups[id.as_str()].iter().copied().filter(|&j| notes[j].kind != "_blog_metadata").collect();
        let other_bm = groups[id.as_str()].iter().filter(|&&j| j != i && notes[j].kind == "_blog_metadata").count();
        if caps.is_empty() { notwin += 1; if other_bm > 0 { twin_bm_only += 1; } continue; }
        twin += 1; bm_with_twin.insert(i); ncap_with_bm += 1;
        let mut ks: Vec<String> = caps.iter().map(|&j| notes[j].kind.clone()).collect(); ks.sort(); ks.dedup();
        *twin_kinds.entry(ks.join("+")).or_default() += 1;
        // best containment among captures (metadata body in capture body), small=smaller
        let mut best = 0.0f64;
        for &j in &caps {
            let (a, b) = (&notes[i].body, &notes[j].body);
            let c = if a.len() <= b.len() { containment(a, b) } else { containment(b, a) };
            best = best.max(c);
        }
        if best >= 0.9 { c9 += 1 } else if best >= 0.5 { c5 += 1 } else { c0 += 1 }
        // Audit B1 (round-12): directional — the doomed metadata body contained in the surviving capture.
        let dir = caps.iter().map(|&j| containment(&notes[i].body, &notes[j].body)).fold(0.0f64, f64::max);
        if dir >= 0.9 { d9 += 1 } else if dir >= 0.5 { d5 += 1 } else { d0 += 1 }
        if best >= 0.9 && dir < 0.9 { flip += 1 }
    }
    let _ = ncap_with_bm;
    println!("DIRECTIONAL meta-in-capture: >=0.9={d9} 0.5-0.9={d5} <0.5={d0}; small-in-large>=0.9 but directional<0.9: {flip}");
    println!("with_identity_twin_in_non-metadata_note={twin} no_twin={notwin} (of which share identity with another _blog_metadata note only: {twin_bm_only}); without any identity={}", bm.len() - with_id);
    println!("twin capture folder kinds: {twin_kinds:?}");
    println!("containment(metadata body vs best capture body, small-in-large): >=0.9={c9} 0.5-0.9={c5} <0.5={c0} (n={twin})");
    println!("-- date-ish frontmatter keys on _blog_metadata (count of {})", bm.len());
    let mut dk: BTreeMap<String, (usize, BTreeMap<String, usize>)> = BTreeMap::new();
    for &i in &bm { for (k, v) in &notes[i].fm {
        let kl = k.to_lowercase();
        if kl.contains("date") || kl.contains("creat") || kl.contains("publi") || kl.contains("updat") || kl.contains("modif") || kl.contains("time") || kl.contains("fetch") || kl.contains("ingest") {
            let e = dk.entry(k.clone()).or_default(); if !v.is_empty() { e.0 += 1; *e.1.entry(shape(v)).or_default() += 1; }
        }
    } }
    for (k, (c, sh)) in &dk { let mut s: Vec<_> = sh.iter().collect(); s.sort_by(|a, b| b.1.cmp(a.1)); println!("{k}: filled={c} ({:.1}%) shapes={:?}", pct(*c, bm.len()), s.iter().take(3).collect::<Vec<_>>()); }
    // all bm keys
    let mut allk: BTreeMap<&str, usize> = BTreeMap::new();
    for &i in &bm { for k in notes[i].fm.keys() { *allk.entry(k).or_default() += 1; } }
    let mut ak: Vec<_> = allk.into_iter().collect(); ak.sort_by(|a, b| b.1.cmp(&a.1));
    println!("-- all frontmatter keys on _blog_metadata: {:?}", ak.iter().take(25).collect::<Vec<_>>());
    // twins' date keys: do captures carry date?
    // 3 coverage
    println!("== (3) coverage of content-similarity candidates");
    let idx: HashMap<&str, usize> = notes.iter().enumerate().map(|(i, n)| (n.rel.as_str(), i)).collect();
    let rd = |p: &str| -> HashMap<(String, String), f64> {
        let mut m = HashMap::new();
        for l in fs::read_to_string(p).unwrap().lines() {
            let v: Vec<&str> = l.split('\t').collect();
            let (a, b) = if v[0] < v[1] { (v[0], v[1]) } else { (v[1], v[0]) };
            m.insert((a.to_string(), b.to_string()), v[2].parse().unwrap());
        }
        m
    };
    let a = rd(a_tsv); let b = rd(b_tsv);
    let sets: Vec<(&str, Vec<(&String, &String)>)> = vec![
        ("containment>=0.9", a.iter().filter(|x| *x.1 >= 0.9).map(|x| (&x.0 .0, &x.0 .1)).collect()),
        ("BOTH(cont>=0.7 & cos>=0.95)", a.iter().filter(|x| *x.1 >= 0.7 && b.get(x.0).is_some_and(|c| *c >= 0.95)).map(|x| (&x.0 .0, &x.0 .1)).collect()),
    ];
    for (label, pairs) in &sets {
        let mut ex_dd = 0; let mut ex_init = 0; let mut ex_yt = 0; let mut missing = 0;
        let mut rem = 0; let mut expl = 0;
        let mut un_kinds: BTreeMap<String, usize> = BTreeMap::new();
        let mut ex_kinds: BTreeMap<String, usize> = BTreeMap::new();
        let mut un_nosrc = 0; let mut un_both_nosrc = 0; let mut un_one_nosrc = 0; let mut trash_pairs = 0;
        for (x, y) in pairs {
            let (Some(&i), Some(&j)) = (idx.get(x.as_str()), idx.get(y.as_str())) else { trash_pairs += 1; continue };
            let (ni, nj) = (&notes[i], &notes[j]);
            if ni.kind == "daily" && nj.kind == "daily" { ex_dd += 1; continue; }
            if ni.kind == "initiatives" || nj.kind == "initiatives" { ex_init += 1; continue; }
            if ni.kind == "_yt_metadata" || nj.kind == "_yt_metadata" { ex_yt += 1; continue; }
            rem += 1;
            let same = ni.ident.is_some() && ni.ident == nj.ident;
            let same_re = ni.ident_re.is_some() && ni.ident_re == nj.ident_re;
            let (ka, kb) = if ni.kind <= nj.kind { (&ni.kind, &nj.kind) } else { (&nj.kind, &ni.kind) };
            let key = format!("{ka} x {kb}");
            if same || same_re { expl += 1; *ex_kinds.entry(key).or_default() += 1; } else {
                *un_kinds.entry(key).or_default() += 1;
                let (sa, sb) = (ni.fm.contains_key("source"), nj.fm.contains_key("source"));
                if !sa || !sb { un_nosrc += 1; } if !sa && !sb { un_both_nosrc += 1; } if sa != sb { un_one_nosrc += 1; }
            }
            let _ = &mut missing;
        }
        let top = |m: &BTreeMap<String, usize>| { let mut v: Vec<_> = m.iter().collect(); v.sort_by(|a, b| b.1.cmp(a.1)); v.iter().take(8).map(|(k, c)| format!("[{k}]={c}")).collect::<Vec<_>>().join(" ") };
        println!("{label}: total_pairs={} excluded: daily x daily={ex_dd} touching initiatives={ex_init} touching _yt_metadata={ex_yt} (in that precedence) (trash/unresolved={trash_pairs}); remaining={rem}", pairs.len());
        println!("  explained by identity (same stored-first OR recomputed-first ident)={expl} ({:.1}% of remaining); unexplained={}", pct(expl, rem), rem - expl);
        println!("  explained by folder pair: {}", top(&ex_kinds));
        println!("  unexplained by folder pair: {}", top(&un_kinds));
        println!("  unexplained with >=1 side lacking `source`={un_nosrc} ({:.1}%): both lack={un_both_nosrc}, exactly one lacks={un_one_nosrc}", pct(un_nosrc, rem - expl));
    }
    // 4 reclips
    println!("== (4) re-clips: identity groups with >=2 notes outside _blog_metadata");
    let (mut ng, mut in_clip, mut diff_daily, mut spread_known) = (0, 0, 0, 0);
    let (mut s0, mut s7, mut sm) = (0, 0, 0);
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut datesrc = (0usize, 0usize);
    for g in groups.values() {
        let m: Vec<usize> = g.iter().copied().filter(|&i| notes[i].kind != "_blog_metadata").collect();
        if m.len() < 2 { continue; }
        ng += 1;
        let mut ks: Vec<String> = m.iter().map(|&i| notes[i].kind.clone()).collect(); ks.sort(); ks.dedup();
        *kinds.entry(ks.join("+")).or_default() += 1;
        if m.iter().any(|&i| notes[i].kind == "Clippings") { in_clip += 1; }
        let mut dfs: Vec<String> = m.iter().filter_map(|&i| daily_folder(&notes[i].rel)).collect(); dfs.sort(); dfs.dedup();
        if dfs.len() >= 2 { diff_daily += 1; }
        let ds: Vec<i64> = m.iter().filter_map(|&i| { 
            if let Some(d) = date_of(&notes[i]) { datesrc.0 += 1; day_num(&d) } else if let Some(d) = daily_folder(&notes[i].rel) { datesrc.1 += 1; day_num(&d) } else { None } }).collect();
        if ds.len() == m.len() {
            spread_known += 1;
            let sp = ds.iter().max().unwrap() - ds.iter().min().unwrap();
            if sp == 0 { s0 += 1 } else if sp <= 7 { s7 += 1 } else { sm += 1 }
        }
    }
    println!("groups={ng} folder-kind sets: {:?}", { let mut v: Vec<_> = kinds.iter().collect(); v.sort_by(|a, b| b.1.cmp(a.1)); v.into_iter().take(10).collect::<Vec<_>>() });
    println!("groups with a member in Clippings={in_clip}; groups with members in >=2 different daily/day/<date> folders={diff_daily}; in either={}", "n/a (see above)");
    println!("groups with date known for all members={spread_known} (date from created/date/published fm={}, else daily folder date={}): spread same-day={s0} <=7d={s7} more={sm}", datesrc.0, datesrc.1);
}

pub fn state(vault: &str, db: &str) {
    let root = Path::new(vault);
    let opts = PostIdOptions::default();
    let mut files = Vec::new();
    walk(root, root, &mut files);
    let mut bm: HashSet<String> = HashSet::new(); // _blog_metadata identities
    let mut note_ident: HashMap<String, String> = HashMap::new(); // rel -> ident
    let mut bm_n = 0;
    for f in files.iter().filter(|f| !f.starts_with(".trash/")) {
        let Ok(t) = fs::read_to_string(root.join(f)) else { continue };
        let fm = parse_fm(split_fm(&t).0);
        let id = fm.get("post-id").filter(|s| !s.is_empty()).cloned().or_else(|| fm.get("source").filter(|s| !s.is_empty()).map(|s| post_id_from_url(s, &opts)));
        if let Some(id) = id { if f.starts_with("_blog_metadata/") { bm.insert(id.clone()); bm_n += 1; } note_ident.insert(f.clone(), id); }
    }
    println!("blog_metadata notes={bm_n} distinct idents={}", bm.len());
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut st = conn.prepare("SELECT day, outcome, item_id, url, note_path FROM tracker_items WHERE source_kind='blog'").unwrap();
    let rows: Vec<(String, String, String, String, Option<String>)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap().map(|r| r.unwrap()).collect();
    let mut agg: BTreeMap<(bool, String), [usize; 6]> = BTreeMap::new(); // total, with_url, match_item_id, match_url, match_note, match_any
    for (day, outcome, item, url, np) in &rows {
        let e = agg.entry((day == "2026-10-16", outcome.clone())).or_default();
        e[0] += 1;
        let m_item = bm.contains(item) || (!url.is_empty() && bm.contains(item.as_str()));
        let m_url = !url.is_empty() && bm.contains(&post_id_from_url(url, &opts));
        let m_note = np.as_ref().and_then(|p| note_ident.get(p)).is_some_and(|i| bm.contains(i));
        if !url.is_empty() { e[1] += 1; }
        if m_item { e[2] += 1; } if m_url { e[3] += 1; } if m_note { e[4] += 1; }
        if m_item || m_url || m_note { e[5] += 1; }
    }
    println!("blog tracker rows: (is_2026-10-16, outcome) -> [total, with_url, item_id==bm ident, post_id(url)==bm ident, note_path ident==bm ident, any]");
    for (k, v) in &agg { println!("{k:?} {v:?}"); }
}
