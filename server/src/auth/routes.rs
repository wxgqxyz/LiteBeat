//! `/api/v1/auth/*` 路由。错误统一走 `error::json_error`，JSON 里的 ID 是十进制字符串，
//! 会话令牌只出现在 `Set-Cookie`，绝不出现在响应体或日志里。

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{DefaultBodyLimit, Extension, Json, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

use crate::auth::csrf;
use crate::auth::password;
use crate::auth::rate_limit::{AuthLimits, RateLimited, VerifyError};
use crate::auth::session::{self, SessionError, encode_hex};
use crate::auth::{AuthError, authenticate, extract_session_token};
use crate::db::{Db, DbError, Value};
use crate::error::json_error;
use crate::http::AppState;

/// 仓库还没有请求 ID 中间件，沿用既有占位值。
const REQUEST_ID: &str = "unassigned";
/// 登录请求体上限：用户名与口令都远小于该值。
const LOGIN_BODY_LIMIT: usize = 8 * 1024;
/// 限流键保留的最大字符数，防止超长输入撑大键表；
/// `admin create` 用同一上限校验用户名，否则超长账号永远无法登录。
pub const MAX_KEY_CHARS: usize = 64;
/// 校验排队已满时给客户端的重试提示。
const VERIFY_BUSY_RETRY: Duration = Duration::from_secs(2);
const FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
const REAL_IP: HeaderName = HeaderName::from_static("x-real-ip");

/// 组装鉴权路由。防护状态随路由实例创建，生产进程只有一个 `Router`。
pub fn auth_router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/session", get(session_info))
        .route("/api/v1/auth/logout", post(logout))
        .layer(DefaultBodyLimit::max(LOGIN_BODY_LIMIT))
        .layer(Extension(Arc::new(AuthLimits::new())))
}

#[derive(Debug, Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Debug, Serialize)]
struct SessionResponse {
    user_id: String,
    username: String,
    csrf_token: String,
}

