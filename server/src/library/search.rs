//! 中文安全搜索：1–2 字符走规范化列的 LIKE 前缀（普通索引），
//! 3 字符及以上走 FTS5 trigram 字面短语；特殊字符一律按普通字符处理。

use crate::db::{Db, Value};
use crate::library::cursor::filter_digest;
use crate::library::query::{
    Page, QueryError, TRACK_COLUMNS, Where, build_page, keys_sort_id, track_json,
};

pub const MAX_QUERY_CHARS: usize = 64;

/// 转义 LIKE 通配符：`\`、`%`、`_` 均按用户输入的普通字符处理。
pub fn escape_like(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    for ch in value.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// FTS5 字面短语：双引号加倍后整体包裹，禁止把用户文本当表达式执行。
pub fn fts_phrase(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn prefix_digest(norm: &str) -> String {
    filter_digest(&format!("search|prefix|{norm}"))
}

fn fts_digest(norm: &str) -> String {
    filter_digest(&format!("search|fts|{norm}"))
}

/// 供处理器解码游标用：与 `search_page` 内部一致的摘要选择规则。
pub fn digest_for(norm: &str) -> String {
    if norm.chars().count() >= 3 {
        fts_digest(norm)
    } else {
        prefix_digest(norm)
    }
}

fn sort_key(row: &[Value]) -> (String, i64) {
    (
        row[6].as_text().unwrap_or_default().to_owned(),
        row[0].as_i64().unwrap_or(i64::MAX),
    )
}

/// 单列前缀查询 SQL + 绑定参数。`column` 只能来自内部白名单常量；
/// 模式串与游标值一律走绑定参数，不拼进 SQL 文本。
pub fn prefix_sql(
    column: &str,
    pattern: &str,
    cursor: Option<&[String]>,
    limit: usize,
) -> (String, Vec<Value>) {
    let mut filter = Where::default();
    filter.add(
        &format!("{column} COLLATE NOCASE LIKE ? ESCAPE '\\'"),
        [Where::text(pattern)],
    );
    if let Some(keys) = cursor {
        filter.add_keyset("sort_title", &keys[0], keys.get(1));
    }
    let sql = format!(
        "SELECT {TRACK_COLUMNS} FROM tracks {}
         ORDER BY sort_title COLLATE NOCASE ASC, id ASC LIMIT ?",
        filter.sql()
    );
    (sql, filter.into_params(limit))
}

/// FTS5 短语查询 SQL + 绑定参数。注意：fts5 表一旦起别名，
/// `别名 MATCH ?` 会被 SQLite 判为 `no such column`，所以固定用表名参与 JOIN 与 MATCH。
pub fn fts_sql(phrase: &str, cursor: Option<&[String]>, limit: usize) -> (String, Vec<Value>) {
    let mut filter = Where::default();
    filter.add("tracks_fts MATCH ?", [Where::text(phrase)]);
    if let Some(keys) = cursor {
        filter.add_keyset("tracks.sort_title", &keys[0], keys.get(1));
    }
    let sql = format!(
        "SELECT tracks.id, tracks.title, tracks.artist, tracks.album_id,
                tracks.duration_ms, tracks.available, tracks.sort_title
         FROM tracks_fts
         JOIN tracks ON tracks.id = tracks_fts.rowid
         {}
         ORDER BY tracks.sort_title COLLATE NOCASE ASC, tracks.id ASC LIMIT ?",
        filter.sql()
    );
    (sql, filter.into_params(limit))
}

/// 搜索分页入口：`norm` 必须已经过 normalize()。
pub async fn search_page(
    db: &Db,
    norm: &str,
    cursor: Option<&[String]>,
    limit: usize,
) -> Result<Page, QueryError> {
    if norm.chars().count() >= 3 {
        let (sql, params) = fts_sql(&fts_phrase(norm), cursor, limit + 1);
        let rows = db.query_rows(&sql, params).await?;
        return build_page(rows, limit, &fts_digest(norm), keys_sort_id, track_json);
    }
    // 短前缀（trigram 片段不足 3 字符）：三列各跑一次索引查询，
    // 再按全局 (sort_title,id) 归并去重，交给 build_page 统一裁页与出游标。
    let pattern = format!("{}%", escape_like(norm));
    let mut merged: Vec<Vec<Value>> = Vec::new();
    for column in ["sort_title", "sort_artist", "sort_album"] {
        let (sql, params) = prefix_sql(column, &pattern, cursor, limit + 1);
        for row in db.query_rows(&sql, params).await? {
            if !merged.iter().any(|kept| sort_key(kept) == sort_key(&row)) {
                merged.push(row);
            }
        }
    }
    merged.sort_by_key(|row| sort_key(row));
    build_page(
        merged,
        limit,
        &prefix_digest(norm),
        keys_sort_id,
        track_json,
    )
}

#[cfg(test)]
mod tests {
    use super::{escape_like, fts_phrase};

    #[test]
    fn like_wildcards_are_literalized() {
        assert_eq!(escape_like("50%_off\\x"), r"50\%\_off\\x");
    }

    #[test]
    fn fts_quotes_are_doubled_and_wrapped() {
        assert_eq!(fts_phrase("\"周杰\" %_"), "\"\"\"周杰\"\" %_\"");
    }
}
