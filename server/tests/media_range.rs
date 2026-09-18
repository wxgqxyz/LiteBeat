//! T04 媒体传输契约测试：驱动真实 `http::router`，覆盖文档规定的
//! 10000 字节 Range 协议向量、路径约束、会话鉴权与全局/单会话流配额。

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use sha2::{Digest, Sha256};
use tower::Service;

use litebeat::auth::{SESSION_COOKIE, hash_token};
use litebeat::db::{Db, DbOptions, Value};
use litebeat::http::{AppState, router};
use litebeat::media::MediaEnv;
use litebeat::media::body_guard::MediaQuota;

const HOST: &str = "localhost:8090";
const FIXTURE: &[u8] = include_bytes!("../../fixtures/media/range_10000.bin");
const FIXTURE_SHA256: &str = "3421d9aa928a94decb191ab8e8b76c1d8434bf602c5b3ba10ad42f54c8199c34";

struct App {
    router: Router,
    db: Arc<Db>,
    media: Arc<MediaEnv>,
    /// 库根目录（媒体文件所在），在临时目录内。
    root: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl App {
    fn build(global: usize, per_session: usize, chunk_bytes: usize, no_progress: Duration) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<html></html>").unwrap();
        let root = dir.path().join("music");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("track.bin"), FIXTURE).unwrap();
        let db = Db::open(dir.path().join("litebeat.db"), DbOptions::default()).unwrap();
        let media = Arc::new(MediaEnv {
            quota: Arc::new(MediaQuota::new(global, per_session)),
            chunk_bytes,
            no_progress_timeout: no_progress,
            allowed_roots: vec![std::fs::canonicalize(&root).unwrap()],
        });
        let router = router(AppState {
            web_dir: dir.path().to_path_buf(),
            ready: Arc::new(AtomicBool::new(true)),
            db: Some(Arc::clone(&db)),
            media: Arc::clone(&media),
        });
        Self {
            router,
            db,
            media,
            root,
            _dir: dir,
        }
    }

    fn default_app() -> Self {
        Self::build(32, 4, 32 * 1024, Duration::from_secs(30))
    }

    async fn call(&mut self, request: Request<Body>) -> Response {
        self.router.call(request).await.unwrap()
    }

    async fn seed_user_named(&self, name: &str) -> i64 {
        self.db
            .execute(
                "INSERT INTO users(username, password_hash) VALUES (?, 'x')",
                vec![Value::Text(name.into())],
            )
            .await
            .unwrap()
            .last_rowid
    }

    async fn seed_user(&self) -> i64 {
        self.seed_user_named("admin").await
    }

    async fn existing_root_id(&self) -> i64 {
        let rows = self
            .db
            .query_rows("SELECT id FROM library_roots LIMIT 1", vec![])
            .await
            .unwrap();
        rows[0][0].as_i64().unwrap()
    }

    /// 写入一条永不过期（相对 now +1 天）的会话，返回 Cookie 值。
    async fn seed_session(&self, user_id: i64, token: &str) {
        self.db
            .execute(
                "INSERT INTO sessions(token_hash, user_id, csrf_token, expires_at)
                 VALUES (?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+1 day'))",
                vec![
                    Value::Blob(hash_token(token)),
                    Value::Integer(user_id),
                    Value::Blob(hash_token(&format!("csrf-{token}"))),
                ],
            )
            .await
            .unwrap();
    }

    async fn seed_root(&self, name: &str, path: &std::path::Path) -> i64 {
        self.db
            .execute(
                "INSERT INTO library_roots(name, canonical_path) VALUES (?, ?)",
                vec![
                    Value::Text(name.into()),
                    Value::Text(path.display().to_string()),
                ],
            )
            .await
            .unwrap()
            .last_rowid
    }

    async fn seed_track(
        &self,
        root_id: i64,
        relative_path: &str,
        available: i64,
        mime: Option<&str>,
    ) -> i64 {
        let size = std::fs::metadata(self.root.join(relative_path.replace('/', "\\")))
            .map(|m| m.len() as i64)
            .unwrap_or(1234);
        self.db
            .execute(
                "INSERT INTO tracks(root_id, relative_path, title, size_bytes, mtime_ns,
                                    sort_title, sort_artist, sort_album, available, mime)
                 VALUES (?, ?, 'T', ?, 1, 'T', '', '', ?, ?)",
                vec![
                    Value::Integer(root_id),
                    Value::Text(relative_path.into()),
                    Value::Integer(size),
                    Value::Integer(available),
                    mime.map(|m| Value::Text(m.into())).unwrap_or(Value::Null),
                ],
            )
            .await
            .unwrap()
            .last_rowid
    }

    /// 完整的已登录播放链路准备：用户、会话、库根、曲目。
    async fn seeded(token: &str) -> (Self, i64) {
        let app = Self::default_app();
        let user = app.seed_user().await;
        app.seed_session(user, token).await;
        let root = app.seed_root("music", &app.root.clone()).await;
        let track = app.seed_track(root, "track.bin", 1, None).await;
        (app, track)
    }
}

