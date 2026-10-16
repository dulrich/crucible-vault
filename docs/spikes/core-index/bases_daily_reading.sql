-- daily-reading-list.base translated; :this_name = embedding note's file.name
WITH settings AS (
  SELECT CAST(json_extract(p.value,'$') AS REAL) wpm FROM props p JOIN files f ON f.id=p.file_id
  WHERE f.path='_custom/Settings.md' AND p.key='reading_wpm'),
rows AS (
  SELECT f.id, f.name,
    (SELECT CAST(value AS REAL) FROM props WHERE file_id=f.id AND key='word-count') wc,
    (SELECT value FROM props WHERE file_id=f.id AND key='read') = 'true' AS is_read,
    (SELECT group_concat(replace(t.value,'#',''), ', ') FROM props p, json_each(p.value) t
       WHERE p.file_id=f.id AND p.key='tags' AND replace(t.value,'#','') NOT IN ('clippings','refined','using')) tags
  FROM files f WHERE f.is_md AND f.folder = 'daily/day/' || :this_name)
SELECT name, tags, wc, round(wc / s.wpm, 1) minutes, CASE WHEN is_read THEN '✅' ELSE '❌' END read,
  CASE WHEN is_read THEN 0 ELSE wc END unread_words, CASE WHEN is_read THEN 0 ELSE round(wc / s.wpm, 1) END unread_minutes
FROM rows, settings s ORDER BY name;
