# Throwaway: binary+f32-rescore recall@10 on real short-query embeddings (generic queries, not vault text). Aggregates only.
import sqlite3, sys, json, urllib.request, numpy as np
c = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
blobs = [r[0] for r in c.execute("select embedding from chunks where embedding is not null order by rowid")]
M = np.frombuffer(b"".join(blobs), dtype="<f4").reshape(len(blobs), -1)
B = np.packbits(M > 0, axis=1)
Q = ["AI coding agents", "local language models", "how to sleep better", "fantasy novel dragon chapter", "weekly newsletter ideas",
     "stock market outlook", "rust programming", "obsidian plugin", "productivity system", "machine learning papers",
     "exercise and fitness", "meeting notes", "travel plans", "history of science", "philosophy of mind",
     "startup fundraising", "music recommendations", "cooking recipes", "home renovation", "parenting advice",
     "climate change policy", "space exploration", "video game design", "writing tips", "personal finance budget",
     "open source licensing", "database performance", "security vulnerability", "election news", "mental health",
     "book review", "podcast transcript", "robotics", "semiconductor supply chain", "anthropic claude", "self insert fanfiction",
     "deep research report", "inflation and interest rates", "learning a language", "time management"]
def emb(t):
    r = json.load(urllib.request.urlopen(urllib.request.Request("http://127.0.0.1:4811/v1/embeddings", data=json.dumps({"model": "bge-m3-f16", "input": t}).encode(), headers={"content-type": "application/json"})))
    return np.array(r["data"][0]["embedding"], dtype="f4")
pop = np.unpackbits(np.arange(256, dtype=np.uint8)[:, None], axis=1).sum(1)
res = {c_: [] for c_ in (100, 200, 400)}
for q in Q:
    v = emb(q); v /= np.linalg.norm(v)
    truth = set(np.argsort(-(M @ v))[:10])
    ham = pop[np.bitwise_xor(B, np.packbits(v > 0))].sum(1)
    order = np.argsort(ham, kind="stable")
    for cand in res:
        ids = order[:cand]; top = ids[np.argsort(-(M[ids] @ v))[:10]]
        res[cand].append(len(truth & set(top)) / 10)
for cand, r in res.items():
    print(f"real queries n={len(Q)} binary cand={cand} f32 rescore: mean recall@10={np.mean(r):.3f} min={np.min(r):.1f}")
# Asymmetric variant: f32 query · ±1 doc signs (no query binarization), then f32 rescore; and larger symmetric pools.
S = np.where(M > 0, 1.0, -1.0).astype("f4")
res2 = {(k, c_): [] for k in ("asym", "sym") for c_ in (200, 400, 800)}
for q in Q:
    v = emb(q); v /= np.linalg.norm(v)
    truth = set(np.argsort(-(M @ v))[:10])
    orders = {"asym": np.argsort(-(S @ v), kind="stable"), "sym": np.argsort(pop[np.bitwise_xor(B, np.packbits(v > 0))].sum(1), kind="stable")}
    for (k, cand) in res2:
        ids = orders[k][:cand]; top = ids[np.argsort(-(M[ids] @ v))[:10]]
        res2[(k, cand)].append(len(truth & set(top)) / 10)
for (k, cand), r in res2.items():
    print(f"real queries {k:4s} cand={cand}: mean recall@10={np.mean(r):.3f} min={np.min(r):.1f}")
