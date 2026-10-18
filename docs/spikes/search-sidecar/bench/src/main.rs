// Throwaway spike: vector-scan variants over the companion's bge-m3 embeddings.
use rusqlite::Connection;
use std::time::Instant;

const K: usize = 10;

fn topk_f32(scores: impl Iterator<Item = (usize, f32)>, k: usize) -> Vec<(usize, f32)> {
    let mut v: Vec<(usize, f32)> = Vec::with_capacity(k + 1);
    for (i, s) in scores {
        if v.len() < k || s > v[v.len() - 1].1 {
            let pos = v.partition_point(|x| x.1 >= s);
            v.insert(pos, (i, s));
            if v.len() > k { v.pop(); }
        }
    }
    v
}

#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 16];
    for (ca, cb) in a.chunks_exact(16).zip(b.chunks_exact(16)) {
        for j in 0..16 { acc[j] += ca[j] * cb[j]; }
    }
    acc.iter().sum()
}
#[inline]
fn dot_i8(a: &[i8], b: &[i8]) -> i32 {
    let mut acc = [0i32; 32];
    for (ca, cb) in a.chunks_exact(32).zip(b.chunks_exact(32)) {
        for j in 0..32 { acc[j] += ca[j] as i32 * cb[j] as i32; }
    }
    acc.iter().sum()
}
#[inline]
fn hamming(a: &[u64], b: &[u64]) -> u32 {
    a.iter().zip(b).map(|(x, y)| (x ^ y).count_ones()).sum()
}

fn par<T: Send>(n: usize, threads: usize, f: impl Fn(std::ops::Range<usize>) -> T + Sync) -> Vec<T> {
    let step = n.div_ceil(threads);
    std::thread::scope(|s| {
        let hs: Vec<_> = (0..threads).map(|t| { let f = &f; let r = (t * step).min(n)..((t + 1) * step).min(n); s.spawn(move || f(r)) }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

fn main() {
    let db = std::env::args().nth(1).expect("db path");
    let nq: usize = std::env::args().nth(2).map(|s| s.parse().unwrap()).unwrap_or(200);
    let t0 = Instant::now();
    let conn = Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut st = conn.prepare("SELECT embedding FROM chunks WHERE embedding IS NOT NULL ORDER BY rowid").unwrap();
    let mut m: Vec<f32> = Vec::new();
    let mut dim = 0;
    let mut rows = st.query([]).unwrap();
    while let Some(r) = rows.next().unwrap() {
        let b: Vec<u8> = r.get(0).unwrap();
        dim = b.len() / 4;
        m.extend(b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])));
    }
    let n = m.len() / dim;
    println!("load: n={n} dim={dim} f32_bytes={} in {:?}", m.len() * 4, t0.elapsed());
    let norm0: f32 = m[..dim].iter().map(|x| x * x).sum::<f32>().sqrt();
    println!("row0 norm={norm0:.4}");

    // int8: global symmetric scale from max |x|
    let maxabs = m.iter().fold(0f32, |a, &x| a.max(x.abs()));
    let sc = 127.0 / maxabs;
    let q8: Vec<i8> = m.iter().map(|&x| (x * sc).round().clamp(-127.0, 127.0) as i8).collect();
    // binary: sign bits
    let words = dim / 64;
    let mut bin = vec![0u64; n * words];
    for i in 0..n { for d in 0..dim { if m[i * dim + d] > 0.0 { bin[i * words + d / 64] |= 1 << (d % 64); } } }

    // queries: deterministic spread of existing rows (doc-to-doc proxy); self excluded
    let qs: Vec<usize> = (0..nq).map(|i| (i * 7919 + 13) % n).collect();
    let threads_list = [1usize, 4, 12];

    let exact = |q: usize, threads: usize| -> Vec<(usize, f32)> {
        let qv = &m[q * dim..(q + 1) * dim];
        let parts = par(n, threads, |r| topk_f32(r.map(|i| (i, if i == q { f32::MIN } else { dot(qv, &m[i * dim..(i + 1) * dim]) })), K));
        topk_f32(parts.into_iter().flatten(), K)
    };
    let mut truth = Vec::new();
    for &t in &threads_list {
        let t1 = Instant::now();
        let r: Vec<_> = qs.iter().map(|&q| exact(q, t)).collect();
        let el = t1.elapsed();
        println!("f32 exact  threads={t:2}: {:.2} ms/query", el.as_secs_f64() * 1e3 / nq as f64);
        truth = r;
    }
    let recall = |got: &[Vec<usize>]| -> f64 {
        let mut hit = 0; for (g, t) in got.iter().zip(&truth) { for x in t { if g.contains(&x.0) { hit += 1; } } }
        hit as f64 / (nq * K) as f64
    };

    for &t in &threads_list {
        let t1 = Instant::now();
        let got: Vec<Vec<usize>> = qs.iter().map(|&q| {
            let qv = &q8[q * dim..(q + 1) * dim];
            let parts = par(n, t, |r| topk_f32(r.map(|i| (i, if i == q { f32::MIN } else { dot_i8(qv, &q8[i * dim..(i + 1) * dim]) as f32 })), K));
            topk_f32(parts.into_iter().flatten(), K).into_iter().map(|x| x.0).collect()
        }).collect();
        println!("int8       threads={t:2}: {:.2} ms/query recall@10={:.4}", t1.elapsed().as_secs_f64() * 1e3 / nq as f64, recall(&got));
    }
    for &cand in &[0usize, 50, 100, 200, 400] {
        for &(t, rescore_i8) in &[(1usize, false), (1, true), (4, false)] {
            let t1 = Instant::now();
            let got: Vec<Vec<usize>> = qs.iter().map(|&q| {
                let qb = &bin[q * words..(q + 1) * words];
                let c = cand.max(K);
                let parts = par(n, t, |r| topk_f32(r.map(|i| (i, if i == q { f32::MIN } else { -(hamming(qb, &bin[i * words..(i + 1) * words]) as f32) })), c));
                let cands = topk_f32(parts.into_iter().flatten(), c);
                if cand == 0 { return cands.into_iter().take(K).map(|x| x.0).collect(); }
                let qv = &m[q * dim..(q + 1) * dim];
                let qv8 = &q8[q * dim..(q + 1) * dim];
                topk_f32(cands.into_iter().map(|(i, _)| (i, if rescore_i8 { dot_i8(qv8, &q8[i * dim..(i + 1) * dim]) as f32 } else { dot(qv, &m[i * dim..(i + 1) * dim]) })), K).into_iter().map(|x| x.0).collect()
            }).collect();
            println!("binary cand={cand:3} rescore={} threads={t:2}: {:.3} ms/query recall@10={:.4}", if cand == 0 { "none" } else if rescore_i8 { "int8" } else { "f32 " }, t1.elapsed().as_secs_f64() * 1e3 / nq as f64, recall(&got));
        }
    }
    println!("memory: f32={} MiB int8={} MiB binary={} MiB", n * dim * 4 >> 20, n * dim >> 20, n * words * 8 >> 20);
}
