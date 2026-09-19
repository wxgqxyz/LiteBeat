//! T06 曲库检索契约测试：真实 router + 真实 SQLite（含 FTS 触发器），
//! 覆盖游标分页稳定性、过滤器串用拒绝、中文/特殊字符搜索语义、
//! queue-ids 上限、以及 1 万行规模下的 EXPLAIN QUERY PLAN 索引验证。

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use serde_json::Value as Json;
use tower::Service;

use litebeat::auth::SESSION_COOKIE;
use litebeat::db::{Db, DbOptions, Reply, Value};
use litebeat::http::{AppState, router};
use litebeat::library::normalize::normalize;
use litebeat::media::MediaEnv;
use litebeat::scanner::ScanEnv;

const HOST: &str = "localhost:8090";
const TOKEN: &str = "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";

struct App {
    router: Router,
    db: Arc<Db>,
    root_id: i64,
    _dir: tempfile::TempDir,
}

/// 全局递增，保证跨多次 seed_tracks 的 relative_path 唯一。
static SEED_INDEX: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl App {
    async fn seeded() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<html></html>").unwrap();
        let db = Db::open(dir.path().join("litebeat.db"), DbOptions::default()).unwrap();
        db.execute(
            "INSERT INTO users(username, password_hash) VALUES ('admin', 'x')",
            vec![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO sessions(token_hash, user_id, csrf_token, expires_at)
             VALUES (?, 1, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+1 day'))",
            vec![
                Value::Blob(litebeat::auth::hash_token(TOKEN)),
                Value::Blob(litebeat::auth::hash_token(&format!("csrf-{TOKEN}"))),
            ],
        )
        .await
        .unwrap();
        let root_id = db
            .execute(
                "INSERT INTO library_roots(name, canonical_path) VALUES ('r', '/r')",
                vec![],
            )
            .await
            .unwrap()
            .last_rowid;
        let router = router(AppState {
            web_dir: dir.path().to_path_buf(),
            ready: Arc::new(AtomicBool::new(true)),
            db: Some(Arc::clone(&db)),
            media: Arc::new(MediaEnv::default()),
            scans: Arc::new(ScanEnv::default()),
        });
        Self {
            router,
            db,
            root_id,
            _dir: dir,
        }
    }

    async fn call(&mut self, uri: &str) -> Response {
        self.router
            .call(
                Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .header(header::HOST, HOST)
                    .header(header::COOKIE, format!("{SESSION_COOKIE}={TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn call_anonymous(&mut self, uri: &str) -> Response {
        self.router
            .call(
                Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .header(header::HOST, HOST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// 沿游标走完一个列表端点，返回按页顺序收集的 id；不收敛即失败。
    async fn walk(&mut self, first_uri: &str) -> Vec<String> {
        let mut uri = first_uri.to_owned();
        let mut ids: Vec<String> = Vec::new();
        for _ in 0..400 {
            let json = json_of(self.call(&uri).await).await;
            assert!(json["error"].is_null(), "翻页收到错误响应：{json}");
            let items = json["items"]
                .as_array()
                .unwrap_or_else(|| panic!("响应缺 items：{json}"));
            ids.extend(
                items
                    .iter()
                    .map(|item| item["id"].as_str().unwrap().to_owned()),
            );
            if !json["has_more"].as_bool().unwrap() {
                assert!(json["next_cursor"].is_null(), "末页不应带游标：{json}");
                return ids;
            }
            let cursor = json["next_cursor"]
                .as_str()
                .unwrap_or_else(|| panic!("has_more 却缺游标：{json}"));
            uri = format!("{first_uri}&cursor={cursor}");
        }
        panic!("翻页 400 页仍未收敛，游标可能没有推进");
    }

    /// 种子曲目：(title, artist, album_title, disc, track)，排序键走同一规范化函数。
    async fn seed_tracks(&self, tracks: &[(&str, &str, &str, i64, i64)]) -> Vec<i64> {
        let mut ids = Vec::new();
        for (title, artist, album, disc, track) in tracks.iter() {
            let index = SEED_INDEX.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let album_id = if album.is_empty() {
                Value::Null
            } else {
                let key = format!("dir-{album}");
                let existing = self
                    .db
                    .query_rows(
                        "SELECT id FROM albums WHERE root_id = ?1 AND directory_key = ?2",
                        vec![Value::Integer(self.root_id), Value::Text(key.clone())],
                    )
                    .await
                    .unwrap();
                match existing.first() {
                    Some(row) => Value::Integer(row[0].as_i64().unwrap()),
                    None => Value::Integer(
                        self.db
                            .execute(
                                "INSERT INTO albums(root_id, directory_key, title, album_artist, sort_title)
                                 VALUES (?, ?, ?, ?, ?)",
                                vec![
                                    Value::Integer(self.root_id),
                                    Value::Text(key),
                                    Value::Text(album.to_string()),
                                    Value::Text(artist.to_string()),
                                    Value::Text(normalize(album)),
                                ],
                            )
                            .await
                            .unwrap()
                            .last_rowid,
                    ),
                }
            };
            let reply = self
                .db
                .execute(
                    "INSERT INTO tracks(root_id, relative_path, title, artist, album_id,
                        album_title, duration_ms, codec, mime, size_bytes, mtime_ns,
                        sort_title, sort_artist, sort_album, disc_no, track_no)
                     VALUES (?, ?, ?, ?, ?, ?, 1000, 'mp3', 'audio/mpeg', 100, 1, ?, ?, ?, ?, ?)",
                    vec![
                        Value::Integer(self.root_id),
                        Value::Text(format!("t{index}.mp3")),
                        Value::Text(title.to_string()),
                        Value::Text(artist.to_string()),
                        album_id,
                        Value::Text(album.to_string()),
                        Value::Text(normalize(title)),
                        Value::Text(normalize(artist)),
                        Value::Text(normalize(album)),
                        Value::Integer(*disc),
                        Value::Integer(*track),
                    ],
                )
                .await
                .unwrap();
            ids.push(reply.last_rowid);
        }
        ids
    }

    /// 大批量种子：单事务插入 n 行，供上限与查询计划测试。
    async fn bulk_seed(&self, n: i64) {
        let db = Arc::clone(&self.db);
        let root_id = self.root_id;
        let rx = db
            .submit_write((n as usize) * 256, move |conn| {
                let tx = conn.unchecked_transaction()?;
                {
                    let mut stmt = tx.prepare_cached(
                        "INSERT INTO tracks(root_id, relative_path, title, artist,
                                album_title, size_bytes, mtime_ns,
                                sort_title, sort_artist, sort_album)
                             VALUES (?1, ?2, ?3, ?4, '', 100, 1, ?3, ?4, '')",
                    )?;
                    for i in 0..n {
                        let title = format!("bulk 曲目 {i:06}");
                        let artist = format!("bulk-artist-{}", i % 97);
                        stmt.execute(rusqlite::params![
                            root_id,
                            format!("bulk{i}.mp3"),
                            normalize(&title),
                            normalize(&artist),
                        ])?;
                    }
                }
                let affected = tx.changes();
                tx.commit()?;
                Ok(Reply {
                    rows: None,
                    affected,
                    last_rowid: 0,
                })
            })
            .unwrap();
        rx.await.unwrap().unwrap();
    }
}

async fn json_of(response: Response) -> Json {
    let body = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

async fn error_code(response: Response) -> String {
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    serde_json::from_slice::<Json>(&body).unwrap()["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn titles(json: &Json) -> Vec<String> {
    json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["title"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn library_endpoints_require_auth() {
    let mut app = App::seeded().await;
    for uri in [
        "/api/v1/tracks",
        "/api/v1/tracks/1",
        "/api/v1/albums",
        "/api/v1/albums/1/tracks",
        "/api/v1/search?q=x",
        "/api/v1/queue-ids?scope=library",
    ] {
        let response = app.call_anonymous(uri).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(error_code(response).await, "UNAUTHORIZED");
    }
}

#[tokio::test]
async fn pagination_with_duplicate_titles_is_stable_and_complete() {
    let mut app = App::seeded().await;
    // 同名 5 首，游标翻页必须不重不漏、按 (sort_title,id) 稳定推进。
    let ids = app
        .seed_tracks(&[
            ("晴天", "A", "", 0, 0),
            ("晴天", "B", "", 0, 0),
            ("晴天", "C", "", 0, 0),
            ("晴天", "D", "", 0, 0),
            ("晴天", "E", "", 0, 0),
        ])
        .await;
    let first = json_of(app.call("/api/v1/tracks?limit=2").await).await;
    assert_eq!(first["items"].as_array().unwrap().len(), 2, "不得超页");
    assert!(first["has_more"].as_bool().unwrap());
    let cursor = first["next_cursor"].as_str().unwrap();
    assert!(cursor.len() <= 512, "游标必须不超 512 字节");

    let walked = app.walk("/api/v1/tracks?limit=2").await;
    // 同名时以 id 为次级键，遍历结果必须恰是这 5 个 id 的稳定序。
    let mut expected: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
    expected.sort();
    assert_eq!(walked.len(), 5, "翻页必须不重不漏：{walked:?}");
    let mut unique = walked.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique, expected, "翻页内容与种子集合不一致：{walked:?}");
}

#[tokio::test]
async fn invalid_limit_sort_and_cursor_rejected() {
    let mut app = App::seeded().await;
    app.seed_tracks(&[
        ("a", "x", "", 0, 0),
        ("b", "y", "", 0, 0),
        ("c", "x", "", 0, 0),
    ])
    .await;
    for uri in [
        "/api/v1/tracks?limit=0",
        "/api/v1/tracks?limit=101",
        "/api/v1/tracks?limit=abc",
        "/api/v1/tracks?sort=bogus",
        "/api/v1/tracks?cursor=%21%21notbase64%21%21",
    ] {
        let response = app.call(uri).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        let code = error_code(response).await;
        assert!(
            code == "INVALID_REQUEST" || code == "INVALID_CURSOR",
            "{uri} -> {code}"
        );
    }
    // 游标绑定的过滤器变了 → 400，绝不静默换过滤器继续翻页。
    let first = json_of(app.call("/api/v1/tracks?limit=1&artist=x").await).await;
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    let response = app
        .call(&format!("/api/v1/tracks?limit=1&artist=y&cursor={cursor}"))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(response).await, "INVALID_CURSOR");
}

#[tokio::test]
async fn artist_filter_uses_normalized_comparison() {
    let mut app = App::seeded().await;
    app.seed_tracks(&[("夜曲", "周杰伦", "", 0, 0), ("倔强", "五月天", "", 0, 0)])
        .await;
    let json = json_of(
        app.call("/api/v1/tracks?artist=%E5%91%A8%E6%9D%B0%E4%BC%A6")
            .await,
    )
    .await;
    assert_eq!(titles(&json), vec!["夜曲"]);
    // 大小写与多余空白都先规范化再比较。
    let json = json_of(app.call("/api/v1/tracks?artist=JAY%20%20CHOU").await).await;
    assert_eq!(titles(&json), Vec::<String>::new());
    app.seed_tracks(&[("we are", "JAY  CHOU", "", 0, 0)]).await;
    let json = json_of(app.call("/api/v1/tracks?artist=jay%20chou").await).await;
    assert_eq!(titles(&json), vec!["we are"]);
}

#[tokio::test]
async fn search_samples_cover_chinese_case_fullwidth_and_metachars() {
    let mut app = App::seeded().await;
    app.seed_tracks(&[
        ("周杰伦的床边故事", "周杰伦", "", 0, 0),
        ("双截棍", "周杰伦", "", 0, 0),
        ("花心", "周华健", "", 0, 0),
        ("we will rock you", "queen", "", 0, 0),
        ("jay zhou live", "", "", 0, 0),
        ("50% off", "", "", 0, 0),
        ("_hidden track", "", "", 0, 0),
        ("ahill", "", "", 0, 0),
        ("他说“周杰”", "", "", 0, 0),
        ("不为谁而作的歌", "", "", 0, 0),
    ])
    .await;

    // 1–2 字符走前缀：标题/歌手/专辑任一列以“周”开头。
    let json = json_of(app.call("/api/v1/search?q=%E5%91%A8").await).await; // 周
    assert_eq!(
        titles(&json),
        vec!["双截棍", "周杰伦的床边故事", "花心"],
        "前缀按 (sort_title,id) 归并且 title/artist 命中去重"
    );
    // “周杰”两字前缀：标题或歌手任一起头即命中，按 (sort_title,id) 排。
    let json = json_of(app.call("/api/v1/search?q=%E5%91%A8%E6%9D%B0").await).await;
    assert_eq!(titles(&json), vec!["双截棍", "周杰伦的床边故事"]);

    // 3 字及以上走 FTS trigram：子串语义（命中三列任一）。
    let json = json_of(
        app.call("/api/v1/search?q=%E5%91%A8%E6%9D%B0%E4%BC%A6")
            .await,
    )
    .await; // 周杰伦
    assert_eq!(titles(&json), vec!["双截棍", "周杰伦的床边故事"]);
    // 双字前缀查不到“他说“周杰””（不是前缀），三字 trigram 子串能查到引号内容。
    let json = json_of(
        app.call("/api/v1/search?q=%E4%BB%96%E8%AF%B4%E2%80%9C%E5%91%A8")
            .await,
    )
    .await; // 他说“周
    assert_eq!(
        titles(&json),
        vec!["他说“周杰”"],
        "trigram 必须按字面处理引号"
    );

    // ASCII 大小写与全角：查询先规范化。
    let json = json_of(app.call("/api/v1/search?q=QUEEN").await).await;
    assert_eq!(titles(&json), vec!["we will rock you"]);
    let json = json_of(
        app.call("/api/v1/search?q=%EF%BD%8A%EF%BD%81%EF%BD%99")
            .await,
    )
    .await; // ＪＡＹ 全角3字
    assert_eq!(titles(&json), vec!["jay zhou live"]);

    // % 与 _ 作为普通字符（1–2 字符路径同样验证）：_h 前缀只命中字面下划线。
    let json = json_of(app.call("/api/v1/search?q=_h").await).await;
    assert_eq!(
        titles(&json),
        vec!["_hidden track"],
        "下划线不得当单字符通配"
    );
    let json = json_of(app.call("/api/v1/search?q=50%25").await).await // "50%"
    ;
    assert_eq!(titles(&json), vec!["50% off"], "百分号不得当任意串通配");

    // FTS 元字符不会把查询炸成语法错误或伪匹配。
    let json = json_of(
        app.call("/api/v1/search?q=%22%E5%91%A8%E6%9D%B0%22%20AND")
            .await,
    )
    .await; // "周杰" AND ——整体按字面
    assert_eq!(titles(&json), Vec::<String>::new());

    // 不存在的名称：空页而不是报错。
    let json = json_of(
        app.call("/api/v1/search?q=%E4%B8%8D%E5%AD%98%E5%9C%A8%E7%9A%84%E6%AD%8C")
            .await,
    ) // 不存在的歌
    .await;
    assert_eq!(json["items"].as_array().unwrap().len(), 0);
    assert_eq!(json["has_more"].as_bool(), Some(false));

    // q 缺失 / 空白 / 超长。
    let response = app.call("/api/v1/search").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = app.call("/api/v1/search?q=%20%20").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let long = "字".repeat(65);
    let response = app.call(&format!("/api/v1/search?q={long}")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn search_pagination_walks_fts_results() {
    let mut app = App::seeded().await;
    let owned: Vec<String> = (0..10).map(|i| format!("周杰伦 live take {i}")).collect();
    let seed: Vec<(&str, &str, &str, i64, i64)> = owned
        .iter()
        .map(|title| (title.as_str(), "", "", 0, 0))
        .collect();
    app.seed_tracks(&seed).await;
    // walk 会校验每页 has_more/next_cursor 的一致性并在末页要求游标为空。
    let ids = app
        .walk("/api/v1/search?q=%E5%91%A8%E6%9D%B0%E4%BC%A6&limit=3")
        .await;
    assert_eq!(ids.len(), 10, "FTS 翻页必须遍历全部命中：{ids:?}");
    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), 10, "FTS 翻页出现重复：{ids:?}");
}

#[tokio::test]
async fn queue_ids_scopes_limit_and_ownership() {
    let mut app = App::seeded().await;
    // 1005 条库内曲目：library 范围必须截断为 1000 并置 truncated。
    app.bulk_seed(1005).await;
    let json = json_of(app.call("/api/v1/queue-ids?scope=library").await).await;
    let ids = json["ids"].as_array().unwrap();
    assert_eq!(ids.len(), 1000);
    assert_eq!(json["truncated"].as_bool(), Some(true));

    // album 范围按 (disc,track,id)；缺 album_id 或非法 scope 拒绝。
    app.seed_tracks(&[("t1", "a", "专", 1, 5), ("t2", "a", "专", 1, 2)])
        .await;
    let albums = app
        .db
        .query_rows("SELECT id FROM albums", vec![])
        .await
        .unwrap();
    let album_id = albums[0][0].as_i64().unwrap();
    let json = json_of(
        app.call(&format!(
            "/api/v1/queue-ids?scope=album&album_id={album_id}"
        ))
        .await,
    )
    .await;
    let album_ids = json["ids"].as_array().unwrap();
    assert_eq!(album_ids.len(), 2);
    assert_eq!(json["truncated"].as_bool(), Some(false));
    let response = app.call("/api/v1/queue-ids?scope=album").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = app.call("/api/v1/queue-ids?scope=bogus").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // favorites/playlist 空表语义与所有权。
    let json = json_of(app.call("/api/v1/queue-ids?scope=favorites").await).await;
    assert_eq!(json["ids"].as_array().unwrap().len(), 0);
    let response = app
        .call("/api/v1/queue-ids?scope=playlist&playlist_id=999")
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    app.db
        .execute(
            "INSERT INTO users(id, username, password_hash) VALUES (2, 'other', 'x')",
            vec![],
        )
        .await
        .unwrap();
    app.db
        .execute(
            "INSERT INTO playlists(id, owner_id, name) VALUES (1, 2, '他人歌单')",
            vec![],
        )
        .await
        .unwrap();
    let response = app
        .call("/api/v1/queue-ids?scope=playlist&playlist_id=1")
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn albums_and_album_tracks_and_detail_endpoints() {
    let mut app = App::seeded().await;
    app.seed_tracks(&[
        ("c曲", "a", "乙专辑", 1, 2),
        ("a曲", "a", "乙专辑", 1, 1),
        ("b曲", "a", "乙专辑", 0, 9),
        ("d曲", "b", "甲专辑", 1, 1),
    ])
    .await;
    let json = json_of(app.call("/api/v1/albums").await).await;
    let album_titles: Vec<String> = json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["title"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(album_titles, vec!["乙专辑", "甲专辑"], "按 (sort_title,id)");
    let album_id = json["items"][0]["id"].as_str().unwrap();
    let json = json_of(app.call(&format!("/api/v1/albums/{album_id}/tracks")).await).await;
    assert_eq!(
        titles(&json),
        vec!["b曲", "a曲", "c曲"],
        "专辑曲目必须按 (disc,track,id)"
    );
    let ids: Vec<String> = json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_owned())
        .collect();
    let detail = json_of(app.call(&format!("/api/v1/tracks/{}", ids[0])).await).await;
    assert_eq!(detail["title"], "b曲");
    assert!(detail.get("relative_path").is_none(), "详情不泄露路径");
    assert_eq!(
        app.call("/api/v1/tracks/99999").await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.call("/api/v1/tracks/abc").await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.call("/api/v1/albums/99999/tracks").await.status(),
        StatusCode::NOT_FOUND
    );
}

async fn explain(db: &Db, sql: &str, params: Vec<Value>) -> String {
    let rows = db
        .query_rows(&format!("EXPLAIN QUERY PLAN {sql}"), params)
        .await
        .unwrap();
    rows.iter()
        .map(|row| {
            let detail = row.last().and_then(|v| v.as_text()).unwrap_or("");
            format!("{detail}\n")
        })
        .collect()
}

/// 计划里是否出现对某张表的裸全表扫描（`SCAN tracks USING INDEX ...` 不算）。
fn has_full_scan(plan: &str, table: &str) -> bool {
    let exact = format!("SCAN {table}");
    let subquery = format!("SCAN {table} AS SUBQUERY");
    plan.lines()
        .any(|line| line.trim() == exact || line.trim() == subquery)
}

/// 断言执行计划命中期望索引，且不做裸全表扫描。
fn assert_plan(plan: &str, expect: &[&str], allow_temp_tree: bool, case: &str) {
    for needle in expect {
        assert!(plan.contains(needle), "{case} 计划缺少 {needle}：\n{plan}");
    }
    for table in ["tracks", "albums"] {
        assert!(
            !has_full_scan(plan, table),
            "{case} 出现 {table} 全表扫描：\n{plan}"
        );
    }
    if !allow_temp_tree {
        assert!(
            !plan.contains("TEMP B-TREE"),
            "{case} 建了临时排序树，说明排序键未被索引满足：\n{plan}"
        );
    }
}

/// 1 万行实测 EXPLAIN QUERY PLAN 留存（2026-09-19，Windows x64，bundled SQLite）：
/// - 曲目首页/翻页：`SCAN tracks USING INDEX idx_tracks_title`（按排序键顺扫，无临时树）
/// - 歌手过滤（有无游标同）：`SEARCH tracks USING INDEX idx_tracks_artist_title (sort_artist=?)`
/// - recent 翻页：`SEARCH tracks USING INDEX idx_tracks_artist (sort_artist=? AND id<?)`
/// - 标题前缀：`SEARCH tracks USING INDEX idx_tracks_title (sort_title>? AND sort_title<?)`
/// - 歌手前缀：`SEARCH ... idx_tracks_artist (...)` + `USE TEMP B-TREE FOR ORDER BY`
///   （过滤列与排序列不同，只能对前缀命中集合排序，不是全表排序）
/// - FTS：`SCAN tracks_fts VIRTUAL TABLE INDEX 0:M3` + `SEARCH tracks USING INTEGER PRIMARY KEY (rowid=?)`
///   + `USE TEMP B-TREE FOR ORDER BY`（FTS 命中按 rowid 序，需重排）
/// - 专辑首页/翻页：`SCAN albums USING INDEX idx_albums_title`
/// - 专辑曲目翻页：`SEARCH tracks USING INDEX idx_tracks_album_id (album_id=?)`
///
/// 反面证据（修前的实测）：守卫式谓词 `(?1 IS NULL OR sort_artist COLLATE NOCASE = ?1)` 得到
/// `SCAN tracks USING INDEX idx_tracks_title`——扫完整个标题索引逐行过滤；albums 无排序索引时
/// 每页都是 `SCAN albums` + `USE TEMP B-TREE FOR ORDER BY`。这正是 002 迁移与条件化谓词要消除的。
/// 已知代价：未覆盖投影的曲目翻页仍是索引顺序扫描（游标不被折成 seek 区间），
/// 深页成本随 offset 线性增长，只读预算到点会返回 QUERY_TIMEOUT 而不是挂死。
#[tokio::test]
async fn explain_query_plan_at_10k_uses_prefix_indexes_without_full_scan() {
    use litebeat::library::query::{TrackSort, album_tracks_sql, albums_sql, tracks_sql};
    use litebeat::library::search::{fts_sql, prefix_sql};

    let mut app = App::seeded().await;
    app.bulk_seed(10_000).await;
    let db = Arc::clone(&app.db);
    let keys = || -> Vec<String> { vec!["bulk-10".into(), "10".into(), "0".into()] };

    // 0) 回归守卫：条件生成谓词，绝不能再写 `IS NULL OR` 空值守卫。
    for (sql, _) in [
        tracks_sql(None, TrackSort::Title, None, 51),
        tracks_sql(Some("a"), TrackSort::Title, Some(&keys()), 51),
        albums_sql(None, 51),
        album_tracks_sql(1, Some(&keys()), 51),
    ] {
        assert!(
            !sql.contains("IS NULL OR"),
            "守卫式谓词会毁掉区间扫描：\n{sql}"
        );
    }

    // 1) 无过滤 (sort_title,id) 首页：按标题索引顺序读，LIMIT 即可早停。
    let (sql, params) = tracks_sql(None, TrackSort::Title, None, 51);
    assert_plan(
        &explain(&db, &sql, params).await,
        &["idx_tracks_title"],
        false,
        "曲目首页",
    );

    // 2) 带游标翻页：走标题索引顺序扫描。跨列 OR 形态的 keyset 不会被折成
    // seek 区间（实测无 `(sort_title>?)`），但扫描方向与排序键一致，
    // 越过游标即停，因此既无临时树也不需要跳过 offset。
    let (sql, params) = tracks_sql(None, TrackSort::Title, Some(&keys()), 51);
    assert_plan(
        &explain(&db, &sql, params).await,
        &["idx_tracks_title"],
        false,
        "曲目翻页",
    );

    // 3) 歌手过滤 + 标题排序：必须命中 (歌手,标题,id) 复合索引且不建临时树。
    for (case, cursor) in [("无游标", None), ("带游标", Some(keys()))] {
        let (sql, params) = tracks_sql(
            Some("bulk-artist-3"),
            TrackSort::Title,
            cursor.as_deref(),
            51,
        );
        let plan = explain(&db, &sql, params).await;
        assert_plan(&plan, &["idx_tracks_artist_title"], false, case);
    }

    // 4) recent 排序按 rowid 逆序翻页，不得建临时树。
    let (sql, params) = tracks_sql(
        Some("bulk-artist-3"),
        TrackSort::Recent,
        Some(&keys()[..1]),
        51,
    );
    assert_plan(&explain(&db, &sql, params).await, &[], false, "recent 翻页");

    // 5) 1–2 字符短前缀：走被搜索列的表达式索引。
    let (sql, params) = prefix_sql("sort_title", "周%", None, 51);
    assert_plan(
        &explain(&db, &sql, params).await,
        &["idx_tracks_title"],
        false,
        "标题前缀",
    );
    // 过滤列与排序列不同：区间 seek 命中歌手索引，跨列排序只能临时树——
    // 排序规模已被前缀匹配集合限制，不是全表排序。
    let (sql, params) = prefix_sql("sort_artist", "周%", Some(&keys()), 51);
    assert_plan(
        &explain(&db, &sql, params).await,
        &["idx_tracks_artist"],
        true,
        "歌手前缀",
    );

    // 6) 3 字符及以上：FTS5 trigram 索引 + 主键回表，不扫 tracks 全表。
    let (sql, params) = fts_sql("\"周杰伦\"", Some(&keys()), 51);
    assert_plan(
        &explain(&db, &sql, params).await,
        &[
            "tracks_fts VIRTUAL TABLE INDEX",
            "USING INTEGER PRIMARY KEY",
        ],
        true,
        "FTS 搜索",
    );

    // 7) 专辑列表：002 迁移补的 (sort_title,id) 索引必须生效且无临时树。
    for (case, cursor) in [("首页", None), ("翻页", Some(keys()))] {
        let (sql, params) = albums_sql(cursor.as_deref(), 51);
        assert_plan(
            &explain(&db, &sql, params).await,
            &["idx_albums_title"],
            false,
            case,
        );
    }

    // 8) 专辑内曲目按 (album_id,disc_no,track_no,id) 索引顺序翻页。
    let (sql, params) = album_tracks_sql(1, Some(&keys()), 51);
    assert_plan(
        &explain(&db, &sql, params).await,
        &["idx_tracks_album_id"],
        false,
        "专辑曲目翻页",
    );

    // 9) 真机走查：计划好看 ≠ 数据完整，整库游标翻页必须与 SQL 全序逐条一致。
    let walked = app.walk("/api/v1/tracks?limit=100").await;
    let expected = db
        .query_rows(
            "SELECT id FROM tracks ORDER BY sort_title COLLATE NOCASE, id",
            vec![],
        )
        .await
        .unwrap();
    let expected: Vec<String> = expected
        .iter()
        .map(|row| row[0].as_i64().unwrap().to_string())
        .collect();
    assert_eq!(walked.len(), expected.len(), "游标翻页覆盖的行数不对");
    assert_eq!(
        walked, expected,
        "游标翻页与 SQL 全序不一致（重复/漏项/错序）"
    );

    // 10) 带歌手过滤的深页走查同样必须逐条一致。
    let walked = app
        .walk("/api/v1/tracks?limit=20&artist=bulk-artist-3")
        .await;
    let expected = db
        .query_rows(
            "SELECT id FROM tracks WHERE sort_artist = 'bulk-artist-3'
             ORDER BY sort_title COLLATE NOCASE, id",
            vec![],
        )
        .await
        .unwrap();
    let expected: Vec<String> = expected
        .iter()
        .map(|row| row[0].as_i64().unwrap().to_string())
        .collect();
    assert!(!expected.is_empty(), "夹具必须含该歌手的行");
    assert_eq!(walked, expected, "歌手过滤翻页与 SQL 全序不一致");
}
