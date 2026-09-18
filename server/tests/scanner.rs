//! T05 受限扫描契约测试：驱动真实 `http::router` 的 admin scans 端点、
//! 真实 `litebeat scan-worker` 子进程与真实 SQLite 写线程，覆盖文档规定的
//! 全量入库、增量、目录错误不清库、取消、幂等重放、并发 409、错误上限，
//! 以及 worker 超时/崩溃/越界的杀死重建路径。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use serde_json::Value as Json;
use tower::Service;

use litebeat::auth::{SESSION_COOKIE, hash_token, session::encode_hex};
use litebeat::db::{Db, DbOptions, Value};
use litebeat::http::{AppState, router};
use litebeat::media::MediaEnv;
use litebeat::media::body_guard::MediaQuota;
use litebeat::scanner::ScanConfig;
use litebeat::scanner::ScanEnv;
use litebeat::scanner::protocol::DebugDirective;
use litebeat::scanner::worker::{ParseFailure, ParseWorker, WorkerConfig};

const HOST: &str = "localhost:8090";
const TOKEN: &str = "d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0";
const SCANS: &str = "/api/v1/admin/scans";
const FIXTURE_BIN: &[u8] = include_bytes!("../../fixtures/media/range_10000.bin");

/// 真实被测子进程：集成测试里不能用 current_exe（那是测试二进制）。
fn worker_program() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_litebeat"))
}

fn fixture_dir(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../fixtures/scanner")
        .join(rel)
}

/// 已存在则覆盖地递归复制目录树。
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

struct App {
    router: Router,
    db: Arc<Db>,
    root: PathBuf,
    music: PathBuf,
    scans: Arc<ScanEnv>,
    _dir: tempfile::TempDir,
}

impl App {
    fn build(config: ScanConfig) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<html></html>").unwrap();
        let root = dir.path().join("scan");
        std::fs::create_dir_all(&root).unwrap();
        copy_tree(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../fixtures/scanner")
                .as_path(),
            &root,
        );
        // 媒体库根与扫描根分开，避免全量扫描把测试用的播放曲目判为不可用。
        let music = dir.path().join("music");
        std::fs::create_dir_all(&music).unwrap();
        std::fs::write(music.join("track.bin"), FIXTURE_BIN).unwrap();

        let db = Db::open(dir.path().join("litebeat.db"), DbOptions::default()).unwrap();
        let media = Arc::new(MediaEnv {
            quota: Arc::new(MediaQuota::new(32, 4)),
            chunk_bytes: 32 * 1024,
            no_progress_timeout: Duration::from_secs(30),
            allowed_roots: vec![std::fs::canonicalize(&music).unwrap()],
        });
        let scans = ScanEnv::new(config);
        let router = router(AppState {
            web_dir: dir.path().to_path_buf(),
            ready: Arc::new(AtomicBool::new(true)),
            db: Some(Arc::clone(&db)),
            media,
            scans: Arc::clone(&scans),
        });
        Self {
            router,
            db,
            root,
            music,
            scans,
            _dir: dir,
        }
    }

    async fn call(&mut self, request: Request<Body>) -> Response {
        self.router.call(request).await.unwrap()
    }

    async fn seed_admin(&self) -> i64 {
        self.db
            .execute(
                "INSERT INTO users(username, password_hash) VALUES ('admin', 'x')",
                vec![],
            )
            .await
            .unwrap()
            .last_rowid
    }

    async fn seed_session(&self, user_id: i64) {
        self.db
            .execute(
                "INSERT INTO sessions(token_hash, user_id, csrf_token, expires_at)
                 VALUES (?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+1 day'))",
                vec![
                    Value::Blob(hash_token(TOKEN)),
                    Value::Integer(user_id),
                    Value::Blob(hash_token(&format!("csrf-{TOKEN}"))),
                ],
            )
            .await
            .unwrap();
    }

    async fn seed_root(&self, name: &str, path: &Path) -> i64 {
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

    /// 登录 + 建根 + 建曲目的完整准备，返回 (App, root_id)。
    async fn seeded(config: ScanConfig) -> (Self, i64) {
        let app = Self::build(config);
        let user = app.seed_admin().await;
        app.seed_session(user).await;
        let root = app.seed_root("scan", &app.root.clone()).await;
        (app, root)
    }
}

/// 默认测试配置：真实二进制、开 debug、可控延迟与目录错误注入。
fn test_config(
    per_file_delay: Option<Duration>,
    inject_dir_error_after_files: Option<u64>,
) -> ScanConfig {
    ScanConfig {
        worker: WorkerConfig {
            program: worker_program(),
            memory_limit_bytes: 96 * 1024 * 1024,
            debug: true,
        },
        parse_timeout: Duration::from_secs(5),
        per_file_delay,
        inject_dir_error_after_files,
    }
}

fn json_body(value: Json) -> Body {
    Body::from(serde_json::to_vec(&value).unwrap())
}

fn create_scan_request(
    root_id: i64,
    force: bool,
    key: &str,
    with_auth: bool,
    with_csrf: bool,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(SCANS)
        .header("host", HOST)
        .header(header::CONTENT_TYPE, "application/json");
    if with_auth {
        builder = builder.header(header::COOKIE, format!("{SESSION_COOKIE}={TOKEN}"));
    }
    if with_csrf {
        builder = builder.header(
            "x-csrf-token",
            encode_hex(&hash_token(&format!("csrf-{TOKEN}"))),
        );
    }
    if !key.is_empty() {
        builder = builder.header("idempotency-key", key);
    }
    builder
        .body(json_body(serde_json::json!({
            "root_id": root_id.to_string(),
            "force": force,
        })))
        .unwrap()
}

fn get_request(uri: &str, with_auth: bool) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("host", HOST);
    if with_auth {
        builder = builder.header(header::COOKIE, format!("{SESSION_COOKIE}={TOKEN}"));
    }
    builder.body(Body::empty()).unwrap()
}

