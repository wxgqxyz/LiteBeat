//! T03 鉴权集成测试：直接驱动 `http::router`，用真实 Db（TempDir 保活）验证
//! 登录、会话、CSRF、Origin、限流顺序与 Argon2id 口令哈希。
//!
//! `tower::ServiceExt::oneshot` 会消费服务，因此这里用 `Service::call` 复用同一个
//! Router —— 限流与口令校验状态就挂在这个 Router 上，重建会丢状态。

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use serde_json::Value as Json;
use tower::Service;

use litebeat::auth::password;
use litebeat::auth::{SESSION_COOKIE, hash_token, session};
use litebeat::db::{Db, DbOptions, Value};
use litebeat::http::{AppState, router};

const HOST: &str = "localhost:8090";
const ORIGIN: &str = "http://localhost:8090";
const USERNAME: &str = "admin";
const PASSWORD: &str = "Litebeat!2026-test";

/// 全部用例共用一次 Argon2id 哈希，避免每个用例都付 32 MiB 的成本。
fn admin_hash() -> &'static str {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| password::hash_password(PASSWORD).expect("测试口令哈希"))
}

struct TestApp {
    router: Router,
    db: Arc<Db>,
    /// 声明顺序决定丢弃顺序：Router、Db、TempDir，确保连接关闭后才删目录。
    _dir: tempfile::TempDir,
}

impl TestApp {
    fn new() -> Self {
        Self::build(true)
    }

    /// 建了库但不交给 Router，用于验证依赖未就绪时的 503。
    fn without_db() -> Self {
        Self::build(false)
    }

    fn build(serve_db: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<html></html>").unwrap();
        let db = Db::open(dir.path().join("litebeat.db"), DbOptions::default()).unwrap();
        let router = router(AppState {
            web_dir: dir.path().to_path_buf(),
            ready: Arc::new(AtomicBool::new(true)),
            db: serve_db.then(|| Arc::clone(&db)),
            media: Arc::new(litebeat::media::MediaEnv::default()),
            scans: Arc::new(litebeat::scanner::ScanEnv::default()),
        });
        Self {
            router,
            db,
            _dir: dir,
        }
    }

    async fn call(&mut self, request: Request<Body>) -> Response {
        self.router.call(request).await.unwrap()
    }

    async fn seed_admin(&self, username: &str) -> i64 {
        self.db
            .execute(
                "INSERT INTO users(username, password_hash) VALUES (?, ?)",
                vec![
                    Value::Text(username.into()),
                    Value::Text(admin_hash().into()),
                ],
            )
            .await
            .unwrap()
            .last_rowid
    }

