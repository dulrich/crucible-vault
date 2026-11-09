# Example views

Reading-list views that are not built in. To use one, copy it into `<config>/views/` (the config directory the daemon reads, next to `captures.toml`) and restart or reload the daemon:

```bash
cp config/examples/views/monthly-reading.toml config/examples/views/daily-reading-list.toml "<config>/views/"
```

- `monthly-reading.toml` (`Unread`, `By Tag`, `By Day`) — embed in a month note; see `../notes/monthly.md`.
- `daily-reading-list.toml` (`Reading List`) — embed in a day note; see `../notes/daily.md`.
- `blog-story-list.toml` (`Stories`) — embed in a work note that sets `author-filter` and/or `source-prefix-filter` in its frontmatter.

The views read the day folder through `folder("day")`, so they follow `[periods.day].folder`. They assume notes carry `word-count`, `read`, `created` and `tags` properties, and that `_custom/Settings.md` holds a `reading_wpm` property (the pivot falls back to 250). Category tags (`blog`, `news`, `transcript`, `serial`) are plain examples: edit the `category` formula and the pivot `columns` for your own taxonomy. The test suite parses every file here, so they stay valid.
