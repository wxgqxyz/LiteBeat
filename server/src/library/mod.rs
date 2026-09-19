//! T06 曲库检索：tracks/albums/search/queue-ids 端点、游标分页与中文搜索。
//!
//! 分工：
//! - [`normalize`]：入库排序键与查询词共用的规范化函数。
//! - [`cursor`]：不透明游标的编解码与过滤器摘要绑定。
//! - [`query`]：列表分页 SQL 与投影。
//! - [`search`]：短前缀 LIKE 与 FTS5 trigram 搜索。

#![allow(clippy::result_large_err)]

pub mod cursor;
pub mod normalize;
pub mod query;
pub mod search;

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::auth::{Session, authenticate};
use crate::db::{Db, DbError, Value};
use crate::error::json_error;
use crate::http::AppState;
use crate::library::cursor::{CursorError, decode};
use crate::library::normalize::normalize;
use crate::library::query::{TrackSort, album_tracks_page, albums_page, track_detail, tracks_page};
use crate::library::search::{MAX_QUERY_CHARS, search_page};

const REQUEST_ID: &str = "library";
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 100;
pub const QUEUE_MAX_IDS: usize = 1000;

pub fn library_router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/tracks", get(list_tracks))
        .route("/api/v1/tracks/{id}", get(get_track))
        .route("/api/v1/albums", get(list_albums))
        .route("/api/v1/albums/{id}/tracks", get(get_album_tracks))
        .route("/api/v1/search", get(search))
        .route("/api/v1/queue-ids", get(queue_ids))
}

async fn guard(state: &AppState, headers: &HeaderMap) -> Result<(Arc<Db>, Session), Response> {
    let Some(db) = state.db.clone() else {
        return Err(json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "NOT_READY",
            "服务尚未就绪",
            REQUEST_ID,
        ));
    };
    let session = authenticate(&db, headers)
        .await
        .map_err(|error| error.to_response(REQUEST_ID))?;
    Ok((db, session))
}

fn bad_request(message: &str) -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        "INVALID_REQUEST",
        message,
        REQUEST_ID,
    )
}

fn cursor_error(error: CursorError) -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        "INVALID_CURSOR",
        &error.to_string(),
        REQUEST_ID,
    )
}

fn db_error(error: DbError) -> Response {
    match error {
        DbError::QueueFull | DbError::PayloadTooLarge => json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "LIBRARY_BUSY",
            "音乐库正在处理请求，请稍后重试",
            REQUEST_ID,
        ),
        DbError::QueryTimeout => json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "QUERY_TIMEOUT",
            "查询超时，请稍后重试",
            REQUEST_ID,
        ),
        _ => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "服务暂时不可用",
            REQUEST_ID,
        ),
    }
}

fn parse_limit(raw: Option<&String>) -> Result<usize, Response> {
    match raw {
        None => Ok(DEFAULT_LIMIT),
        Some(text) => match text.parse::<usize>() {
            Ok(0) => Err(bad_request("limit 必须在 1..=100")),
            Ok(value @ 1..=MAX_LIMIT) => Ok(value),
            _ => Err(bad_request("limit 必须在 1..=100")),
        },
    }
}

fn parse_id(raw: &str) -> Result<i64, Response> {
    raw.trim()
        .parse::<i64>()
        .map_err(|_| bad_request("id 必须是十进制字符串"))
}

fn page_response(page: crate::library::query::Page) -> Response {
    (StatusCode::OK, Json(page.to_json())).into_response()
}

fn query_error(error: crate::library::query::QueryError) -> Response {
    match error {
        crate::library::query::QueryError::Db(error) => db_error(error),
        crate::library::query::QueryError::Cursor(error) => cursor_error(error),
    }
}