    async fn insert_session(&self, user_id: i64, raw: &str, offset: &str) {
        let csrf: Vec<u8> = raw.as_bytes().iter().copied().cycle().take(32).collect();
        self.db
            .execute(
                "INSERT INTO sessions(token_hash, user_id, csrf_token, expires_at)
                 VALUES (?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?))",
                vec![
                    Value::Blob(hash_token(raw)),
                    Value::Integer(user_id),
                    Value::Blob(csrf),
                    Value::Text(offset.into()),
                ],
            )
            .await
            .unwrap();
    }

    async fn count(&self, sql: &str) -> i64 {
        let rows = self.db.query_rows(sql, vec![]).await.unwrap();
        rows[0][0].as_i64().unwrap()
    }

    async fn first_row(&self, sql: &str, raw_token: &str) -> Vec<Value> {
        self.db
            .query_rows(sql, vec![Value::Blob(hash_token(raw_token))])
            .await
            .unwrap()
            .into_iter()
            .next()
            .expect("应有会话行")
    }

    async fn login(&mut self, username: &str, password: &str, client: &str) -> Response {
        self.call(login_request(username, password, client)).await
    }

    /// 登录成功并返回会话令牌。
    async fn login_ok(&mut self, client: &str) -> String {
        let response = self.login(USERNAME, PASSWORD, client).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        issued_cookie(&response).expect("登录成功应有 Set-Cookie")
    }

    async fn session_info(&mut self, cookie: Option<&str>) -> Response {
        let mut builder = Request::builder()
            .uri("/api/v1/auth/session")
            .header(header::HOST, HOST);
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, format!("{SESSION_COOKIE}={cookie}"));
        }
        self.call(builder.body(Body::empty()).unwrap()).await
    }

    async fn csrf_for(&mut self, cookie: &str) -> String {
        let response = self.session_info(Some(cookie)).await;
        assert_eq!(response.status(), StatusCode::OK);
        json_body(response).await["csrf_token"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn logout(&mut self, cookie: &str, csrf: Option<&str>, origin: Option<&str>) -> Response {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/auth/logout")
            .header(header::HOST, HOST)
            .header(header::COOKIE, format!("{SESSION_COOKIE}={cookie}"));
        if let Some(origin) = origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        if let Some(csrf) = csrf {
            builder = builder.header("x-csrf-token", csrf);
        }
        self.call(builder.body(Body::empty()).unwrap()).await
    }
}

impl Drop for TestApp {
    /// 先关停数据库工作线程，再让 TempDir 删除文件，否则 Windows 上删不掉。
    fn drop(&mut self) {
        Arc::clone(&self.db).close();
    }
}

fn login_request(username: &str, password: &str, client: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/api/v1/auth/login")
        .header(header::HOST, HOST)
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-forwarded-for", client)
        .body(Body::from(format!(
            r#"{{"username":"{username}","password":"{password}"}}"#
        )))
        .unwrap()
}

fn header_value(response: &Response, name: header::HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// 取 `Set-Cookie` 里的会话令牌值；清除 Cookie 时返回空串。
fn issued_cookie(response: &Response) -> Option<String> {
    let header = header_value(response, header::SET_COOKIE)?;
    let pair = header.split(';').next().unwrap_or_default().trim();
    pair.strip_prefix(&format!("{SESSION_COOKIE}="))
        .map(str::to_owned)
}

async fn json_body(response: Response) -> Json {
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn error_code(response: Response) -> String {
    json_body(response).await["error"]["code"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn login_sets_session_cookie_and_session_reports_identity() {
    let mut app = TestApp::new();
    let user_id = app.seed_admin(USERNAME).await;
    let response = app.login(USERNAME, PASSWORD, "10.0.0.1").await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let cookie = issued_cookie(&response).expect("登录成功应有 Set-Cookie");
    assert_eq!(cookie.len(), session::TOKEN_BYTES * 2);
    assert_eq!(cookie, cookie.to_lowercase());
    assert!(
        to_bytes(response.into_body(), 1024)
            .await
            .unwrap()
            .is_empty(),
        "204 不应带响应体"
    );

    let response = app.session_info(Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_value(&response, header::CACHE_CONTROL).as_deref(),
        Some("no-store")
    );
    let body = json_body(response).await;
    assert_eq!(body["user_id"], Json::String(user_id.to_string()));
    assert_eq!(body["username"], Json::String(USERNAME.into()));
    let csrf = body["csrf_token"].as_str().unwrap();
    assert_eq!(csrf.len(), session::TOKEN_BYTES * 2);
    // 原始令牌不得出现在任何响应体里。
    assert!(!body.to_string().contains(&cookie));

    // 库里存的是令牌哈希与原始 CSRF 字节。
    let stored = app
        .first_row(
            "SELECT token_hash, csrf_token FROM sessions WHERE token_hash = ?",
            &cookie,
        )
        .await;
    assert_eq!(stored[0], Value::Blob(hash_token(&cookie)));
    assert_eq!(
        stored[1],
        Value::Blob(session::decode_hex(csrf).expect("CSRF 令牌是十六进制"))
    );
}

#[tokio::test]
async fn login_sets_exactly_one_session_cookie() {
    let mut app = TestApp::new();
    app.seed_admin(USERNAME).await;
    let response = app.login(USERNAME, PASSWORD, "10.0.0.14").await;
    assert_eq!(
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .count(),
        1
    );
    assert_eq!(
        issued_cookie(&response).map(|value| value.len()),
        Some(session::TOKEN_BYTES * 2)
    );
}

#[tokio::test]
async fn session_expiry_is_seven_days() {
    let mut app = TestApp::new();
    app.seed_admin(USERNAME).await;
    let cookie = app.login_ok("10.0.0.2").await;
    let rows = app
        .db
        .query_rows(
            "SELECT COUNT(*) FROM sessions WHERE token_hash = ?
             AND expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+6 days')
             AND expires_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+7 days')",
            vec![Value::Blob(hash_token(&cookie))],
        )
        .await
        .unwrap();
    assert_eq!(rows[0][0].as_i64(), Some(1));
}

#[tokio::test]
async fn wrong_password_is_401_without_cookie() {
    let mut app = TestApp::new();
    app.seed_admin(USERNAME).await;
    let response = app.login(USERNAME, "bad password", "10.0.0.3").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(issued_cookie(&response), None);
    assert_eq!(error_code(response).await, "UNAUTHORIZED");
    assert_eq!(app.count("SELECT COUNT(*) FROM sessions").await, 0);
}

#[tokio::test]
async fn unknown_username_is_indistinguishable_from_wrong_password() {
    let mut app = TestApp::new();
    app.seed_admin(USERNAME).await;
    let mut bodies = Vec::new();
    for name in ["ghost-admin", USERNAME] {
        let response = app.login(name, "bad password", "10.0.0.4").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = json_body(response).await;
        bodies.push((
            body["error"]["code"].as_str().unwrap().to_owned(),
            body["error"]["message"].as_str().unwrap().to_owned(),
            body["error"]["request_id"].as_str().unwrap().to_owned(),
        ));
    }
    assert_eq!(bodies[0], bodies[1]);
    assert_eq!(app.count("SELECT COUNT(*) FROM sessions").await, 0);
}

#[tokio::test]
async fn expired_or_missing_cookie_is_rejected_with_json_401() {
    let mut app = TestApp::new();
    let user_id = app.seed_admin(USERNAME).await;
    app.insert_session(user_id, "expired-token", "-1 day").await;
    let response = app.session_info(Some("expired-token")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(response).await, "UNAUTHORIZED");
    // 未携带 Cookie 同样是 401 JSON，而不是 HTML 登录页。
    let response = app.session_info(None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        header_value(&response, header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
}

#[tokio::test]
async fn logout_requires_csrf_header_and_revokes_session() {
    let mut app = TestApp::new();
    app.seed_admin(USERNAME).await;
    let cookie = app.login_ok("10.0.0.5").await;
    let csrf = app.csrf_for(&cookie).await;

    let response = app.logout(&cookie, None, Some(ORIGIN)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(error_code(response).await, "CSRF_FAILED");
    // 缺少 CSRF 头不能撤销会话。
    assert_eq!(app.count("SELECT COUNT(*) FROM sessions").await, 1);

    let response = app.logout(&cookie, Some("deadbeef"), Some(ORIGIN)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(error_code(response).await, "CSRF_FAILED");

    // Origin 与 Host 不一致时，即便带上正确 CSRF 也拒绝。
    let response = app
        .logout(&cookie, Some(&csrf), Some("http://evil.example"))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(error_code(response).await, "CSRF_FAILED");
    assert_eq!(app.count("SELECT COUNT(*) FROM sessions").await, 1);

    let response = app.logout(&cookie, Some(&csrf), Some(ORIGIN)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(issued_cookie(&response).as_deref(), Some(""));
    assert_eq!(app.count("SELECT COUNT(*) FROM sessions").await, 0);

    let response = app.session_info(Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn forged_origin_on_login_is_rejected() {
    let mut app = TestApp::new();
    app.seed_admin(USERNAME).await;
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/auth/login")
        .header(header::HOST, HOST)
        .header(header::ORIGIN, "http://evil.example")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-forwarded-for", "10.0.0.6")
        .body(Body::from(format!(
            r#"{{"username":"{USERNAME}","password":"{PASSWORD}"}}"#
        )))
        .unwrap();
    let response = app.call(request).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(issued_cookie(&response), None);
    assert_eq!(error_code(response).await, "CSRF_FAILED");
    assert_eq!(app.count("SELECT COUNT(*) FROM sessions").await, 0);
}

#[tokio::test]
async fn login_rejects_non_json_content_type() {
    let mut app = TestApp::new();
    app.seed_admin(USERNAME).await;
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/auth/login")
        .header(header::HOST, HOST)
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "text/plain")
        .header("x-forwarded-for", "10.0.0.7")
        .body(Body::from("username=admin"))
        .unwrap();
    let response = app.call(request).await;
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(issued_cookie(&response), None);
}

#[tokio::test]
async fn rate_limit_blocks_before_password_verification() {
    let mut app = TestApp::new();
    app.seed_admin(USERNAME).await;
    for attempt in 1..=5 {
        let response = app.login(USERNAME, "bad password", "10.0.0.8").await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "第 {attempt} 次尝试应进入口令校验"
        );
    }
    // 第 6 次起：即使口令正确也不该被校验，直接 429 且不下发会话。
    for _ in 6..=7 {
        let response = app.login(USERNAME, PASSWORD, "10.0.0.8").await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(issued_cookie(&response), None);
        assert_eq!(error_code(response).await, "RATE_LIMITED");
    }
    assert_eq!(app.count("SELECT COUNT(*) FROM sessions").await, 0);
    // 限流按来源分桶，其他来源仍可登录。
    assert!(!app.login_ok("10.0.0.9").await.is_empty());
}

#[tokio::test]
async fn rate_limit_sends_retry_after_within_window() {
    let mut app = TestApp::new();
    app.seed_admin(USERNAME).await;
    for _ in 0..5 {
        let response = app.login("nobody", "bad password", "10.0.0.10").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let response = app.login("nobody", "bad password", "10.0.0.10").await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after = header_value(&response, header::RETRY_AFTER).expect("429 需带 Retry-After");
    let seconds: u64 = retry_after.parse().unwrap();
    assert!(
        (1..=60).contains(&seconds),
        "意外的 Retry-After: {retry_after}"
    );
}

#[tokio::test]
async fn username_policy_is_per_account_not_per_source() {
    let mut app = TestApp::new();
    app.seed_admin(USERNAME).await;
    // 轮换来源压满账号配额：第 11 次起按账号限流。
    for attempt in 1..=10 {
        let response = app
            .login(USERNAME, "bad password", &format!("10.0.1.{attempt}"))
            .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let response = app.login(USERNAME, "bad password", "10.0.1.99").await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    // 另一个账号不受影响。
    app.seed_admin("second").await;
    let response = app.login("second", "bad password", "10.0.1.99").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn active_sessions_are_capped_at_sixty_four() {
    let mut app = TestApp::new();
    let user_id = app.seed_admin(USERNAME).await;
    for index in 0..70 {
        app.insert_session(user_id, &format!("bulk-token-{index}"), "+1 day")
            .await;
    }
    assert_eq!(app.count("SELECT COUNT(*) FROM sessions").await, 70);
    let cookie = app.login_ok("10.0.0.11").await;
    assert_eq!(
        app.count("SELECT COUNT(*) FROM sessions").await,
        session::MAX_ACTIVE_SESSIONS as i64
    );
    // 新会话一定在保留集合里。
    let kept = app
        .first_row(
            "SELECT token_hash FROM sessions WHERE token_hash = ?",
            &cookie,
        )
        .await;
    assert_eq!(kept[0], Value::Blob(hash_token(&cookie)));
}

#[tokio::test]
async fn expired_sessions_are_pruned_on_login() {
    let mut app = TestApp::new();
    let user_id = app.seed_admin(USERNAME).await;
    app.insert_session(user_id, "stale-a", "-3 days").await;
    app.insert_session(user_id, "stale-b", "-1 hours").await;
    app.insert_session(user_id, "live", "+1 days").await;
    app.login_ok("10.0.0.12").await;
    // 到期行被清理，未过期的旧会话与新会话都保留（1 旧 + 1 新 = 2）。
    assert_eq!(app.count("SELECT COUNT(*) FROM sessions").await, 2);
    // 幸存的两条都还没过期。
    assert_eq!(
        app.count(
            "SELECT COUNT(*) FROM sessions
             WHERE expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn auth_endpoints_report_service_unavailable_without_db() {
    let mut app = TestApp::without_db();
    for request in [
        login_request(USERNAME, PASSWORD, "10.0.0.13"),
        Request::builder()
            .uri("/api/v1/auth/session")
            .header(header::HOST, HOST)
            .body(Body::empty())
            .unwrap(),
    ] {
        let uri = request.uri().clone();
        let response = app.call(request).await;
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "未配置 Db 时 {uri} 应回 503"
        );
        assert_eq!(error_code(response).await, "NOT_READY");
    }
}

#[tokio::test]
async fn password_hash_round_trip_rejects_wrong_password() {
    let hash = password::hash_password("另外一个口令").unwrap();
    assert!(hash.starts_with("$argon2id$v=19$"));
    assert!(hash.contains("m=32768,t=3,p=1"));
    assert!(password::verify_password(&hash, "另外一个口令").unwrap());
    assert!(!password::verify_password(&hash, "另外一个口令 ").unwrap());
    assert!(!password::verify_password(&hash, "猜的").unwrap());
}

#[test]
fn session_cookie_contract_is_local_http_by_default() {
    let issued = session::session_cookie("aabb");
    assert!(issued.starts_with("lb_session=aabb; "));
    assert!(issued.contains("HttpOnly"));
    assert!(issued.contains("SameSite=Lax"));
    assert!(issued.contains("Path=/"));
    assert!(issued.contains("Max-Age=604800"));
    // 本机 HTTP 默认不带 Secure，否则明文部署直接无法登录。
    assert!(!issued.contains("Secure"));
    assert_eq!(session::MAX_AGE_SECONDS, 7 * 24 * 60 * 60);
    assert_eq!(
        session::cleared_cookie(),
        "lb_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"
    );
}
