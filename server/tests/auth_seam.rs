//! 会话接缝契约测试：T03/T04 都依赖这里的语义，改动前先看它。

use std::sync::Arc;

use axum::http::HeaderMap;
use litebeat::auth::{self, AuthError};
use litebeat::db::{Db, DbOptions, Value};

fn open(dir: &tempfile::TempDir) -> Arc<Db> {
    Db::open(dir.path().join("litebeat.db"), DbOptions::default()).unwrap()
}

async fn insert_user(db: &Db) -> i64 {
    db.execute(
        "INSERT INTO users(username, password_hash) VALUES ('admin', 'x')",
        vec![],
    )
    .await
    .unwrap()
    .last_rowid
}

async fn insert_session(db: &Db, user_id: i64, raw: &str, valid: bool) {
    let offset = if valid { "+1 day" } else { "-1 day" };
    // 必须使用与 authenticate() 比较一致的 UTC RFC 3339 文本格式。
    db.execute(
        "INSERT INTO sessions(token_hash, user_id, csrf_token, expires_at)
         VALUES (?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?))",
        vec![
            Value::Blob(auth::hash_token(raw)),
            Value::Integer(user_id),
            Value::Blob(vec![7u8; 32]),
            Value::Text(offset.into()),
        ],
    )
    .await
    .unwrap();
}

fn headers_with_cookie(raw: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::COOKIE,
        format!("other=1; {}={raw}", auth::SESSION_COOKIE)
            .parse()
            .unwrap(),
    );
    headers
}

#[tokio::test]
async fn valid_unexpired_session_authenticates() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let user = insert_user(&db).await;
    insert_session(&db, user, "deadbeef", true).await;
    let session = auth::authenticate(&db, &headers_with_cookie("deadbeef"))
        .await
        .unwrap();
    assert_eq!(session.user_id, user);
    assert_eq!(session.csrf_token, vec![7u8; 32]);
    db.close();
}

#[tokio::test]
async fn expired_or_missing_or_wrong_token_is_unauthorized() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let user = insert_user(&db).await;
    insert_session(&db, user, "valid", false).await; // 已过期

    let expired = auth::authenticate(&db, &headers_with_cookie("valid"))
        .await
        .unwrap_err();
    assert!(matches!(expired, AuthError::Unauthorized));

    let missing = auth::authenticate(&db, &HeaderMap::new())
        .await
        .unwrap_err();
    assert!(matches!(missing, AuthError::Unauthorized));

    let wrong = auth::authenticate(&db, &headers_with_cookie("guess"))
        .await
        .unwrap_err();
    assert!(matches!(wrong, AuthError::Unauthorized));
    db.close();
}

#[tokio::test]
async fn unauthenticated_maps_to_401_json() {
    let response = AuthError::Unauthorized.to_response("req-1");
    assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("UNAUTHORIZED"));
    assert!(text.contains("req-1"));
}
