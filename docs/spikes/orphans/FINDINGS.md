# Orphaned attachments and trash — measurement (2026-10-26)

Read-only against the live index (`~/.cache/crucible-vault/index-3.db`) and the vault's `.trash/` after the round-15 deploy. Aggregates only; no note text, titles or personal paths. Feeds `plans/tidiness-trash-orphans.md`.

## Commands and results

- `sqlite3 -readonly -header ~/.cache/crucible-vault/index-3.db < docs/spikes/orphans/measure.sql` — managed (`_MD5`) media attachments by live reference: 2,229 unreferenced (679.6 MiB), 6,769 referenced (1,255.2 MiB); no indexed file or link source lies under `.trash/` (the index excludes it).
- `sqlite3 -readonly ~/.cache/crucible-vault/index-3.db < docs/spikes/orphans/dup.sql` — 380 orphans (151.5 MiB) have a linked twin of the same name and the same size; orphans span 5 top-level folders; 1,016 sit under an `_attachments/` folder.
- Trash on disk: `find <vault>/.trash -type f | wc -l` → 2,387 files; `-name '*.md'` → 615 notes; `du -sm` → 277 MiB; `-name '*_MD5.*'` → 1,769 managed attachments. Trashed-note ctime: 610 of 615 under 7 days, so ctime does not record when a file was trashed (and the rename keeps mtime).
- Orphans named by trashed notes: the `_MD5` basenames referenced in `.trash/**/*.md` (`grep -rhoP '[^/\[\]\(\)|"\s]+_MD5\.[A-Za-z0-9]+' <vault>/.trash --include=*.md | sort -u`, 1,665 names), joined to the orphan list by `name.ext`:

| named by a trashed note | linked twin | count | MiB |
|---|---|---|---|
| yes | no | 1,536 | 477.2 |
| yes | yes | 194 | 136.7 |
| no | yes | 186 | 14.8 |
| no | no | 313 | 50.9 |

- Filesystem: `stat -c %d` gives device 66308 for both the vault and `~/.local/share`, and `~/.local/share/Trash` exists, so an XDG home-trash move is a same-device rename.
