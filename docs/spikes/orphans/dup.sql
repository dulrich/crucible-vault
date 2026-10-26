WITH m AS (SELECT f.id, f.name, f.ext, f.size, f.path FROM files f WHERE f.is_md=0 AND f.name LIKE '%\_MD5' ESCAPE '\'
    AND lower(f.ext) IN ('avif','bmp','gif','jpeg','jpg','png','svg','webp','flac','m4a','mp3','ogg','wav','webm','3gp','mkv','mov','mp4','ogv','pdf')),
o AS (SELECT * FROM m WHERE NOT EXISTS (SELECT 1 FROM links l WHERE l.resolved_id=m.id)),
lk AS (SELECT * FROM m WHERE EXISTS (SELECT 1 FROM links l WHERE l.resolved_id=m.id))
SELECT 'orphan_with_linked_samename_twin', COUNT(*), ROUND(SUM(size)/1048576.0,1) FROM o WHERE EXISTS (SELECT 1 FROM lk WHERE lk.name=o.name AND lk.ext=o.ext)
UNION ALL SELECT 'twin_same_size', COUNT(*), ROUND(SUM(size)/1048576.0,1) FROM o WHERE EXISTS (SELECT 1 FROM lk WHERE lk.name=o.name AND lk.ext=o.ext AND lk.size=o.size)
UNION ALL SELECT 'orphan_folders_distinct', COUNT(DISTINCT substr(path,1,instr(path,'/'))), NULL FROM o
UNION ALL SELECT 'orphans_under_attachments_dir', COUNT(*), NULL FROM o WHERE path LIKE '%\_attachments/%' ESCAPE '\';
