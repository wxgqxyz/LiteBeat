//! 媒体路由：鉴权 → 曲目定位 → 配额 → 按范围流式响应。

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::auth::{authenticate, extract_session_token, hash_token};
use crate::db::DbError;
use crate::error::json_error;
use crate::http::AppState;

use super::body_guard::QuotaError;
use super::path::{ResolveError, resolve_track};
use super::stream::{RangeDecision, decide_range, spawn_stream_body};

const REQUEST_ID: &str = "media";

pub fn media_router() -> axum::Router<AppState> {
    axum::Router::new().route("/media/tracks/{id}", get(stream_track).head(head_track))
}

async fn stream_track(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    serve(state, id, headers, false).await
}

async fn head_track(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    serve(state, id, headers, true).await
}

async fn serve(state: AppState, id: String, headers: HeaderMap, head_only: bool) -> Response {
    let Some(db) = state.db.as_ref() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "LIBRARY_BUSY",
            "服务尚未就绪",
            REQUEST_ID,
        );
    };
    if let Err(err) = authenticate(db, &headers).await {
        return err.to_response(REQUEST_ID);
    }
    let Ok(track_id) = id.parse::<i64>() else {
        return not_found();
    };
    let env = state.media.clone();
    let track = match resolve_track(db, &env.allowed_roots, track_id).await {
        Ok(track) => track,
        Err(ResolveError::NotFound) => return not_found(),
        Err(ResolveError::Database(DbError::QueueFull | DbError::PayloadTooLarge)) => {
            return library_busy();
        }
        Err(ResolveError::Database(_) | ResolveError::Io(_)) => return internal(),
    };
    // HEAD 只回元数据，不占用流配额；GET 的配额由响应体守卫持有到流结束。
    if head_only {
        return metadata_response(&track, StatusCode::OK, None);
    }
    let session_key = match extract_session_token(&headers) {
        Some(raw) => hash_token(&raw),
        None => return not_found(),
    };
    let guard = match env.quota.try_acquire(session_key) {
        Ok(guard) => guard,
        Err(QuotaError::GlobalFull) => return library_busy(),
        Err(QuotaError::SessionFull) => {
            return json_error(
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "当前会话并发播放流已达上限",
                REQUEST_ID,
            );
        }
    };
    let range = headers.get(header::RANGE).map(|value| value.to_str().ok());
    let if_range = headers.get(header::IF_RANGE).and_then(|v| v.to_str().ok());
    let super::path::ResolvedTrack { file, len, mime } = track;
    let decision = match range {
        Some(None) => RangeDecision::Unsatisfiable,
        Some(Some(text)) => decide_range(Some(text), if_range, len),
        None => RangeDecision::Full,
    };
    // 守卫在这里仍被持有；提前返回的路径靠 Drop 归还配额。
    match decision {
        RangeDecision::Unsatisfiable => {
            let mut response = json_error(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "RANGE_NOT_SATISFIABLE",
                "范围请求不可满足",
                REQUEST_ID,
            );
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                axum::http::HeaderValue::from_str(&format!("bytes */{len}"))
                    .expect("数值 Content-Range"),
            );
            response
        }
        RangeDecision::Full => streaming_response(file, mime, len, 0, len, &env, guard),
        RangeDecision::Partial { start, end } => {
            streaming_response(file, mime, len, start, end - start + 1, &env, guard)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn streaming_response(
    file: tokio::fs::File,
    mime: String,
    total: u64,
    start: u64,
    len: u64,
    env: &super::MediaEnv,
    guard: super::body_guard::StreamGuard,
) -> Response {
    let body = spawn_stream_body(
        file,
        start,
        len,
        env.chunk_bytes,
        env.no_progress_timeout,
        guard,
    );
    let status = if start == 0 && len == total {
        StatusCode::OK
    } else {
        StatusCode::PARTIAL_CONTENT
    };
    let content_range = if status == StatusCode::PARTIAL_CONTENT {
        Some(format!("bytes {start}-{}/{}", start + len - 1, total))
    } else {
        None
    };
    let mut response = metadata_response_fields(&mime, len, status, content_range).into_response();
    *response.body_mut() = body;
    response
}

/// HEAD 与成功 GET 共用的元数据头；HEAD 响应体恒为空。
fn metadata_response(
    track: &super::path::ResolvedTrack,
    status: StatusCode,
    content_range: Option<String>,
) -> Response {
    metadata_response_fields(&track.mime, track.len, status, content_range).into_response()
}

fn metadata_response_fields(
    mime: &str,
    len: u64,
    status: StatusCode,
    content_range: Option<String>,
) -> Response {
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, len.to_string())
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "private, no-store");
    if let Some(range) = content_range {
        builder = builder.header(header::CONTENT_RANGE, range);
    }
    builder
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn not_found() -> Response {
    json_error(StatusCode::NOT_FOUND, "NOT_FOUND", "资源不存在", REQUEST_ID)
}

fn library_busy() -> Response {
    let mut response = json_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "LIBRARY_BUSY",
        "播放通道已满，请稍后重试",
        REQUEST_ID,
    );
    response.headers_mut().insert(
        header::RETRY_AFTER,
        axum::http::HeaderValue::from_static("1"),
    );
    response
}

fn internal() -> Response {
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "INTERNAL",
        "服务暂时不可用",
        REQUEST_ID,
    )
}
