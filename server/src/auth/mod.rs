//! 会话接缝：T03 生产 session，T04 及受保护路由消费校验。
//!
//! 契约（两个任务都必须遵守，不得各自改名）：
//! - Cookie 名 `lb_session`，值为 32 字节随机令牌的十六进制小写字符串。
//! - 数据库 `sessions.token_hash` 存该令牌原始字节经 SHA-256 后的 32 字节。
//! - `sessions.expires_at` 为 UTC 文本 `strftime('%Y-%m-%dT%H:%M:%fZ','now')` 同格式。
//! - 校验通过返回 `Session`；无/无效/过期 Cookie 一律 `AuthError::Unauthorized`，
//!   媒体与 API 都据此回 401，不返回 HTML 登录页。

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use crate::db::{Db, DbError, Value};
use crate::error::json_error;

pub const SESSION_COOKIE: &str = "lb_session";

/// 已通过校验的会话，携带继续做所有权/CSRF 判断所需的最小信息。
#[derive(Debug, Clone)]
pub struct Session {
    pub user_id: i64,
    pub csrf_token: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("未登录或会话失效")]
    Unauthorized,
    #[error(transparent)]
    Database(#[from] DbError),
}

/// 令牌哈希：与登录写入路径共用，避免两处实现漂移。
pub fn hash_token(raw_hex: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(raw_hex.as_bytes());
    hasher.finalize().to_vec()
}

/// 从请求头解析 `lb_session` 令牌的原始十六进制值。
pub fn extract_session_token(headers: &HeaderMap) -> Option<String> {
    let header = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    for part in header.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix(&format!("{SESSION_COOKIE}=")) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// 校验会话令牌是否存在、未过期，并返回对应 `user_id` 与 CSRF 令牌。
/// 过期比较交给 SQLite，避免应用与数据库时区口径不一致。
pub async fn authenticate(db: &Db, headers: &HeaderMap) -> Result<Session, AuthError> {
    let Some(raw) = extract_session_token(headers) else {
        return Err(AuthError::Unauthorized);
    };
    let token_hash = hash_token(&raw);
    let rows = db
        .query_rows(
            "SELECT user_id, csrf_token FROM sessions
             WHERE token_hash = ? AND expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            vec![Value::Blob(token_hash)],
        )
        .await?;
    let Some(row) = rows.into_iter().next() else {
        return Err(AuthError::Unauthorized);
    };
    let mut iter = row.into_iter();
    let (Some(user), Some(csrf)) = (iter.next(), iter.next()) else {
        return Err(AuthError::Unauthorized);
    };
    let user_id = user.as_i64().ok_or(AuthError::Unauthorized)?;
    let Value::Blob(csrf_token) = csrf else {
        return Err(AuthError::Unauthorized);
    };
    Ok(Session {
        user_id,
        csrf_token,
    })
}

impl AuthError {
    /// 受保护路由统一据此构造错误响应：401 JSON，不泄露 SQL/路径。
    pub fn to_response(&self, request_id: &str) -> Response {
        match self {
            AuthError::Unauthorized => json_error(
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "未登录或会话已失效",
                request_id,
            ),
            AuthError::Database(DbError::QueueFull | DbError::PayloadTooLarge) => json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "LIBRARY_BUSY",
                "音乐库正在处理请求，请稍后重试",
                request_id,
            ),
            AuthError::Database(_) => json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "服务暂时不可用",
                request_id,
            ),
        }
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        self.to_response("unassigned")
    }
}

pub mod csrf;
pub mod password;
pub mod rate_limit;
pub mod routes;
pub mod session;

pub use routes::auth_router;
