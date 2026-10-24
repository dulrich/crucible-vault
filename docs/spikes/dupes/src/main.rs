//! Throwaway read-only duplicate-candidate driver. Output: aggregates only.
//! Usage: dupes shingles <vault> <out.tsv> | dupes semantic <search.db> <out.tsv> | dupes overlap <vault> <a.tsv> <b.tsv>
use cv_text::similar::{containment, normalize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if name == ".obsidian" || name == ".git" { continue; }
            walk(root, &p, out);
        } else if name.ends_with(".md") {
            out.push(p.strip_prefix(root).unwrap().to_string_lossy().to_string());
        }
    }
}

fn split_fm(text: &str) -> (&str, &str) {
    if let Some(rest) = text.strip_prefix("---\n") {
        if let Some(i) = rest.find("\n---") {
            let body_start = rest[i + 4..].find('\n').map_or(rest.len(), |n| i + 4 + n + 1);
            return (&rest[..i], &rest[body_start.min(rest.len())..]);
        }
    }
    ("", text)
}

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn fnv(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (n, p) in parts.iter().enumerate() {
        if n > 0 { h ^= 0x20; h = h.wrapping_mul(0x0100_0000_01b3); }
        for b in p.bytes() { h ^= u64::from(b); h = h.wrapping_mul(0x0100_0000_01b3); }
    }
    h
}

fn shingle_set(body: &str) -> Vec<u64> {
    let norm = normalize(body);
    let toks: Vec<&str> = norm.split(' ').filter(|t| !t.is_empty()).collect();
    let mut v: Vec<u64> = toks.windows(4).map(fnv).collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn inter(a: &[u64], b: &[u64]) -> usize {
    let (mut i, mut j, mut n) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => { n += 1; i += 1; j += 1; }
        }
    }
    n
}

const NH: usize = 63;
const ROWS: usize = 3;
const MIN_SH: usize = 20;
const DF_CAP: usize = 30;

