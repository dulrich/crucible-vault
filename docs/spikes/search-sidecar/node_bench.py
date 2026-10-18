# Throwaway: end-to-end /v1/search latency on the Node companion (scratch copy, port 4899). Aggregates only.
import sqlite3, sys, json, time, struct, urllib.request, statistics, random
db, port = sys.argv[1], sys.argv[2]
c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
vault = c.execute("select vault_id from chunks limit 1").fetchone()[0]
n = c.execute("select max(rowid) from chunks").fetchone()[0]
c.execute("CREATE VIRTUAL TABLE temp.vocab USING fts5vocab(main, 'chunks_fts', 'row')")
terms = [r[0] for r in c.execute("SELECT term FROM temp.vocab WHERE length(term)>=5 AND term GLOB '[a-z]*' AND doc BETWEEN 50 AND 2000")]
random.seed(3)
def post(body):
    req = urllib.request.Request(f"http://127.0.0.1:{port}/v1/search", data=json.dumps(body).encode(), headers={"content-type": "application/json"})
    t = time.perf_counter(); r = json.load(urllib.request.urlopen(req, timeout=120)); return (time.perf_counter() - t) * 1e3, r
def emb():
    while True:
        row = c.execute("select embedding from chunks where rowid=? and embedding is not null", (random.randint(1, n),)).fetchone()
        if row: return list(struct.unpack(f"<{len(row[0])//4}f", row[0]))
def run(label, mk, k=30):
    ts, flags = [], {}
    for _ in range(k):
        ms, r = post(mk())
        ts.append(ms)
        for f in ("vectorPending", "semanticUsed", "degraded"):
            if r.get(f): flags[f] = flags.get(f, 0) + 1
    ts.sort(); print(f"{label:32s} n={k} p50={statistics.median(ts):7.1f}ms p90={ts[int(k*.9)]:7.1f}ms max={ts[-1]:7.1f}ms flags={flags}")
run("keywords only", lambda: {"vaultId": vault, "query": random.choice(terms), "limit": 20})
# warm the matrix: first hybrid call schedules the off-request build
t0 = time.perf_counter()
for i in range(600):
    ms, r = post({"vaultId": vault, "query": random.choice(terms), "limit": 20, "queryEmbedding": emb(), "embeddingSpace": "bge-m3/f16"})
    if not r.get("vectorPending"): break
    time.sleep(0.5)
print(f"matrix warm after {(time.perf_counter()-t0):.1f}s ({i+1} probes); keys={sorted(r.keys())[:12]}")
run("hybrid (FTS + vector + RRF)", lambda: {"vaultId": vault, "query": random.choice(terms), "limit": 20, "queryEmbedding": emb(), "embeddingSpace": "bge-m3/f16"})
