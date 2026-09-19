//! 列表端点的共享查询：keyset（游标）分页、白名单排序、参数化绑定，
//! 只取播放所需字段；一律多取一行判定 has_more，不做全表 COUNT。

use serde_json::{Value as Json, json};

use crate::db::{Db, DbError, Value};
use crate::library::cursor::{CursorError, encode, filter_digest};

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Cursor(#[from] CursorError),
}

pub struct Page {
    pub items: Vec<Json>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

impl Page {
    pub fn to_json(&self) -> Json {
        json!({
            "items": self.items,
            "next_cursor": self.next_cursor,
            "has_more": self.has_more,
        })
    }
}

fn migration_error() -> QueryError {
    QueryError::Db(DbError::Migration("行形状不符合投影".into()))
}

/// 把（多取一行的）结果集组装为分页；`keys_of` 从末行提取游标键。
pub fn build_page(
    rows: Vec<Vec<Value>>,
    limit: usize,
    digest: &str,
    keys_of: fn(&[Value]) -> Vec<String>,
    item_of: fn(&[Value]) -> Option<Json>,
) -> Result<Page, QueryError> {
    let has_more = rows.len() > limit;
    let mut rows = rows;
    if has_more {
        rows.pop();
    }
    let items = rows
        .iter()
        .map(|row| item_of(row))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(migration_error)?;
    let next_cursor = match (has_more, rows.last()) {
        (true, Some(last)) => Some(encode(digest, &keys_of(last))?),
        _ => None,
    };
    Ok(Page {
        items,
        next_cursor,
        has_more,
    })
}

/// tracks 通用投影列序：id,title,artist,album_id,duration_ms,available,sort_title
pub const TRACK_COLUMNS: &str = "id, title, artist, album_id, duration_ms, available, sort_title";

pub fn track_json(row: &[Value]) -> Option<Json> {
    Some(json!({
        "id": row.first()?.as_i64()?.to_string(),
        "title": row.get(1)?.as_text()?,
        "artist": row.get(2)?.as_text()?,
        "album_id": row.get(3)?.as_i64().map(|id| id.to_string()),
        "duration_ms": row.get(4)?.as_i64(),
        "available": row.get(5)?.as_i64()? == 1,
    }))
}

/// albums 投影列序：id,title,album_artist,sort_title
fn album_json(row: &[Value]) -> Option<Json> {
    Some(json!({
        "id": row.first()?.as_i64()?.to_string(),
        "title": row.get(1)?.as_text()?,
        "album_artist": row.get(2)?.as_text()?,
    }))
}

pub(crate) fn keys_sort_id(row: &[Value]) -> Vec<String> {
    vec![
        row.last()
            .and_then(Value::as_text)
            .unwrap_or_default()
            .to_owned(),
        row.first()
            .and_then(Value::as_i64)
            .map(|v| v.to_string())
            .unwrap_or_default(),
    ]
}

fn keys_id(row: &[Value]) -> Vec<String> {
    vec![
        row.first()
            .and_then(Value::as_i64)
            .map(|v| v.to_string())
            .unwrap_or_default(),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackSort {
    Title,
    Recent,
}

impl TrackSort {
    pub fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw.unwrap_or("title") {
            "title" => Ok(Self::Title),
            "recent" => Ok(Self::Recent),
            other => Err(format!("sort 只允许 title|recent，收到 {other}")),
        }
    }

    pub fn digest(self, artist: Option<&str>) -> String {
        filter_digest(&format!(
            "tracks|{}|artist={}",
            match self {
                Self::Title => "title",
                Self::Recent => "recent",
            },
            artist.unwrap_or("\u{0}")
        ))
    }
}

/// WHERE 片段收集器：只拼接白名单固定片段与 `?` 占位符，
/// 用户输入一律按出现顺序进入 `params`，绝不拼进 SQL 文本。
#[derive(Default)]
pub(crate) struct Where {
    clauses: Vec<String>,
    params: Vec<Value>,
}

impl Where {
    pub(crate) fn add(&mut self, clause: &str, params: impl IntoIterator<Item = Value>) {
        self.clauses.push(clause.to_owned());
        self.params.extend(params);
    }

    /// (排序键, id) 复合游标谓词。`column` 只能来自内部白名单常量，
    /// 因为它会进入 SQL 文本；游标值一律走绑定参数。
    pub(crate) fn add_keyset(&mut self, column: &str, key: &str, id: Option<&String>) {
        self.add(
            &format!(
                "({column} COLLATE NOCASE > ? \
                 OR ({column} COLLATE NOCASE = ? AND id > ?))"
            ),
            [Self::text(key), Self::text(key), Self::int(id)],
        );
    }

    pub(crate) fn text(value: &str) -> Value {
        Value::Text(value.to_owned())
    }

    pub(crate) fn int(value: Option<&String>) -> Value {
        Value::Integer(value.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    pub(crate) fn sql(&self) -> String {
        if self.clauses.is_empty() {
            return "WHERE 1".to_owned();
        }
        format!("WHERE {}", self.clauses.join(" AND "))
    }

    pub(crate) fn into_params(self, limit: usize) -> Vec<Value> {
        let mut params = self.params;
        params.push(Value::Integer(limit as i64));
        params
    }
}

/// 曲目分页 SQL + 绑定参数（公开以便测试直接对同款 SQL 做 EXPLAIN）。
/// 游标与过滤器存在时才写对应谓词：写成 `(?N IS NULL OR ...)` 会让
/// SQLite 放弃索引区间扫描，整页退化为扫描后过滤。
pub fn tracks_sql(
    artist: Option<&str>,
    sort: TrackSort,
    cursor: Option<&[String]>,
    limit: usize,
) -> (String, Vec<Value>) {
    let mut filter = Where::default();
    if let Some(artist) = artist {
        filter.add("sort_artist COLLATE NOCASE = ?", [Where::text(artist)]);
    }
    let order = match sort {
        TrackSort::Title => {
            if let Some(keys) = cursor {
                filter.add_keyset("sort_title", &keys[0], keys.get(1));
            }
            "sort_title COLLATE NOCASE ASC, id ASC"
        }
        TrackSort::Recent => {
            if let Some(keys) = cursor {
                filter.add("id < ?", [Where::int(keys.first())]);
            }
            "id DESC"
        }
    };
    let sql = format!(
        "SELECT {TRACK_COLUMNS} FROM tracks {} ORDER BY {order} LIMIT ?",
        filter.sql()
    );
    (sql, filter.into_params(limit))
}

pub async fn tracks_page(
    db: &Db,
    artist: Option<&str>,
    sort: TrackSort,
    cursor: Option<&[String]>,
    limit: usize,
) -> Result<Page, QueryError> {
    let (sql, params) = tracks_sql(artist, sort, cursor, limit + 1);
    let rows = db.query_rows(&sql, params).await?;
    let digest = sort.digest(artist);
    let keys_of: fn(&[Value]) -> Vec<String> = if sort == TrackSort::Recent {
        keys_id
    } else {
        keys_sort_id
    };
    build_page(rows, limit, &digest, keys_of, track_json)
}

/// 专辑分页 SQL（按 (sort_title,id)）；公开以便测试直接做 EXPLAIN。
pub fn albums_sql(cursor: Option<&[String]>, limit: usize) -> (String, Vec<Value>) {
    let mut filter = Where::default();
    if let Some(keys) = cursor {
        filter.add_keyset("sort_title", &keys[0], keys.get(1));
    }
    let sql = format!(
        "SELECT id, title, album_artist, sort_title FROM albums {}
         ORDER BY sort_title COLLATE NOCASE ASC, id ASC LIMIT ?",
        filter.sql()
    );
    (sql, filter.into_params(limit))
}

/// 专辑分页（按 (sort_title,id)）。
pub async fn albums_page(
    db: &Db,
    cursor: Option<&[String]>,
    limit: usize,
) -> Result<Page, QueryError> {
    let (sql, params) = albums_sql(cursor, limit + 1);
    let rows = db.query_rows(&sql, params).await?;
    let digest = filter_digest("albums");
    build_page(rows, limit, &digest, keys_sort_id, album_json)
}

/// 专辑内曲目分页 SQL（按 (disc_no,track_no,id)）；公开以便测试做 EXPLAIN。
pub fn album_tracks_sql(
    album_id: i64,
    cursor: Option<&[String]>,
    limit: usize,
) -> (String, Vec<Value>) {
    let mut filter = Where::default();
    filter.add("album_id = ?", [Value::Integer(album_id)]);
    if let Some(keys) = cursor {
        filter.add(
            "(disc_no > ? OR (disc_no = ? AND (track_no > ? \
             OR (track_no = ? AND id > ?))))",
            [
                Where::int(keys.first()),
                Where::int(keys.first()),
                Where::int(keys.get(1)),
                Where::int(keys.get(1)),
                Where::int(keys.get(2)),
            ],
        );
    }
    let sql = format!(
        "SELECT {TRACK_COLUMNS}, disc_no, track_no FROM tracks {}
         ORDER BY disc_no ASC, track_no ASC, id ASC LIMIT ?",
        filter.sql()
    );
    (sql, filter.into_params(limit))
}

/// 专辑内曲目分页（按 (disc_no,track_no,id)）；调用方需先确认专辑存在。
pub async fn album_tracks_page(
    db: &Db,
    album_id: i64,
    cursor: Option<&[String]>,
    limit: usize,
) -> Result<Page, QueryError> {
    let (sql, params) = album_tracks_sql(album_id, cursor, limit + 1);
    let rows = db.query_rows(&sql, params).await?;
    // 本投影列序：id(0)…sort_title(6),disc_no(7),track_no(8)。
    let digest = filter_digest(&format!("album_tracks|{album_id}"));
    let has_more = rows.len() > limit;
    let mut rows = rows;
    if has_more {
        rows.pop();
    }
    let items = rows
        .iter()
        .map(|row| track_json(row))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(migration_error)?;
    let next_cursor = match (has_more, rows.last()) {
        (true, Some(last)) => Some({
            let keys = vec![
                last[7].as_i64().map(|v| v.to_string()).unwrap_or_default(),
                last[8].as_i64().map(|v| v.to_string()).unwrap_or_default(),
                last[0].as_i64().map(|v| v.to_string()).unwrap_or_default(),
            ];
            encode(&digest, &keys)?
        }),
        _ => None,
    };
    Ok(Page {
        items,
        next_cursor,
        has_more,
    })
}

/// 单曲详情：只暴露播放与展示需要的字段，不泄露磁盘路径。
pub async fn track_detail(db: &Db, id: i64) -> Result<Option<Json>, DbError> {
    let rows = db
        .query_rows(
            "SELECT id, title, artist, album_id, album_title, duration_ms,
                    codec, mime, size_bytes, available
             FROM tracks WHERE id = ?1",
            vec![Value::Integer(id)],
        )
        .await?;
    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };
    let mut it = row.into_iter();
    let (Some(id), Some(title), Some(artist), Some(album_id), Some(album_title)) =
        (it.next(), it.next(), it.next(), it.next(), it.next())
    else {
        return Ok(None);
    };
    let (Some(duration), Some(codec), Some(mime), Some(size), Some(available)) =
        (it.next(), it.next(), it.next(), it.next(), it.next())
    else {
        return Ok(None);
    };
    Ok(Some(json!({
        "id": id.as_i64().map(|v| v.to_string()),
        "title": title.as_text(),
        "artist": artist.as_text(),
        "album_id": album_id.as_i64().map(|v| v.to_string()),
        "album_title": album_title.as_text(),
        "duration_ms": duration.as_i64(),
        "codec": codec.as_text(),
        "mime": mime.as_text(),
        "size_bytes": size.as_i64().map(|v| v.to_string()),
        "available": available.as_i64() == Some(1),
    })))
}