async fn list_tracks(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (db, _session) = match guard(&state, &headers).await {
        Ok(guarded) => guarded,
        Err(response) => return response,
    };
    let limit = match parse_limit(params.get("limit")) {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    let sort = match TrackSort::parse(params.get("sort").map(String::as_str)) {
        Ok(sort) => sort,
        Err(message) => return bad_request(&message),
    };
    let artist = params
        .get("artist")
        .map(|value| normalize(value))
        .filter(|value| !value.is_empty());
    let digest = sort.digest(artist.as_deref());
    let cursor = match params.get("cursor") {
        Some(raw) => match decode(raw, &digest) {
            Ok(keys) => Some(keys),
            Err(error) => return cursor_error(error),
        },
        None => None,
    };
    match tracks_page(&db, artist.as_deref(), sort, cursor.as_deref(), limit).await {
        Ok(page) => page_response(page),
        Err(error) => query_error(error),
    }
}

async fn get_track(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let (db, _session) = match guard(&state, &headers).await {
        Ok(guarded) => guarded,
        Err(response) => return response,
    };
    let Ok(id) = parse_id(&id) else {
        return bad_request("id 必须是十进制字符串");
    };
    match track_detail(&db, id).await {
        Ok(Some(detail)) => (StatusCode::OK, Json(detail)).into_response(),
        Ok(None) => json_error(StatusCode::NOT_FOUND, "NOT_FOUND", "曲目不存在", REQUEST_ID),
        Err(error) => db_error(error),
    }
}

async fn list_albums(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (db, _session) = match guard(&state, &headers).await {
        Ok(guarded) => guarded,
        Err(response) => return response,
    };
    let limit = match parse_limit(params.get("limit")) {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    let digest = cursor::filter_digest("albums");
    let cursor = match params.get("cursor") {
        Some(raw) => match decode(raw, &digest) {
            Ok(keys) => Some(keys),
            Err(error) => return cursor_error(error),
        },
        None => None,
    };
    match albums_page(&db, cursor.as_deref(), limit).await {
        Ok(page) => page_response(page),
        Err(error) => query_error(error),
    }
}

async fn get_album_tracks(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (db, _session) = match guard(&state, &headers).await {
        Ok(guarded) => guarded,
        Err(response) => return response,
    };
    let Ok(album_id) = parse_id(&id) else {
        return bad_request("id 必须是十进制字符串");
    };
    let limit = match parse_limit(params.get("limit")) {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    let digest = cursor::filter_digest(&format!("album_tracks|{album_id}"));
    let cursor = match params.get("cursor") {
        Some(raw) => match decode(raw, &digest) {
            Ok(keys) => Some(keys),
            Err(error) => return cursor_error(error),
        },
        None => None,
    };
    let exists = match db
        .query_rows(
            "SELECT 1 FROM albums WHERE id = ?1",
            vec![Value::Integer(album_id)],
        )
        .await
    {
        Ok(rows) => !rows.is_empty(),
        Err(error) => return db_error(error),
    };
    if !exists {
        return json_error(StatusCode::NOT_FOUND, "NOT_FOUND", "专辑不存在", REQUEST_ID);
    }
    match album_tracks_page(&db, album_id, cursor.as_deref(), limit).await {
        Ok(page) => page_response(page),
        Err(error) => query_error(error),
    }
}

async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (db, _session) = match guard(&state, &headers).await {
        Ok(guarded) => guarded,
        Err(response) => return response,
    };
    let Some(raw) = params.get("q") else {
        return bad_request("缺少查询参数 q");
    };
    let norm = normalize(raw);
    let chars = norm.chars().count();
    if chars == 0 {
        return bad_request("查询词不能为空");
    }
    if chars > MAX_QUERY_CHARS {
        return bad_request(&format!("查询词最长 {MAX_QUERY_CHARS} 个字符"));
    }
    let limit = match parse_limit(params.get("limit")) {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    let digest = search::digest_for(&norm);
    let cursor = match params.get("cursor") {
        Some(raw) => match decode(raw, &digest) {
            Ok(keys) => Some(keys),
            Err(error) => return cursor_error(error),
        },
        None => None,
    };
    match search_page(&db, &norm, cursor.as_deref(), limit).await {
        Ok(page) => page_response(page),
        Err(error) => query_error(error),
    }
}

async fn queue_ids(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (db, session) = match guard(&state, &headers).await {
        Ok(guarded) => guarded,
        Err(response) => return response,
    };
    let scope = params.get("scope").map(String::as_str).unwrap_or("");
    let (sql, params_vec): (String, Vec<Value>) = match scope {
        "library" => (
            "SELECT id FROM tracks WHERE available = 1
             ORDER BY sort_title COLLATE NOCASE ASC, id ASC LIMIT ?1"
                .to_owned(),
            vec![Value::Integer((QUEUE_MAX_IDS + 1) as i64)],
        ),
        "album" => {
            let Ok(album_id) = parse_or_missing(&params, "album_id") else {
                return bad_request("album 范围需要数字 album_id");
            };
            (
                "SELECT id FROM tracks WHERE album_id = ?1 AND available = 1
                 ORDER BY disc_no ASC, track_no ASC, id ASC LIMIT ?2"
                    .to_owned(),
                vec![
                    Value::Integer(album_id),
                    Value::Integer((QUEUE_MAX_IDS + 1) as i64),
                ],
            )
        }
        "favorites" => (
            "SELECT t.id FROM favorites f JOIN tracks t ON t.id = f.track_id
             WHERE f.user_id = ?1 AND t.available = 1
             ORDER BY f.created_at DESC, f.track_id DESC LIMIT ?2"
                .to_owned(),
            vec![
                Value::Integer(session.user_id),
                Value::Integer((QUEUE_MAX_IDS + 1) as i64),
            ],
        ),
        "playlist" => {
            let Ok(playlist_id) = parse_or_missing(&params, "playlist_id") else {
                return bad_request("playlist 范围需要数字 playlist_id");
            };
            match db
                .query_rows(
                    "SELECT owner_id FROM playlists WHERE id = ?1",
                    vec![Value::Integer(playlist_id)],
                )
                .await
            {
                Ok(rows) => match rows.first().and_then(|row| row.first()) {
                    Some(owner) if owner.as_i64() == Some(session.user_id) => {}
                    Some(_) => {
                        return json_error(
                            StatusCode::FORBIDDEN,
                            "FORBIDDEN",
                            "无权访问该歌单",
                            REQUEST_ID,
                        );
                    }
                    None => {
                        return json_error(
                            StatusCode::NOT_FOUND,
                            "NOT_FOUND",
                            "歌单不存在",
                            REQUEST_ID,
                        );
                    }
                },
                Err(error) => return db_error(error),
            }
            (
                "SELECT t.id FROM playlist_items p JOIN tracks t ON t.id = p.track_id
                 WHERE p.playlist_id = ?1 AND t.available = 1
                 ORDER BY p.position ASC, p.id ASC LIMIT ?2"
                    .to_owned(),
                vec![
                    Value::Integer(playlist_id),
                    Value::Integer((QUEUE_MAX_IDS + 1) as i64),
                ],
            )
        }
        other => {
            return bad_request(&format!(
                "scope 只允许 library|album|favorites|playlist，收到 {other}"
            ));
        }
    };
    let rows = match db.query_rows(&sql, params_vec).await {
        Ok(rows) => rows,
        Err(error) => return db_error(error),
    };
    let truncated = rows.len() > QUEUE_MAX_IDS;
    let ids: Vec<String> = rows
        .into_iter()
        .take(QUEUE_MAX_IDS)
        .filter_map(|mut row| row.pop()?.as_i64().map(|id| id.to_string()))
        .collect();
    (
        StatusCode::OK,
        Json(json!({ "ids": ids, "truncated": truncated })),
    )
        .into_response()
}

fn parse_or_missing(
    params: &std::collections::HashMap<String, String>,
    key: &str,
) -> Result<i64, ()> {
    params
        .get(key)
        .and_then(|value| value.trim().parse::<i64>().ok())
        .ok_or(())
}
