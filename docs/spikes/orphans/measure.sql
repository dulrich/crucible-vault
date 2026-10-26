-- managed media attachments (name ends _MD5), classified by where they live and who links them. Aggregates only.
WITH m AS (
  SELECT f.id, f.path, f.name, f.ext, f.size, f.mtime, (f.path LIKE '.trash/%') AS in_trash FROM files f
  WHERE f.is_md=0 AND f.name LIKE '%\_MD5' ESCAPE '\'
    AND lower(f.ext) IN ('avif','bmp','gif','jpeg','jpg','png','svg','webp','flac','m4a','mp3','ogg','wav','webm','3gp','mkv','mov','mp4','ogv','pdf')),
r AS (
  SELECT m.*,
    EXISTS(SELECT 1 FROM links l JOIN files s ON s.id=l.src_id WHERE l.resolved_id=m.id AND s.path NOT LIKE '.trash/%') AS live_ref,
    EXISTS(SELECT 1 FROM links l JOIN files s ON s.id=l.src_id WHERE l.resolved_id=m.id AND s.path LIKE '.trash/%') AS trash_ref
  FROM m)
SELECT in_trash, live_ref, trash_ref, COUNT(*), ROUND(SUM(size)/1048576.0,1) AS mib FROM r GROUP BY 1,2,3 ORDER BY 1,2,3;
