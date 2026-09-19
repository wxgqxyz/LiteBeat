-- 002_library_indexes：修正 T06 检索端点的分页执行计划。
-- 001 只给 tracks 建了单列排序索引，albums 完全没有排序索引。1 万行
-- EXPLAIN QUERY PLAN 实测：专辑列表每页都是 `SCAN albums` + 临时 B 树，
-- 歌手过滤则退化成扫完整个标题索引（LIMIT 并不能避免全表扫描）。
-- 排序/过滤表达式统一带 COLLATE NOCASE，与查询里的表达式同形才能命中。

-- 专辑列表按 (sort_title, id) 游标翻页。
CREATE INDEX IF NOT EXISTS idx_albums_title ON albums(sort_title COLLATE NOCASE, id);

-- 带歌手过滤的曲目列表：过滤键在前、排序键在后，使整页按索引序读取并可提前停止。
CREATE INDEX IF NOT EXISTS idx_tracks_artist_title
    ON tracks(sort_artist COLLATE NOCASE, sort_title COLLATE NOCASE, id);
