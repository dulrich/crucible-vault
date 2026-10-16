"""Synthetic vault generator (editor-usability-round WP-5, audit E2). Synthetic text only.
usage: python3 -I gen.py <out_dir> <notes> <folders>   (folders=0 -> single folder, cv-testkit style)"""
import os, random, sys
out, n, nf = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
rng = random.Random(42)
folders = [""]
if nf:
    folders = []
    while len(folders) < nf:
        depth = rng.randint(1, 4)
        parent = rng.choice(folders) if folders and depth > 1 else ""
        name = f"f{len(folders):04d}"
        folders.append(f"{parent}/{name}".strip("/") if parent.count("/") < 3 else name)
else:
    folders = ["notes"]
names = [f"note-{i:05d}" for i in range(n)]
tags = [f"tag{i}" for i in range(60)]
for i, name in enumerate(names):
    d = os.path.join(out, rng.choice(folders))
    os.makedirs(d, exist_ok=True)
    links = " ".join(f"[[{rng.choice(names)}]]" for _ in range(rng.randint(2, 10)))
    t = rng.sample(tags, 3)
    body = (f"---\ntitle: Synthetic {i}\ntags: [{t[0]}, {t[1]}]\nkind: k{i % 7}\n---\n"
            f"# Synthetic {i}\n\nLorem ipsum {links} #{t[2]}\n\n- [ ] task a {i}\n- [x] task b\n")
    with open(os.path.join(d, name + ".md"), "w") as f:
        f.write(body)
print(f"notes={n} folders={len(set(folders))}")