fn request(
    method: Method,
    uri: &str,
    cookie: Option<&str>,
    range: Option<&str>,
    extra: &[(&'static str, &'static str)],
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", HOST);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, format!("{SESSION_COOKIE}={cookie}"));
    }
    if let Some(range) = range {
        builder = builder.header(header::RANGE, range);
    }
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::empty()).unwrap()
}

fn header_text(response: &Response, name: header::HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)
        .map(|v| v.to_str().unwrap().to_string())
}

async fn body_bytes(response: Response) -> Vec<u8> {
    to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

const TOKEN: &str = "d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0";

fn track_uri(id: i64) -> String {
    format!("/media/tracks/{id}")
}

/// 文档规定的协议向量（测试文件长度 = 10000 字节）。
#[tokio::test]
async fn range_protocol_vectors() {
    let (mut app, track) = App::seeded(TOKEN).await;
    let uri = track_uri(track);

    // 无 Range -> 200，完整 10000 字节，SHA-256 与夹具记录一致。
    let response = app
        .call(request(Method::GET, &uri, Some(TOKEN), None, &[]))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_text(&response, header::CONTENT_LENGTH).as_deref(),
        Some("10000")
    );
    assert_eq!(
        header_text(&response, header::ACCEPT_RANGES).as_deref(),
        Some("bytes")
    );
    assert_eq!(
        header_text(&response, header::CACHE_CONTROL).as_deref(),
        Some("private, no-store")
    );
    assert_eq!(
        header_text(&response, header::CONTENT_TYPE).as_deref(),
        Some("application/octet-stream")
    );
    let body = body_bytes(response).await;
    let mut hasher = Sha256::new();
    hasher.update(&body);
    assert_eq!(hex(hasher.finalize()), FIXTURE_SHA256);
    assert_eq!(body.len(), 10000);

    let cases: Vec<(&str, u16, std::ops::Range<usize>, &str)> = vec![
        ("bytes=0-0", 206, 0..1, "bytes 0-0/10000"),
        ("bytes=100-199", 206, 100..200, "bytes 100-199/10000"),
        ("bytes=9000-", 206, 9000..10000, "bytes 9000-9999/10000"),
        ("bytes=-100", 206, 9900..10000, "bytes 9900-9999/10000"),
        (
            "bytes=9990-99999",
            206,
            9990..10000,
            "bytes 9990-9999/10000",
        ),
    ];
    for (range, status, slice, content_range) in cases {
        let response = app
            .call(request(Method::GET, &uri, Some(TOKEN), Some(range), &[]))
            .await;
        assert_eq!(
            u16::from(response.status()),
            status,
            "范围 {range} 状态码不符"
        );
        assert_eq!(
            header_text(&response, header::CONTENT_RANGE).as_deref(),
            Some(content_range),
            "范围 {range} Content-Range 不符"
        );
        let body = body_bytes(response).await;
        assert_eq!(body, &FIXTURE[slice], "范围 {range} 字节不符");
    }
}

