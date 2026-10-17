# Spike: of blog tracker rows with a body (outcome line/note), how many are still present in their feed now? Aggregates only.
import sqlite3, urllib.request, sys, collections
db = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
rows = db.execute("SELECT source_key, url, item_id, outcome FROM tracker_items WHERE source_kind='blog' AND outcome IN ('line','note') AND full_body=1").fetchall()
by = collections.defaultdict(list)
for k,u,i,o in rows: by[k].append((u,i,o))
hit=collections.Counter(); miss=collections.Counter(); fail=0
for k, items in by.items():
    try:
        x = urllib.request.urlopen(urllib.request.Request(k, headers={"User-Agent":"Mozilla/5.0"}), timeout=20).read().decode("utf-8","replace")
    except Exception:
        fail += 1; miss.update(o for _,_,o in items); continue
    for u,i,o in items:
        (hit if (u in x or i in x) else miss)[o] += 1
print(f"feeds={len(by)} feed_fetch_failed={fail} present={dict(hit)} absent={dict(miss)}")