fn shingles_mode(vault: &str, out: &str) {
    let t0 = Instant::now();
    let root = Path::new(vault);
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files.sort();
    let mut paths = Vec::new();
    let mut sets: Vec<Vec<u64>> = Vec::new();
    let mut short = 0usize;
    for f in &files {
        let Ok(t) = fs::read_to_string(root.join(f)) else { continue };
        let (_, body) = split_fm(&t);
        let s = shingle_set(body);
        if s.len() < MIN_SH { short += 1; continue; }
        paths.push(f.clone());
        sets.push(s);
    }
    eprintln!("loaded {} notes, {} short-excluded (<{} shingles) in {:?}", paths.len(), short, MIN_SH, t0.elapsed());
    let n = paths.len();
    let t1 = Instant::now();
    // (1) inverted index with df cap -> exact candidates
    let mut post: HashMap<u64, Vec<u32>> = HashMap::new();
    for (i, s) in sets.iter().enumerate() { for &h in s { post.entry(h).or_default().push(i as u32); } }
    let mut cand: HashSet<(u32, u32)> = HashSet::new();
    let mut capped = 0usize;
    let mut counts: HashMap<(u32, u32), u32> = HashMap::new();
    for l in post.values() {
        if l.len() < 2 { continue; }
        if l.len() > DF_CAP { capped += 1; continue; }
        for x in 0..l.len() { for y in x + 1..l.len() { *counts.entry((l[x], l[y])).or_default() += 1; } }
    }
    for (k, c) in &counts { if *c >= 3 { cand.insert(*k); } }
    drop(counts);
    let t_inv = t1.elapsed();
    eprintln!("inverted: {} candidates, {} shingles over df cap, {:?}", cand.len(), capped, t_inv);
    // (2) MinHash LSH
    let t2 = Instant::now();
    let bands = NH / ROWS;
    let mut buckets: Vec<HashMap<u64, Vec<u32>>> = vec![HashMap::new(); bands];
    for (i, s) in sets.iter().enumerate() {
        let mut mh = [u64::MAX; NH];
        for &h in s { for (k, m) in mh.iter_mut().enumerate() { let v = splitmix(h ^ splitmix(k as u64 + 1)); if v < *m { *m = v; } } }
        for b in 0..bands {
            let key = mh[b * ROWS..(b + 1) * ROWS].iter().fold(0u64, |a, &v| splitmix(a ^ v));
            buckets[b].entry(key).or_default().push(i as u32);
        }
    }
    let mut lsh: HashSet<(u32, u32)> = HashSet::new();
    for b in &buckets { for l in b.values() { if l.len() < 2 || l.len() > 50 { continue; } for x in 0..l.len() { for y in x + 1..l.len() { lsh.insert((l[x], l[y])); } } } }
    let t_lsh = t2.elapsed();
    eprintln!("lsh: {} candidates, {:?}", lsh.len(), t_lsh);
    // exact verify union
    let t3 = Instant::now();
    let all: HashSet<(u32, u32)> = cand.union(&lsh).copied().collect();
    let mut res: Vec<(u32, u32, f64, f64)> = Vec::new();
    for &(a, b) in &all {
        let (sa, sb) = (&sets[a as usize], &sets[b as usize]);
        let i = inter(sa, sb) as f64;
        let c = i / sa.len().min(sb.len()) as f64;
        let j = i / ((sa.len() + sb.len()) as f64 - i);
        res.push((a, b, c, j));
    }
    let t_ver = t3.elapsed();
    let mut f = fs::File::create(out).unwrap();
    let mut by = [[0usize; 3]; 3]; // [threshold][inv, lsh, either]
    let ths = [0.5, 0.7, 0.9];
    let mut jt = [0usize; 3];
    for &(a, b, c, j) in &res {
        let ia = cand.contains(&(a, b)); let il = lsh.contains(&(a, b));
        for (k, t) in ths.iter().enumerate() {
            if c >= *t { by[k][2] += 1; if ia { by[k][0] += 1; } if il { by[k][1] += 1; } }
            if j >= *t { jt[k] += 1; }
        }
        if c >= 0.5 { writeln!(f, "{}\t{}\t{:.4}", paths[a as usize], paths[b as usize], c).unwrap(); }
    }
    println!("notes={n} short_excluded={short} inverted_cands={} df_capped_shingles={capped} lsh_cands={} union_cands={} lsh_params=NH{NH}xROWS{ROWS}", cand.len(), lsh.len(), all.len());
    for (k, t) in ths.iter().enumerate() {
        println!("containment>={t}: pairs={} found_by_inverted={} found_by_lsh={} jaccard>={t}:{}", by[k][2], by[k][0], by[k][1], jt[k]);
    }
    println!("time_load={:?} time_inverted={:?} time_lsh={:?} time_verify={:?} total={:?}", t0.elapsed() - t_inv - t_lsh - t_ver, t_inv, t_lsh, t_ver, t0.elapsed());
    // cross-check against cv_text::similar::containment on a sample of 200 verified pairs
    let mut chk = 0; let mut bad = 0; let mut maxd: f64 = 0.0;
    for &(a, b, c, _) in res.iter().filter(|r| r.2 >= 0.5).take(200) {
        let rd = |p: &str| { let t = fs::read_to_string(root.join(p)).unwrap(); split_fm(&t).1.to_string() };
        let (ta, tb) = (rd(&paths[a as usize]), rd(&paths[b as usize]));
        let (small, large) = if sets[a as usize].len() <= sets[b as usize].len() { (ta, tb) } else { (tb, ta) };
        let d = (containment(&small, &large) - c).abs();
        maxd = maxd.max(d); chk += 1; if d > 0.01 { bad += 1; }
    }
    println!("crosscheck_vs_cv_text_containment: checked={chk} differing>0.01={bad} max_abs_diff={maxd:.4}");
}

