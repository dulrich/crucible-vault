# Throwaway: FTS5 latency on the companion DB copy. Terms drawn from fts5vocab by doc frequency; prints aggregates only.
import sqlite3, sys, time, statistics, random
db = sys.argv[1]
c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
c.execute("CREATE TEMP TABLE IF NOT EXISTS v_dummy(x)")
c.execute("CREATE VIRTUAL TABLE temp.vocab USING fts5vocab(main, 'chunks_fts', 'row')")
rows = c.execute("SELECT term, doc FROM temp.vocab WHERE length(term) >= 4 AND term GLOB '[a-z]*' ").fetchall()
rows.sort(key=lambda r: r[1])
n = len(rows); random.seed(7)
bands = {"rare(<10 docs)": [r for r in rows if r[1] < 10], "mid(100-1k)": [r for r in rows if 100 <= r[1] < 1000], "common(>5k)": [r for r in rows if r[1] >= 5000]}
print(f"vocab terms(len>=4,alpha)={n}")
def bench(label, q_of, terms_sets, limit=50):
    ts = []
    for ts_ in terms_sets:
        q = q_of(ts_)
        t = time.perf_counter()
        c.execute("SELECT rowid, bm25(chunks_fts, 0,0,0, 4.0, 2.0, 1.0, 1.0) r FROM chunks_fts WHERE chunks_fts MATCH ? ORDER BY r LIMIT ?", (q, limit)).fetchall()
        ts.append((time.perf_counter() - t) * 1e3)
    ts.sort()
    print(f"{label:38s} n={len(ts)} p50={statistics.median(ts):7.2f}ms p90={ts[int(len(ts)*0.9)]:7.2f}ms max={ts[-1]:7.2f}ms")
for name, pool in bands.items():
    sample = [random.sample(pool, 1) for _ in range(40)]
    bench(f"1 term {name}", lambda s: '"%s"' % s[0][0], sample)
mixed = [[random.choice(bands["mid(100-1k)"]), random.choice(bands["common(>5k)"])] for _ in range(40)]
bench("2 terms AND (mid+common)", lambda s: " ".join('"%s"' % t[0] for t in s), mixed)
bench("2 terms OR (mid+common)", lambda s: " OR ".join('"%s"' % t[0] for t in s), mixed)
three = [[random.choice(bands["common(>5k)"]) for _ in range(3)] for _ in range(40)]
bench("3 common terms OR", lambda s: " OR ".join('"%s"' % t[0] for t in s), three)
bench("prefix 2 chars (typeahead)", lambda s: '"%s"*' % s[0][0][:2], [random.sample(bands["mid(100-1k)"], 1) for _ in range(40)])
bench("prefix 3 chars (typeahead)", lambda s: '"%s"*' % s[0][0][:3], [random.sample(bands["mid(100-1k)"], 1) for _ in range(40)])
bench("prefix 5 chars (no prefix idx)", lambda s: '"%s"*' % s[0][0][:5], [random.sample(bands["mid(100-1k)"], 1) for _ in range(40)])