fn post_empty(uri: &str, with_auth: bool, with_csrf: bool) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("host", HOST);
    if with_auth {
        builder = builder.header(header::COOKIE, format!("{SESSION_COOKIE}={TOKEN}"));
    }
    if with_csrf {
        builder = builder.header(
            "x-csrf-token",
            encode_hex(&hash_token(&format!("csrf-{TOKEN}"))),
        );
    }
    builder.body(Body::empty()).unwrap()
}

async fn body_json(response: Response) -> Json {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn error_code(response: Response) -> String {
    let json = body_json(response).await;
    json["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

impl App {
    async fn job_field(&self, job_id: i64, field: &str) -> Option<Json> {
        let rows = self
            .db
            .query_rows(
                format!("SELECT {field} FROM scan_jobs WHERE id = ?").as_str(),
                vec![Value::Integer(job_id)],
            )
            .await
            .unwrap();
        rows.into_iter().next().and_then(|mut row| {
            let value = row.pop()?;
            Some(match value {
                Value::Text(t) => Json::String(t),
                Value::Integer(i) => Json::Number(i.into()),
                Value::Null => Json::Null,
                _ => Json::Null,
            })
        })
    }

    async fn job_state(&self, job_id: i64) -> String {
        self.job_field(job_id, "state")
            .await
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// 轮询直到任务进入终态，超时 panic 带当前状态。
    async fn wait_terminal(&self, job_id: i64) -> String {
        for _ in 0..1_000 {
            let state = self.job_state(job_id).await;
            if matches!(
                state.as_str(),
                "completed" | "failed" | "cancelled" | "interrupted"
            ) {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "等待扫描 {job_id} 结束超时，最后状态 {}",
            self.job_state(job_id).await
        );
    }

    async fn job_i64(&self, job_id: i64, field: &str) -> i64 {
        self.job_field(job_id, field)
            .await
            .and_then(|v| v.as_i64())
            .unwrap_or(-1)
    }

    async fn create_ok(&mut self, root_id: i64, force: bool, key: &str) -> i64 {
        let response = self
            .call(create_scan_request(root_id, force, key, true, true))
            .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED, "创建扫描应 202");
        let json = body_json(response).await;
        json["job_id"].as_str().unwrap().parse().unwrap()
    }

    async fn count(&self, sql: &str) -> i64 {
        self.db
            .query_rows(sql, vec![])
            .await
            .unwrap()
            .into_iter()
            .next()
            .and_then(|mut row| row.pop())
            .and_then(|v| v.as_i64())
            .unwrap()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_scan_ingests_valid_tracks_and_records_failures() {
    let (mut app, root) = App::seeded(test_config(None, None)).await;
    let job = app.create_ok(root, false, "k-full").await;
    assert_eq!(app.wait_terminal(job).await, "completed");

    // 有效音频入库：no_tags / huge_cover / 中文各一条，损坏文件只记错误。
    let valid = app
        .db
        .query_rows(
            "SELECT relative_path FROM tracks WHERE root_id = ? AND available = 1 ORDER BY relative_path",
            vec![Value::Integer(root)],
        )
        .await
        .unwrap();
    let paths: Vec<String> = valid
        .into_iter()
        .map(|row| row[0].as_text().unwrap_or_default().to_owned())
        .collect();
    assert!(
        paths.iter().any(|p| p.ends_with("no_tags.mp3")),
        "{paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("huge_cover.mp3")),
        "{paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.contains("中文歌曲.mp3")),
        "中文路径应入库: {paths:?}"
    );

    // 损坏/非法音频记入 scan_errors，且不写 tracks。
    let errors = app
        .db
        .query_rows(
            "SELECT relative_path, code FROM scan_errors WHERE job_id = ? ORDER BY relative_path",
            vec![Value::Integer(job)],
        )
        .await
        .unwrap();
    let codes: Vec<(String, String)> = errors
        .into_iter()
        .map(|row| {
            (
                row[0].as_text().unwrap_or_default().to_owned(),
                row[1].as_text().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    assert!(
        codes
            .iter()
            .any(|(p, c)| p == "corrupt_tag.mp3" && c == "PARSE_FAILED"),
        "corrupt_tag 应有 PARSE_FAILED: {codes:?}"
    );
    assert!(
        codes
            .iter()
            .any(|(p, c)| p == "broken_audio.mp3" && c == "PARSE_FAILED"),
        "broken_audio 应有 PARSE_FAILED: {codes:?}"
    );

    // 中文标签完整保留，专辑去重生成 albums 行。
    let cn = app
        .db
        .query_rows(
            "SELECT title, artist, album_title FROM tracks WHERE root_id = ? AND relative_path LIKE '%中文歌曲.mp3'",
            vec![Value::Integer(root)],
        )
        .await
        .unwrap();
    assert_eq!(cn[0][0].as_text(), Some("中文歌曲标题"));
    assert_eq!(cn[0][1].as_text(), Some("中文歌手"));
    assert_eq!(cn[0][2].as_text(), Some("中文专辑名"));
    assert!(app.count("SELECT COUNT(*) FROM albums").await >= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_scan_is_incremental_and_no_new_rows() {
    let (mut app, root) = App::seeded(test_config(None, None)).await;
    let first = app.create_ok(root, false, "k1").await;
    assert_eq!(app.wait_terminal(first).await, "completed");
    let tracks_after_first = app
        .count("SELECT COUNT(*) FROM tracks WHERE available = 1")
        .await;

    // 再扫一次：文件未变，updated 应为 0，条目总数不增。
    let second = app.create_ok(root, false, "k2").await;
    assert_eq!(app.wait_terminal(second).await, "completed");
    assert_eq!(
        app.job_i64(second, "updated").await,
        0,
        "未变文件不应重复入库"
    );
    let tracks_after_second = app
        .count("SELECT COUNT(*) FROM tracks WHERE available = 1")
        .await;
    assert_eq!(
        tracks_after_first, tracks_after_second,
        "重复扫描不应新增条目"
    );

    // 改一首（追加字节改变大小）：只更新对应条目。
    let target = app.root.join("no_tags.mp3");
    let mut data = std::fs::read(&target).unwrap();
    data.extend_from_slice(&[0u8; 417]);
    std::fs::write(&target, &data).unwrap();
    let third = app.create_ok(root, false, "k3").await;
    assert_eq!(app.wait_terminal(third).await, "completed");
    assert_eq!(app.job_i64(third, "updated").await, 1, "仅改动文件应被更新");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_walk_marks_missing_unavailable_keeps_row() {
    let (mut app, root) = App::seeded(test_config(None, None)).await;
    // 预置一条磁盘上不存在的可用曲目（模拟上次扫描的遗留）。
    let ghost = app
        .db
        .execute(
            "INSERT INTO tracks(root_id, relative_path, title, size_bytes, mtime_ns,
                                sort_title, sort_artist, sort_album, available)
             VALUES (?, 'ghost.mp3', 'G', 10, 10, 'g', '', '', 1)",
            vec![Value::Integer(root)],
        )
        .await
        .unwrap()
        .last_rowid;

    let job = app.create_ok(root, false, "k-ghost").await;
    assert_eq!(app.wait_terminal(job).await, "completed");

    let row = app
        .db
        .query_rows(
            "SELECT available, id FROM tracks WHERE id = ?",
            vec![Value::Integer(ghost)],
        )
        .await
        .unwrap();
    // 完整遍历后幽灵条目判不可用，但行与 id 保留（收藏外键不悬空）。
    assert_eq!(row[0][0].as_i64(), Some(0), "完整扫描应把未见条目置不可用");
    assert_eq!(row[0][1].as_i64(), Some(ghost));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_walk_does_not_clear_availability() {
    // 成功扫描一次让 ghost 保留，然后用注入的目录错误让第二次扫描失败。
    let (mut app, root) = App::seeded(test_config(None, None)).await;
    let ghost = app
        .db
        .execute(
            "INSERT INTO tracks(root_id, relative_path, title, size_bytes, mtime_ns,
                                sort_title, sort_artist, sort_album, available)
             VALUES (?, 'ghost.mp3', 'G', 10, 10, 'g', '', '', 1)",
            vec![Value::Integer(root)],
        )
        .await
        .unwrap()
        .last_rowid;

    // 注入：遍历出第 1 个文件后模拟根目录掉线 → 任务 failed，且不执行标记。
    app.scans
        .set_config(|c| c.inject_dir_error_after_files = Some(1));
    let job = app.create_ok(root, false, "k-fail").await;
    assert_eq!(
        app.wait_terminal(job).await,
        "failed",
        "目录错误应让任务 failed"
    );
    let available = app
        .db
        .query_rows(
            "SELECT available FROM tracks WHERE id = ?",
            vec![Value::Integer(ghost)],
        )
        .await
        .unwrap();
    assert_eq!(
        available[0][0].as_i64(),
        Some(1),
        "遍历不完整时绝不能把既有曲目判不可用"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_stops_running_scan_without_marking_unavailable() {
    // 每文件延迟 200ms，制造可取消的慢扫描。
    let (mut app, root) = App::seeded(test_config(Some(Duration::from_millis(200)), None)).await;
    let ghost = app
        .db
        .execute(
            "INSERT INTO tracks(root_id, relative_path, title, size_bytes, mtime_ns,
                                sort_title, sort_artist, sort_album, available)
             VALUES (?, 'ghost.mp3', 'G', 10, 10, 'g', '', '', 1)",
            vec![Value::Integer(root)],
        )
        .await
        .unwrap()
        .last_rowid;
    let job = app.create_ok(root, false, "k-cancel").await;
    // 等它进入 running 再取消。
    for _ in 0..200 {
        if app.job_state(job).await == "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let response = app
        .call(post_empty(&format!("{SCANS}/{job}/cancel"), true, true))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["cancelling"].as_bool(), Some(true));

    assert_eq!(app.wait_terminal(job).await, "cancelled");
    let available = app
        .db
        .query_rows(
            "SELECT available FROM tracks WHERE id = ?",
            vec![Value::Integer(ghost)],
        )
        .await
        .unwrap();
    assert_eq!(available[0][0].as_i64(), Some(1), "取消不得清可用性");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_requires_auth_csrf_and_valid_body() {
    let (mut app, root) = App::seeded(test_config(None, None)).await;

    let response = app
        .call(create_scan_request(root, false, "kx", false, false))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(response).await, "UNAUTHORIZED");

    let response = app
        .call(create_scan_request(root, false, "kx", true, false))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(error_code(response).await, "CSRF_FAILED");

    // 缺少 Idempotency-Key。
    let response = app
        .call(create_scan_request(root, false, "", true, true))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(response).await, "INVALID_REQUEST");

    // 非法 root_id（字符串）→ 400。
    let bad = app
        .call(
            Request::builder()
                .method(Method::POST)
                .uri(SCANS)
                .header("host", HOST)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, format!("{SESSION_COOKIE}={TOKEN}"))
                .header(
                    "x-csrf-token",
                    encode_hex(&hash_token(&format!("csrf-{TOKEN}"))),
                )
                .header("idempotency-key", "kbad")
                .body(json_body(serde_json::json!({"root_id": "abc"})))
                .unwrap(),
        )
        .await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    // 不存在的 root → 404。
    let missing = app
        .call(create_scan_request(999_999, false, "km", true, true))
        .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(error_code(missing).await, "NOT_FOUND");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idempotency_replays_and_conflicts() {
    let (mut app, root) = App::seeded(test_config(None, None)).await;
    // 首次创建并用同一 key 重放：返回同一 job_id，不占新名额。
    let job = app.create_ok(root, false, "same-key").await;
    let replay = app
        .call(create_scan_request(root, false, "same-key", true, true))
        .await;
    assert_eq!(replay.status(), StatusCode::ACCEPTED, "重放应回放 202");
    let replay_json = body_json(replay).await;
    assert_eq!(
        replay_json["job_id"].as_str(),
        Some(job.to_string().as_str())
    );

    // 同 key、不同载荷（force 变了）→ 409 IDEMPOTENCY_CONFLICT。
    let conflict = app
        .call(create_scan_request(root, true, "same-key", true, true))
        .await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(conflict).await, "IDEMPOTENCY_CONFLICT");

    app.wait_terminal(job).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_scan_while_running_conflicts() {
    // 慢扫描占住在途名额，另一请求（不同 key）应 409 SCAN_RUNNING。
    let (mut app, root) = App::seeded(test_config(Some(Duration::from_millis(200)), None)).await;
    let job = app.create_ok(root, false, "first").await;
    for _ in 0..200 {
        if app.job_state(job).await != "queued" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let second = app
        .call(create_scan_request(root, false, "second", true, true))
        .await;
    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(second).await, "SCAN_RUNNING");
    app.wait_terminal(job).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_endpoint_reports_progress_or_404() {
    let (mut app, root) = App::seeded(test_config(None, None)).await;
    let job = app.create_ok(root, false, "k-status").await;
    assert_eq!(app.wait_terminal(job).await, "completed");
    let response = app.call(get_request(&format!("{SCANS}/{job}"), true)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["id"].as_str(), Some(job.to_string().as_str()));
    assert_eq!(json["state"].as_str(), Some("completed"));
    assert!(
        json["isolation"].as_str().unwrap().contains("job")
            || json["isolation"].as_str().unwrap().contains("cgroup")
            || json["isolation"].as_str().unwrap().contains("unavailable")
    );

    let missing = app
        .call(get_request(&format!("{SCANS}/424242"), true))
        .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let unauth = app
        .call(get_request(&format!("{SCANS}/{job}"), false))
        .await;
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_proceeds_during_scan() {
    // 文档验证：扫描进行中，5 路播放仍可继续（读线程不被单一写任务饿死）。
    let (app, root) = App::seeded(test_config(Some(Duration::from_millis(120)), None)).await;
    // 复用共享媒体库根，预置一条可播放曲目。
    let media_root = app.seed_root("music", &app.music.clone()).await;
    let track = app
        .db
        .execute(
            "INSERT INTO tracks(root_id, relative_path, title, size_bytes, mtime_ns,
                                sort_title, sort_artist, sort_album, available, mime)
             VALUES (?, 'track.bin', 'T', 10000, 1, 't', '', '', 1, 'audio/mpeg')",
            vec![Value::Integer(media_root)],
        )
        .await
        .unwrap()
        .last_rowid;
    let mut app = app;
    let job = app.create_ok(root, false, "k-conc").await;
    for _ in 0..200 {
        if app.job_state(job).await == "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // 扫描尚未结束（仍有每文件延迟），此刻发起 5 路 Range 播放，均应成功。
    let mut held = Vec::new();
    for part in 0..5u64 {
        let range = format!("bytes={}-{}", part * 1000, part * 1000 + 999);
        let response = app
            .call(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/media/tracks/{track}"))
                    .header("host", HOST)
                    .header(header::COOKIE, format!("{SESSION_COOKIE}={TOKEN}"))
                    .header(header::RANGE, range)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::PARTIAL_CONTENT,
            "扫描中第 {part} 路播放应 206"
        );
        held.push(response);
    }
    for (part, response) in held.into_iter().enumerate() {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(bytes.len(), 1000, "第 {part} 路应收到 1000 字节");
    }
    app.wait_terminal(job).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scan_errors_capped_but_failed_counts_all() {
    use litebeat::scanner::protocol::MAX_ERROR_DETAIL_ROWS;
    let (app, _scan_root) = App::seeded(test_config(None, None)).await;
    // 单独一个含大量损坏文件的根，与默认夹具根分开，避免相互干扰。
    let broken = app._dir.path().join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    let total = (MAX_ERROR_DETAIL_ROWS + 250) as u64;
    for i in 0..total {
        std::fs::write(broken.join(format!("bad{i}.mp3")), b"definitely not audio").unwrap();
    }
    let broken_root = app.seed_root("broken", &broken).await;
    let mut app = app;
    let job = app.create_ok(broken_root, false, "k-cap").await;
    assert_eq!(app.wait_terminal(job).await, "completed");
    let stored = app
        .count(&format!(
            "SELECT COUNT(*) FROM scan_errors WHERE job_id = {job}"
        ))
        .await;
    assert_eq!(
        stored as usize, MAX_ERROR_DETAIL_ROWS,
        "错误明细应恰好封顶在 {MAX_ERROR_DETAIL_ROWS} 行"
    );
    assert_eq!(
        app.job_i64(job, "failed").await as usize,
        total as usize,
        "failed 计数必须涵盖全部失败，不受明细上限影响"
    );
}

#[tokio::test]
async fn worker_timeout_and_protocol_and_crash_rebuild() {
    let cfg = WorkerConfig {
        program: worker_program(),
        memory_limit_bytes: 96 * 1024 * 1024,
        debug: true,
    };
    let no_tags = fixture_dir("no_tags.mp3");

    // 超时：让子进程睡 5s，协调端 300ms 判定超时并杀死。
    let mut worker = ParseWorker::spawn(&cfg).await.unwrap();
    let slow = worker
        .parse_with_debug(
            &no_tags,
            Duration::from_millis(300),
            Some(DebugDirective::SleepMs(5000)),
        )
        .await;
    assert!(
        matches!(slow, Err(ParseFailure::Timeout)),
        "应判定超时: {slow:?}"
    );
    // 旧进程已被杀；重建后应能正常解析同一文件。
    drop(worker);
    let mut worker2 = ParseWorker::spawn(&cfg).await.unwrap();
    let ok = worker2.parse(&no_tags, Duration::from_secs(5)).await;
    assert!(ok.is_ok(), "重建后应恢复解析: {:?}", ok.map(|r| r.code));
    worker2.shutdown().await;

    // 协议越界：子进程写 >32KiB 行 → Protocol，进程被杀。
    let mut w3 = ParseWorker::spawn(&cfg).await.unwrap();
    let huge = w3
        .parse_with_debug(
            &no_tags,
            Duration::from_secs(5),
            Some(DebugDirective::HugeOutput),
        )
        .await;
    assert!(
        matches!(huge, Err(ParseFailure::Protocol)),
        "越界输出应判协议违规: {huge:?}"
    );
    drop(w3);

    // 崩溃：子进程 exit(7) → Crashed。
    let mut w4 = ParseWorker::spawn(&cfg).await.unwrap();
    let crash = w4
        .parse_with_debug(
            &no_tags,
            Duration::from_secs(5),
            Some(DebugDirective::Crash),
        )
        .await;
    assert!(
        matches!(crash, Err(ParseFailure::Crashed)),
        "子进程退出应判崩溃: {crash:?}"
    );
    drop(w4);

    // 崩溃后重建仍可服务。
    let mut w5 = ParseWorker::spawn(&cfg).await.unwrap();
    assert!(w5.parse(&no_tags, Duration::from_secs(5)).await.is_ok());
    w5.shutdown().await;
}

#[tokio::test]
async fn startup_marks_running_as_interrupted() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("litebeat.db"), DbOptions::default()).unwrap();
    let root = db
        .execute(
            "INSERT INTO library_roots(name, canonical_path) VALUES ('r', '/tmp/r')",
            vec![],
        )
        .await
        .unwrap()
        .last_rowid;
    for state in ["queued", "running"] {
        db.execute(
            "INSERT INTO scan_jobs(root_id, state, force) VALUES (?, ?, 0)",
            vec![Value::Integer(root), Value::Text(state.into())],
        )
        .await
        .unwrap();
    }
    db.execute(
        "INSERT INTO scan_jobs(root_id, state, force) VALUES (?, 'completed', 0)",
        vec![Value::Integer(root)],
    )
    .await
    .unwrap();

    litebeat::scanner::mark_interrupted(&db).await.unwrap();

    let interrupted = db
        .query_rows(
            "SELECT COUNT(*) FROM scan_jobs WHERE state = 'interrupted'",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(interrupted[0][0].as_i64(), Some(2));
    let still_completed = db
        .query_rows(
            "SELECT COUNT(*) FROM scan_jobs WHERE state = 'completed'",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(still_completed[0][0].as_i64(), Some(1));
}