fn semantic_mode(db: &str, out: &str) {
    let t0 = Instant::now();
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut st = conn.prepare("SELECT c.source, v.embedding FROM chunks c JOIN vectors v ON v.id = c.id ORDER BY c.source").unwrap();
    let mut rows = st.query([]).unwrap();
    let dim = 1024usize;
    let mut names: Vec<String> = Vec::new();
    let mut means: Vec<Vec<f32>> = Vec::new();
    let mut cnt: Vec<u32> = Vec::new();
    let mut chunks = 0usize;
    while let Some(r) = rows.next().unwrap() {
        let s: String = r.get(0).unwrap();
        let blob: Vec<u8> = r.get(1).unwrap();
        assert_eq!(blob.len(), dim * 4);
        if names.last() != Some(&s) { names.push(s); means.push(vec![0.0; dim]); cnt.push(0); }
        let m = means.last_mut().unwrap();
        for (i, c) in blob.chunks_exact(4).enumerate() { m[i] += f32::from_le_bytes([c[0], c[1], c[2], c[3]]); }
        *cnt.last_mut().unwrap() += 1; chunks += 1;
    }
    for m in &mut means { let n = m.iter().map(|x| x * x).sum::<f32>().sqrt(); if n > 0.0 { for x in m.iter_mut() { *x /= n; } } }
    let n = names.len();
    let t_load = t0.elapsed();
    let t1 = Instant::now();
    let flat: Vec<f32> = means.iter().flatten().copied().collect();
    drop(means);
    let nt = 4usize;
    let lo = 0.85f32;
    let mut handles = Vec::new();
    let flat = std::sync::Arc::new(flat);
    for t in 0..nt {
        let flat = flat.clone();
        handles.push(std::thread::spawn(move || {
            let mut out: Vec<(u32, u32, f32)> = Vec::new();
            let mut i = t;
            while i < n {
                let a = &flat[i * dim..(i + 1) * dim];
                for j in i + 1..n {
                    let b = &flat[j * dim..(j + 1) * dim];
                    let d: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
                    if d >= lo { out.push((i as u32, j as u32, d)); }
                }
                i += nt;
            }
            out
        }));
    }
    let mut pairs: Vec<(u32, u32, f32)> = handles.into_iter().flat_map(|h| h.join().unwrap()).collect();
    pairs.sort_by_key(|p| (p.0, p.1));
    let t_scan = t1.elapsed();
    let mut f = fs::File::create(out).unwrap();
    for &(a, b, d) in &pairs { writeln!(f, "{}\t{}\t{:.4}", names[a as usize], names[b as usize], d).unwrap(); }
    println!("sources={n} chunks={chunks} dim={dim} pairs_scanned={}", (n as u64) * (n as u64 - 1) / 2);
    for t in [0.85f32, 0.90, 0.95, 0.98] { println!("cosine>={t}: pairs={}", pairs.iter().filter(|p| p.2 >= t).count()); }
    // per-source NN stats at 0.95
    let mut has: HashSet<u32> = HashSet::new();
    for p in pairs.iter().filter(|p| p.2 >= 0.95) { has.insert(p.0); has.insert(p.1); }
    println!("sources_with_a_neighbour>=0.95: {}", has.len());
    println!("time_load_mean={t_load:?} time_allpairs_4threads={t_scan:?} total={:?}", t0.elapsed());
}

fn read_pairs(p: &str) -> HashMap<(String, String), f64> {
    let mut m = HashMap::new();
    for l in fs::read_to_string(p).unwrap().lines() {
        let v: Vec<&str> = l.split('\t').collect();
        let (a, b) = if v[0] < v[1] { (v[0], v[1]) } else { (v[1], v[0]) };
        m.insert((a.to_string(), b.to_string()), v[2].parse().unwrap());
    }
    m
}

struct Meta { host: String, dup_names: Vec<String>, trash: bool, top: String, dir: String, stem: String }