async fn login(
    State(state): State<AppState>,
    Extension(limits): Extension<Arc<AuthLimits>>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Response {
    let Some(db) = state.db.as_deref() else {
        return not_ready();
    };
    // 先做无成本的来源校验，再限流，最后才碰 Argon2。
    if let Err(error) = csrf::verify_origin(&headers) {
        return error.to_response(REQUEST_ID);
    }
    let username = bounded_key(body.username.trim());
    if let Err(limited) = limits
        .logins
        .check(&client_key(&headers), &username, Instant::now())
    {
        return rate_limited(limited);
    }
    let account = match lookup_user(db, &username).await {
        Ok(account) => account,
        Err(error) => return AuthError::from(error).to_response(REQUEST_ID),
    };
    // 账号不存在时也校验一个哑哈希，让两条分支的耗时一致，无法用响应时间枚举用户名。
    let stored_hash = match &account {
        Some((_user_id, hash)) => hash.clone(),
        None => match password::dummy_hash() {
            Some(hash) => hash.to_owned(),
            None => return unauthorized(),
        },
    };
    let verified = match limits.verifies.verify(stored_hash, body.password).await {
        Ok(verified) => verified,
        Err(VerifyError::Busy) => {
            return rate_limited(RateLimited {
                retry_after: VERIFY_BUSY_RETRY,
            });
        }
        Err(VerifyError::Unavailable | VerifyError::Dropped) => return internal(),
        // 库中哈希损坏不外露细节，按凭据错误处理。
        Err(VerifyError::Password(_)) => return unauthorized(),
    };
    let Some((user_id, _)) = account.filter(|_| verified) else {
        return unauthorized();
    };
    match session::issue(db, user_id).await {
        Ok(issued) => (
            StatusCode::NO_CONTENT,
            [
                (
                    header::SET_COOKIE,
                    session::session_cookie(&issued.raw_token),
                ),
                (header::CACHE_CONTROL, "no-store".to_owned()),
            ],
        )
            .into_response(),
        Err(error) => session_error(error),
    }
}

async fn session_info(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(db) = state.db.as_deref() else {
        return not_ready();
    };
    let session = match authenticate(db, &headers).await {
        Ok(session) => session,
        Err(error) => return error.to_response(REQUEST_ID),
    };
    let rows = match db
        .query_rows(
            "SELECT username FROM users WHERE id = ?",
            vec![Value::Integer(session.user_id)],
        )
        .await
    {
        Ok(rows) => rows,
        Err(error) => return AuthError::from(error).to_response(REQUEST_ID),
    };
    // 会话指向已被删除的用户时按未登录处理。
    let Some(username) = rows
        .first()
        .and_then(|row| row.first())
        .and_then(Value::as_text)
    else {
        return AuthError::Unauthorized.to_response(REQUEST_ID);
    };
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(SessionResponse {
            user_id: session.user_id.to_string(),
            username: username.to_owned(),
            csrf_token: encode_hex(&session.csrf_token),
        }),
    )
        .into_response()
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(db) = state.db.as_deref() else {
        return not_ready();
    };
    if let Err(error) = csrf::verify_origin(&headers) {
        return error.to_response(REQUEST_ID);
    }
    let session = match authenticate(db, &headers).await {
        Ok(session) => session,
        Err(error) => return error.to_response(REQUEST_ID),
    };
    if let Err(error) = csrf::verify_token(&session, &headers) {
        return error.to_response(REQUEST_ID);
    }
    let Some(raw_token) = extract_session_token(&headers) else {
        return AuthError::Unauthorized.to_response(REQUEST_ID);
    };
    if let Err(error) = session::revoke(db, &raw_token).await {
        return AuthError::from(error).to_response(REQUEST_ID);
    }
    (
        StatusCode::NO_CONTENT,
        [
            (header::SET_COOKIE, session::cleared_cookie()),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
    )
        .into_response()
}

/// 参数化查账号；用户名走列上的 NOCASE 唯一约束，不做字符串拼接。
async fn lookup_user(db: &Db, username: &str) -> Result<Option<(i64, String)>, DbError> {
    let rows = db
        .query_rows(
            "SELECT id, password_hash FROM users WHERE username = ? LIMIT 1",
            vec![Value::Text(username.to_owned())],
        )
        .await?;
    Ok(rows.into_iter().next().and_then(|row| {
        let mut cells = row.into_iter();
        let user_id = cells.next()?.as_i64()?;
        let hash = cells.next()?.as_text()?.to_owned();
        Some((user_id, hash))
    }))
}

fn client_key(headers: &HeaderMap) -> String {
    for name in [FORWARDED_FOR, REAL_IP] {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
            let candidate = value.split(',').next().unwrap_or_default().trim();
            if !candidate.is_empty() {
                return bounded_key(candidate);
            }
        }
    }
    // 没有代理头的直连请求共用一个桶：本机单用户部署下真正守住内存的是校验闸门。
    "direct".to_owned()
}

fn bounded_key(value: &str) -> String {
    value.chars().take(MAX_KEY_CHARS).collect()
}

fn unauthorized() -> Response {
    json_error(
        StatusCode::UNAUTHORIZED,
        "UNAUTHORIZED",
        "用户名或口令不正确",
        REQUEST_ID,
    )
}

fn rate_limited(limited: RateLimited) -> Response {
    let mut response = json_error(
        StatusCode::TOO_MANY_REQUESTS,
        "RATE_LIMITED",
        "登录尝试过于频繁，请稍后重试",
        REQUEST_ID,
    );
    let seconds = limited.retry_after_seconds().max(1).to_string();
    if let Ok(value) = HeaderValue::from_str(&seconds) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

fn not_ready() -> Response {
    json_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "NOT_READY",
        "服务尚未就绪",
        REQUEST_ID,
    )
}

fn internal() -> Response {
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "INTERNAL",
        "服务暂时不可用",
        REQUEST_ID,
    )
}

fn session_error(error: SessionError) -> Response {
    match error {
        SessionError::Database(inner) => AuthError::from(inner).to_response(REQUEST_ID),
        SessionError::Random(_) => internal(),
    }
}