#[tokio::test]
async fn unsatisfiable_and_malformed_ranges_get_416() {
    let (mut app, track) = App::seeded(TOKEN).await;
    let uri = track_uri(track);
    for range in [
        "bytes=10000-",
        "bytes=10000-20000",
        "bytes=-0",
        "bytes=5-1",
        "bytes=abc-def",
        "items=0-1",
        "bytes=",
    ] {
        let response = app
            .call(request(Method::GET, &uri, Some(TOKEN), Some(range), &[]))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::RANGE_NOT_SATISFIABLE,
            "范围 {range} 应 416"
        );
        assert_eq!(
            header_text(&response, header::CONTENT_RANGE).as_deref(),
            Some("bytes */10000"),
            "范围 {range} 应携带 */总长"
        );
        let body = body_bytes(response).await;
        assert!(
            body.len() < 400 && String::from_utf8_lossy(&body).contains("RANGE_NOT_SATISFIABLE"),
            "416 必须是 JSON 错误而非媒体字节"
        );
    }
}

#[tokio::test]
async fn multi_range_and_if_range_fall_back_to_full_200() {
    let (mut app, track) = App::seeded(TOKEN).await;
    let uri = track_uri(track);
    let response = app
        .call(request(
            Method::GET,
            &uri,
            Some(TOKEN),
            Some("bytes=0-1,8-9"),
            &[],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_bytes(response).await.len(), 10000);

    let response = app
        .call(request(
            Method::GET,
            &uri,
            Some(TOKEN),
            Some("bytes=100-199"),
            &[("if-range", "\"weak-etag\"")],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_bytes(response).await.len(), 10000);
}

#[tokio::test]
async fn head_ignores_range_and_sends_no_body() {
    let (mut app, track) = App::seeded(TOKEN).await;
    let response = app
        .call(request(
            Method::HEAD,
            &track_uri(track),
            Some(TOKEN),
            Some("bytes=0-99"),
            &[],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_text(&response, header::CONTENT_LENGTH).as_deref(),
        Some("10000")
    );
    assert_eq!(
        header_text(&response, header::CONTENT_TYPE).as_deref(),
        Some("application/octet-stream")
    );
    assert_eq!(
        header_text(&response, header::ACCEPT_RANGES).as_deref(),
        Some("bytes")
    );
    assert_eq!(body_bytes(response).await.len(), 0);
}

#[tokio::test]
async fn unauthenticated_and_expired_sessions_get_401() {
    let (mut app, track) = App::seeded(TOKEN).await;
    let uri = track_uri(track);
    for cookie in [None, Some("bogus"), Some(TOKEN)] {
        let response = app
            .call(request(Method::GET, &uri, cookie, Some("bytes=0-99"), &[]))
            .await;
        if cookie.is_none() || cookie == Some("bogus") {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let body = body_bytes(response).await;
            assert!(
                body.len() < 200 && String::from_utf8_lossy(&body).contains("UNAUTHORIZED"),
                "401 必须是 JSON，且绝不发送媒体字节"
            );
        }
    }
    // 过期会话同样 401。
    let user = app.seed_user_named("admin2").await;
    let expired_raw = format!("{:02x}", 1u8).repeat(32);
    app.db
        .execute(
            "INSERT INTO sessions(token_hash, user_id, csrf_token, expires_at)
             VALUES (?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 day'))",
            vec![
                Value::Blob(hash_token(&expired_raw)),
                Value::Integer(user),
                Value::Blob(hash_token(&format!("csrf-{expired_raw}"))),
            ],
        )
        .await
        .unwrap();
    let response = app
        .call(request(Method::GET, &uri, Some(&expired_raw), None, &[]))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn path_and_availability_constraints_return_404() {
    let (mut app, _ok_track) = App::seeded(TOKEN).await;
    // 合法曲目但标记不可用。
    let root = app.existing_root_id().await;
    std::fs::write(app.root.join("hidden.bin"), FIXTURE).unwrap();
    let hidden = app.seed_track(root, "hidden.bin", 0, None).await;
    let outside = app._dir.path().join("outside.bin");
    std::fs::write(&outside, b"secret").unwrap();
    let traversal = app.seed_track(root, "../outside.bin", 1, None).await;
    let absolute = app.seed_track(root, "/etc/passwd", 1, None).await;
    // DB 根不在配置 allowed_roots 中。
    let other_dir = app._dir.path().join("elsewhere");
    std::fs::create_dir_all(&other_dir).unwrap();
    std::fs::write(other_dir.join("x.bin"), b"x").unwrap();
    let rogue_root = app.seed_root("rogue", &other_dir).await;
    let rogue_track = app.seed_track(rogue_root, "x.bin", 1, None).await;

    for id in [hidden, traversal, absolute, rogue_track, 999_999] {
        let response = app
            .call(request(Method::GET, &track_uri(id), Some(TOKEN), None, &[]))
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "曲目 {id} 应 404");
        let body = body_bytes(response).await;
        assert!(String::from_utf8_lossy(&body).contains("NOT_FOUND"));
    }
    // 非数字 ID 同样 404，且不得触达文件系统。
    let response = app
        .call(request(
            Method::GET,
            "/media/tracks/../../etc",
            Some(TOKEN),
            None,
            &[],
        ))
        .await;
    assert_ne!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn per_session_limit_rejects_fifth_stream_and_recovers_on_drop() {
    let mut app = App::build(32, 2, 1, Duration::from_secs(10));
    let user = app.seed_user().await;
    app.seed_session(user, TOKEN).await;
    let root = app.seed_root("music", &app.root.clone()).await;
    let track = app.seed_track(root, "track.bin", 1, None).await;
    let uri = track_uri(track);

    let mut held = Vec::new();
    for _ in 0..2 {
        let response = app
            .call(request(Method::GET, &uri, Some(TOKEN), None, &[]))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        held.push(response);
    }
    wait_in_flight(&app, 2).await;
    let response = app
        .call(request(Method::GET, &uri, Some(TOKEN), None, &[]))
        .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = body_bytes(response).await;
    assert!(String::from_utf8_lossy(&body).contains("RATE_LIMITED"));

    // 断开一路（响应体丢弃）后新请求应立即可用。
    drop(held.pop().unwrap());
    wait_in_flight(&app, 1).await;
    let response = app
        .call(request(Method::GET, &uri, Some(TOKEN), None, &[]))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_bytes(response).await.len(), 10000);
    // 上一响应消费完即释放另一路配额。
    wait_in_flight(&app, 1).await;
}

#[tokio::test]
async fn global_limit_rejects_excess_sessions_with_retry_after() {
    let mut app = App::build(2, 4, 1, Duration::from_secs(10));
    let user = app.seed_user().await;
    let root = app.seed_root("music", &app.root.clone()).await;
    let track = app.seed_track(root, "track.bin", 1, None).await;
    let uri = track_uri(track);
    let tokens: Vec<String> = (0..3u8).map(|i| format!("{:02x}", i).repeat(32)).collect();
    for token in &tokens {
        app.seed_session(user, token).await;
    }
    let mut held = Vec::new();
    for token in &tokens[..2] {
        let response = app
            .call(request(Method::GET, &uri, Some(token), None, &[]))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        held.push(response);
    }
    wait_in_flight(&app, 2).await;
    let response = app
        .call(request(Method::GET, &uri, Some(&tokens[2]), None, &[]))
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        header_text(&response, header::RETRY_AFTER).as_deref(),
        Some("1")
    );
    body_bytes(response).await;
    drop(held);
    wait_in_flight(&app, 0).await;
}

/// 文档验证口径：默认限额下，33 个会话最多 32 路持配额、超额 503；同一会话第 5 路 429；断开一路立即可用。
#[tokio::test]
async fn default_limits_match_documented_quota_matrix() {
    let mut app = App::build(32, 4, 1, Duration::from_secs(30));
    let user = app.seed_user().await;
    app.seed_session(user, TOKEN).await;
    let root = app.seed_root("music", &app.root.clone()).await;
    let track = app.seed_track(root, "track.bin", 1, None).await;
    let uri = track_uri(track);

    let mut session_held = Vec::new();
    for _ in 0..4 {
        let response = app
            .call(request(Method::GET, &uri, Some(TOKEN), None, &[]))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        session_held.push(response);
    }
    wait_in_flight(&app, 4).await;
    let response = app
        .call(request(Method::GET, &uri, Some(TOKEN), None, &[]))
        .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    body_bytes(response).await;
    drop(session_held.pop().unwrap());
    wait_in_flight(&app, 3).await;
    let response = app
        .call(request(Method::GET, &uri, Some(TOKEN), None, &[]))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    session_held.push(response);
    drop(session_held);
    wait_in_flight(&app, 0).await;

    // 33 个不同会话：至多 32 路持配额，第 33 路 503 + Retry-After；断开一路后立即可用。
    let tokens: Vec<String> = (0..33u8).map(|i| format!("{:02x}", i).repeat(32)).collect();
    for token in &tokens {
        app.seed_session(user, token).await;
    }
    let mut held = Vec::new();
    for token in &tokens[..32] {
        let response = app
            .call(request(Method::GET, &uri, Some(token), None, &[]))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        held.push(response);
    }
    wait_in_flight(&app, 32).await;
    let response = app
        .call(request(Method::GET, &uri, Some(&tokens[32]), None, &[]))
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        header_text(&response, header::RETRY_AFTER).as_deref(),
        Some("1")
    );
    body_bytes(response).await;
    drop(held.pop().unwrap());
    wait_in_flight(&app, 31).await;
    let response = app
        .call(request(Method::GET, &uri, Some(&tokens[32]), None, &[]))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_bytes(response).await.len(), 10000);
}

#[tokio::test]
async fn no_progress_timeout_releases_quota() {
    let mut app = App::build(32, 4, 1, Duration::from_millis(100));
    let user = app.seed_user().await;
    app.seed_session(user, TOKEN).await;
    let root = app.seed_root("music", &app.root.clone()).await;
    let track = app.seed_track(root, "track.bin", 1, None).await;
    let uri = track_uri(track);
    let held = app
        .call(request(Method::GET, &uri, Some(TOKEN), None, &[]))
        .await;
    wait_in_flight(&app, 1).await;
    // 响应体不被消费：100ms 无进展后流必须中止并归还配额。
    wait_in_flight(&app, 0).await;
    let response = app
        .call(request(Method::GET, &uri, Some(TOKEN), None, &[]))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_bytes(response).await.len(), 10000);
    drop(held);
}

#[tokio::test]
async fn chunked_reads_stitch_back_exact_bytes() {
    // 32 KiB 之外的奇数块大小也必须逐字节还原。
    let mut app = App::build(32, 4, 777, Duration::from_secs(30));
    let user = app.seed_user().await;
    app.seed_session(user, TOKEN).await;
    let root = app.seed_root("music", &app.root.clone()).await;
    let track = app
        .seed_track(root, "track.bin", 1, Some("audio/x-wav"))
        .await;
    let response = app
        .call(request(
            Method::GET,
            &track_uri(track),
            Some(TOKEN),
            Some("bytes=13-9999"),
            &[],
        ))
        .await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        header_text(&response, header::CONTENT_TYPE).as_deref(),
        Some("audio/x-wav")
    );
    let body = body_bytes(response).await;
    assert_eq!(body, &FIXTURE[13..10000]);
    wait_in_flight(&app, 0).await;
}

/// 轮询等待在途流数达到期望值（守卫的获取/释放发生在后台任务里）。
async fn wait_in_flight(app: &App, expected: usize) {
    for _ in 0..500 {
        if app.media.quota.in_flight() == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "等待在途流 {expected} 超时，当前 {}",
        app.media.quota.in_flight()
    );
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}