fn meta_of(root: &Path, rel: &str) -> Meta {
    let t = fs::read_to_string(root.join(rel)).unwrap_or_default();
    let (fm, _) = split_fm(&t);
    let mut host = String::new();
    let mut dup = Vec::new();
    let mut in_dup = false;
    for l in fm.lines() {
        if let Some(v) = l.strip_prefix("source:") {
            in_dup = false;
            let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
            if let Some(i) = v.find("://") { let r = &v[i + 3..]; host = r.split('/').next().unwrap_or("").trim_start_matches("www.").to_lowercase(); }
        } else if l.starts_with("duplicate-of:") || l.starts_with("duplicate_of:") {
            in_dup = true; collect_links(l, &mut dup);
        } else if in_dup && (l.starts_with(' ') || l.starts_with('-')) { collect_links(l, &mut dup); }
        else { in_dup = false; }
    }
    let stem = rel.rsplit('/').next().unwrap().trim_end_matches(".md").to_string();
    let top = rel.split('/').next().unwrap().to_string();
    let dir = rel.rsplit_once('/').map_or(String::new(), |x| x.0.to_string());
    Meta { host, dup_names: dup, trash: rel.starts_with(".trash/"), top, dir, stem }
}

fn collect_links(l: &str, out: &mut Vec<String>) {
    let mut rest = l;
    while let Some(i) = rest.find("[[") {
        if let Some(j) = rest[i..].find("]]") {
            let inner = &rest[i + 2..i + j];
            let name = inner.split('|').next().unwrap().split('#').next().unwrap().rsplit('/').next().unwrap();
            out.push(name.to_string());
            rest = &rest[i + j + 2..];
        } else { break; }
    }
}

fn proxies(label: &str, pairs: &[(&String, &String)], metas: &mut HashMap<String, Meta>, root: &Path) {
    for (a, b) in pairs { for p in [a, b] { metas.entry((*p).clone()).or_insert_with(|| meta_of(root, p)); } }
    let n = pairs.len();
    if n == 0 { println!("{label}: pairs=0"); return; }
    let (mut sdir, mut stop, mut shost_both, mut shost_same, mut linked, mut anydup, mut trash) = (0, 0, 0, 0, 0, 0, 0);
    let mut stem = 0;
    let mut tops: HashMap<String, usize> = HashMap::new();
    for (a, b) in pairs {
        let (ma, mb) = (&metas[*a], &metas[*b]);
        if ma.dir == mb.dir { sdir += 1; }
        if ma.top == mb.top { stop += 1; }
        if !ma.host.is_empty() && !mb.host.is_empty() { shost_both += 1; if ma.host == mb.host { shost_same += 1; } }
        if ma.dup_names.contains(&mb.stem) || mb.dup_names.contains(&ma.stem) { linked += 1; }
        if !ma.dup_names.is_empty() || !mb.dup_names.is_empty() { anydup += 1; }
        if ma.trash || mb.trash { trash += 1; }
        if ma.stem == mb.stem { stem += 1; }
        let (x, y) = if ma.top <= mb.top { (&ma.top, &mb.top) } else { (&mb.top, &ma.top) };
        *tops.entry(format!("{x} x {y}")).or_default() += 1;
    }
    let pct = |x: usize, d: usize| if d == 0 { 0.0 } else { 100.0 * x as f64 / d as f64 };
    println!("{label}: pairs={n} same_dir={:.1}% same_top_folder={:.1}% both_have_source_host={} of_which_same_host={:.1}% linked_via_duplicate_of={} ({:.1}%) either_has_duplicate_of={} ({:.1}%) one_side_in_trash={} same_filename_stem={:.1}%", pct(sdir, n), pct(stop, n), shost_both, pct(shost_same, shost_both), linked, pct(linked, n), anydup, pct(anydup, n), trash, pct(stem, n));
    let mut tv: Vec<_> = tops.into_iter().collect();
    tv.sort_by(|x, y| y.1.cmp(&x.1));
    let s: Vec<String> = tv.iter().take(5).map(|(k, v)| format!("[{k}]={v}")).collect();
    println!("  top folder-pair kinds: {}", s.join(" "));
}

