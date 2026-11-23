# WP-0 findings — folder symlinks in the live vault (2026-11-22)

Aggregates only. Vault: ~/vault (dot-directories pruned).

| Measure | Value | Command |
|---|---|---|
| folder links | 39 | `find $V -path "$V/.*" -prune -o -type l -xtype d -print \| wc -l` |
| folder links targeting inside the vault | 0 | `readlink -f` per link, prefix test |
| folder-link targets inside a git work tree | 39 | `git -C <target> rev-parse --is-inside-work-tree` |
| files reachable through folder links | 1192 (1165 `.md`) | `find -L <link> -type f` |
| distinct canonical files among them | 1192 (no duplicates) | `readlink -f \| sort -u` |
| overlap with the 94 file-link targets | 0 | `comm -12` |
| symlink-loop errors | 0 now (the `kindle-layouts` self-loop was replaced by a copy at 21:37) | `find -L` stderr `loop` |
| max depth below a folder link | 5 | `find -L -printf %d` |
| bytes reachable | 171.4 MiB | `find -L -printf %s` |
| index today | 17,609 files → ~18,800 (+6.8%) following links | `select count(*) from files` |
| non-`.md` files reachable (orphan-sweep candidates) | 27 (pdf 11, xlsx 3, zip 2, jpg 2, docx 2, html 2, …) | extension histogram |
| trash rows from `kindle-layouts/` | 0 | `state.db trash_entries.original_path` |
| live `CV_LINK_WRITE_ROOTS` | `<code-root>` | `/proc/<cv pid>/environ` |

Code facts: full walk `follow_links(false)` (`crates/cv-index/src/index.rs:288`); watcher `notify::recommended_watcher` with default `follow_symlinks: true` (`crates/cv-vault/src/thread.rs:136-142`, notify 8.2.0 `config.rs` Default); `VaultFs::write_target` tests only the final component for a symlink (`crates/cv-vault/src/fs.rs:135-160`).