fn overlap_mode(vault: &str, fa: &str, fb: &str) {
    let root = Path::new(vault);
    let a = read_pairs(fa); let b = read_pairs(fb);
    let mut metas: HashMap<String, Meta> = HashMap::new();
    println!("A pairs(cont>=0.5)={} B pairs(cos>=0.85)={}", a.len(), b.len());
    for (ct, st) in [(0.5, 0.90), (0.7, 0.95), (0.9, 0.98)] {
        let aa: HashSet<&(String, String)> = a.iter().filter(|x| *x.1 >= ct).map(|x| x.0).collect();
        let bb: HashSet<&(String, String)> = b.iter().filter(|x| *x.1 >= st).map(|x| x.0).collect();
        let i = aa.intersection(&bb).count();
        // A pairs where both notes are indexed in B universe
        let bnames: HashSet<&String> = b.keys().flat_map(|k| [&k.0, &k.1]).collect();
        println!("overlap cont>={ct} vs cos>={st}: A={} B={} both={} A_only={} B_only={} jaccard_of_sets={:.3} (A pairs whose both notes appear in some B>=0.85 pair: {})", aa.len(), bb.len(), i, aa.len() - i, bb.len() - i, i as f64 / (aa.len() + bb.len() - i).max(1) as f64, aa.iter().filter(|k| bnames.contains(&k.0) && bnames.contains(&k.1)).count());
    }
    // containment bands
    for (lo, hi) in [(0.5, 0.7), (0.7, 0.9), (0.9, 1.01)] {
        let v: Vec<(&String, &String)> = a.iter().filter(|x| *x.1 >= lo && *x.1 < hi).map(|x| (&x.0 .0, &x.0 .1)).collect();
        let inb = v.iter().filter(|(x, y)| b.get(&((*x).clone(), (*y).clone())).is_some_and(|c| *c >= 0.90)).count();
        println!("A band containment [{lo},{hi}): also_cos>=0.90={inb}");
        proxies(&format!("  proxy A[{lo},{hi})"), &v, &mut metas, root);
    }
    for (lo, hi) in [(0.90, 0.95), (0.95, 0.98), (0.98, 1.01)] {
        let v: Vec<(&String, &String)> = b.iter().filter(|x| *x.1 >= lo && *x.1 < hi).map(|x| (&x.0 .0, &x.0 .1)).collect();
        let ina = v.iter().filter(|(x, y)| a.contains_key(&((*x).clone(), (*y).clone()))).count();
        println!("B band cosine [{lo},{hi}): also_cont>=0.5={ina}");
        proxies(&format!("  proxy B[{lo},{hi})"), &v, &mut metas, root);
    }
    let both: Vec<(&String, &String)> = a.iter().filter(|x| *x.1 >= 0.7 && b.get(x.0).is_some_and(|c| *c >= 0.95)).map(|x| (&x.0 .0, &x.0 .1)).collect();
    proxies("  proxy BOTH(cont>=0.7 & cos>=0.95)", &both, &mut metas, root);
    let ao: Vec<(&String, &String)> = a.iter().filter(|x| *x.1 >= 0.7 && !b.get(x.0).is_some_and(|c| *c >= 0.95)).map(|x| (&x.0 .0, &x.0 .1)).collect();
    proxies("  proxy A_only(cont>=0.7, not cos>=0.95)", &ao, &mut metas, root);
    let bo: Vec<(&String, &String)> = b.iter().filter(|x| *x.1 >= 0.95 && !a.get(x.0).is_some_and(|c| *c >= 0.7)).map(|x| (&x.0 .0, &x.0 .1)).collect();
    proxies("  proxy B_only(cos>=0.95, not cont>=0.7)", &bo, &mut metas, root);
    let nd = metas.values().filter(|m| !m.dup_names.is_empty()).count();
    println!("notes_touched={} of_which_with_duplicate_of={}", metas.len(), nd);
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("shingles") if a.len() == 4 => shingles_mode(&a[2], &a[3]),
        Some("semantic") if a.len() == 4 => semantic_mode(&a[2], &a[3]),
        Some("overlap") if a.len() == 5 => overlap_mode(&a[2], &a[3], &a[4]),
        _ => println!("usage: dupes shingles <vault> <out.tsv> | semantic <search.db> <out.tsv> | overlap <vault> <a.tsv> <b.tsv>"),
    }
}
