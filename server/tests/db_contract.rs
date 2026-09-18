//! T02 数据库契约测试：模式、外键、FTS 一致性、PRAGMA、有界队列、
//! 查询预算与迁移版本门槛。对应计划 03 文档 T02 的验收清单。

use std::sync::Arc;
use std::time::{Duration, Instant};

use litebeat::db::{Db, DbError, DbOptions, Value};

fn options() -> DbOptions {
    DbOptions::default()
}

fn open(dir: &tempfile::TempDir) -> Arc<Db> {
    Db::open(dir.path().join("litebeat.db"), options()).unwrap()
}

async fn seed_root(db: &Db, name: &str, path: &str) -> i64 {
    let reply = db
        .execute(
            "INSERT INTO library_roots(name, canonical_path) VALUES (?, ?)",
            vec![Value::Text(name.into()), Value::Text(path.into())],
        )
        .await
        .unwrap();
    reply.last_rowid
}

async fn seed_track(db: &Db, root_id: i64, rel: &str, title: &str) -> i64 {
    let reply = db
        .execute(
            "INSERT INTO tracks(root_id, relative_path, title, sort_title, sort_artist, sort_album, size_bytes, mtime_ns)
             VALUES (?, ?, ?, ?, '', '', 1000, 1)",
            vec![
                Value::Integer(root_id),
                Value::Text(rel.into()),
                Value::Text(title.into()),
                Value::Text(title.into()),
            ],
        )
        .await
        .unwrap();
    reply.last_rowid
}

async fn count(db: &Db, sql: &str) -> i64 {
    let rows = db.query_rows(sql, vec![]).await.unwrap();
    rows[0][0].as_i64().unwrap()
}

fn is_constraint(error: &DbError) -> bool {
    matches!(error, DbError::Sql(rusqlite::Error::SqliteFailure(code, _))
    if matches!(
        code.code,
        rusqlite::ErrorCode::ConstraintViolation | rusqlite::ErrorCode::DatabaseBusy
    ))
}

#[tokio::test]
async fn migration_creates_schema_and_passes_integrity_checks() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let version = db
        .query_rows_on_write("PRAGMA user_version", vec![])
        .await
        .unwrap();
    assert_eq!(version[0][0].as_i64(), Some(1));
    let integrity = db
        .query_rows("PRAGMA integrity_check", vec![])
        .await
        .unwrap();
    assert_eq!(integrity[0][0].as_text(), Some("ok"));
    let fk = db
        .query_rows("PRAGMA foreign_key_check", vec![])
        .await
        .unwrap();
    assert!(fk.is_empty());
    let indexes = db
        .query_rows(
            "SELECT name FROM sqlite_master WHERE type='index' AND name IN
             ('idx_tracks_title','idx_tracks_artist','idx_tracks_album')",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(indexes.len(), 3, "三个规范化名称排序索引必须存在");
    db.close();
}

#[tokio::test]
async fn pragmas_verified_on_read_and_write_connections() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    // 轮询两个读线程：连发 4 次读取都应报告同一 PRAGMA 值。
    for _ in 0..4 {
        let cache = db.query_rows("PRAGMA cache_size", vec![]).await.unwrap();
        assert_eq!(cache[0][0].as_i64(), Some(-2048));
        let mmap = db.query_rows("PRAGMA mmap_size", vec![]).await.unwrap();
        assert_eq!(mmap[0][0].as_i64(), Some(0));
        let fk = db.query_rows("PRAGMA foreign_keys", vec![]).await.unwrap();
        assert_eq!(fk[0][0].as_i64(), Some(1));
    }
    let sync = db
        .query_rows_on_write("PRAGMA synchronous", vec![])
        .await
        .unwrap();
    assert_eq!(sync[0][0].as_i64(), Some(2), "写连接必须 synchronous=FULL");
    let journal = db
        .query_rows_on_write("PRAGMA journal_mode", vec![])
        .await
        .unwrap();
    assert_eq!(journal[0][0].as_text().unwrap().to_lowercase(), "wal");
    db.close();
}

#[tokio::test]
async fn foreign_keys_reject_unknown_parent() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let error = db
        .execute(
            "INSERT INTO tracks(root_id, relative_path, title, sort_title, sort_artist, sort_album, size_bytes, mtime_ns)
             VALUES (9999, 'x.mp3', 'x', 'x', '', '', 1, 1)",
            vec![],
        )
        .await
        .unwrap_err();
    assert!(is_constraint(&error), "应报告外键约束: {error:?}");
    db.close();
}

#[tokio::test]
async fn duplicate_path_within_root_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let root = seed_root(&db, "r", "/m").await;
    seed_track(&db, root, "a.mp3", "A").await;
    let error = seed_track_result(&db, root, "a.mp3", "A2").await;
    assert!(is_constraint(&error));
    db.close();
}

async fn seed_track_result(db: &Db, root: i64, rel: &str, title: &str) -> DbError {
    db.execute(
        "INSERT INTO tracks(root_id, relative_path, title, sort_title, sort_artist, sort_album, size_bytes, mtime_ns)
         VALUES (?, ?, ?, ?, '', '', 1000, 1)",
        vec![
            Value::Integer(root),
            Value::Text(rel.into()),
            Value::Text(title.into()),
            Value::Text(title.into()),
        ],
    )
    .await
    .unwrap_err()
}

#[tokio::test]
async fn failed_transaction_rolls_back_tracks_and_fts() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let root = seed_root(&db, "r", "/m").await;
    let statements = vec![
        (
            "INSERT INTO tracks(root_id, relative_path, title, sort_title, sort_artist, sort_album, size_bytes, mtime_ns)
             VALUES (?, 'ok.mp3', 'Ok', 'Ok', '', '', 1, 1)"
                .to_string(),
            vec![Value::Integer(root)],
        ),
        (
            "INSERT INTO tracks(root_id, relative_path, title, sort_title, sort_artist, sort_album, size_bytes, mtime_ns)
             VALUES (9999, 'bad.mp3', 'Bad', 'Bad', '', '', 1, 1)"
                .to_string(),
            vec![],
        ),
    ];
    let error = db.transaction(statements).await.unwrap_err();
    assert!(is_constraint(&error));
    assert_eq!(count(&db, "SELECT count(*) FROM tracks").await, 0);
    let hits = db
        .query_rows(
            "SELECT count(*) FROM tracks_fts WHERE tracks_fts MATCH ?",
            vec![Value::Text("\"Ok\"".into())],
        )
        .await
        .unwrap();
    assert_eq!(hits[0][0].as_i64(), Some(0), "FTS 不得残留回滚行");
    db.close();
}

#[tokio::test]
async fn fts_trigram_tracks_insert_update_delete() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let root = seed_root(&db, "r", "/m").await;
    let track = seed_track(&db, root, "a.mp3", "Hello World").await;
    let fts_hits = |db: Arc<Db>, phrase: String| async move {
        let rows = db
            .query_rows(
                "SELECT rowid FROM tracks_fts WHERE tracks_fts MATCH ? ORDER BY rowid",
                vec![Value::Text(phrase)],
            )
            .await
            .unwrap();
        rows.into_iter()
            .map(|r| r[0].as_i64().unwrap())
            .collect::<Vec<_>>()
    };
    let db1 = Arc::clone(&db);
    assert_eq!(fts_hits(db1, "\"lo W\"".into()).await, vec![track]);
    db.execute(
        "UPDATE tracks SET title='Goodbye', sort_title='Goodbye' WHERE id = ?",
        vec![Value::Integer(track)],
    )
    .await
    .unwrap();
    let db2 = Arc::clone(&db);
    assert!(
        fts_hits(db2, "\"lo W\"".into()).await.is_empty(),
        "旧词必须消失"
    );
    let db3 = Arc::clone(&db);
    assert_eq!(fts_hits(db3, "\"ood\"".into()).await, vec![track]);
    // 外部内容表一致性检查在每次变更后必须通过。
    db.execute(
        "INSERT INTO tracks_fts(tracks_fts) VALUES('integrity-check')",
        vec![],
    )
    .await
    .unwrap();
    db.execute(
        "DELETE FROM tracks WHERE id = ?",
        vec![Value::Integer(track)],
    )
    .await
    .unwrap();
    let db4 = Arc::clone(&db);
    assert!(
        fts_hits(db4, "\"ood\"".into()).await.is_empty(),
        "删除后不得残留"
    );
    db.execute(
        "INSERT INTO tracks_fts(tracks_fts) VALUES('integrity-check')",
        vec![],
    )
    .await
    .unwrap();
    db.close();
}

#[tokio::test]
async fn playlist_items_allow_repeats_but_reject_position_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let root = seed_root(&db, "r", "/m").await;
    let track = seed_track(&db, root, "a.mp3", "A").await;
    let user = db
        .execute(
            "INSERT INTO users(username, password_hash) VALUES ('admin', 'x')",
            vec![],
        )
        .await
        .unwrap()
        .last_rowid;
    let playlist = db
        .execute(
            "INSERT INTO playlists(owner_id, name) VALUES (?, 'list')",
            vec![Value::Integer(user)],
        )
        .await
        .unwrap()
        .last_rowid;
    let insert = |position: i64| {
        (
            "INSERT INTO playlist_items(playlist_id, track_id, position) VALUES (?, ?, ?)"
                .to_string(),
            vec![
                Value::Integer(playlist),
                Value::Integer(track),
                Value::Integer(position),
            ],
        )
    };
    db.transaction(vec![insert(0)]).await.unwrap();
    db.transaction(vec![insert(1)]).await.unwrap(); // 同一首重复出现
    let error = db.transaction(vec![insert(0)]).await.unwrap_err();
    assert!(is_constraint(&error));
    db.close();
}

#[tokio::test]
async fn slow_read_hits_query_budget_and_worker_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let start = Instant::now();
    let error = db
        .query_rows(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 50000000)
             SELECT max(i) FROM n",
            vec![],
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, DbError::QueryTimeout),
        "预期超时: {error:?}"
    );
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(count(&db, "SELECT 1").await, 1, "超时后读线程仍可服务");
    db.close();
}

fn sleeping_reply_long(
    _conn: &rusqlite::Connection,
) -> Result<litebeat::db::Reply, rusqlite::Error> {
    std::thread::sleep(Duration::from_millis(700));
    Ok(litebeat::db::Reply {
        rows: None,
        affected: 0,
        last_rowid: 0,
    })
}

fn empty_reply(_conn: &rusqlite::Connection) -> Result<litebeat::db::Reply, rusqlite::Error> {
    Ok(litebeat::db::Reply {
        rows: None,
        affected: 0,
        last_rowid: 0,
    })
}

#[tokio::test]
async fn read_queue_full_returns_busy_and_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(
        dir.path().join("litebeat.db"),
        DbOptions {
            read_threads: 2,
            read_queue_capacity: 1,
            ..DbOptions::default()
        },
    )
    .unwrap();
    // 两个在途长任务占住读线程；队列容量各 1。
    let mut holding = vec![db.submit_read(sleeping_reply_long).unwrap()];
    holding.push(db.submit_read(sleeping_reply_long).unwrap());
    let mut rejected = 0;
    let mut accepted = Vec::new();
    for _ in 0..6 {
        match db.submit_read(empty_reply) {
            Ok(rx) => accepted.push(rx),
            Err(DbError::QueueFull) => rejected += 1,
            Err(other) => panic!("只应报告忙碌: {other:?}"),
        }
    }
    assert!(
        rejected >= 4,
        "容量 1×2 时必须有大量快速拒绝，实际 {rejected}"
    );
    std::thread::sleep(Duration::from_millis(900));
    drop(holding);
    drop(accepted);
    assert_eq!(count(&db, "SELECT 1").await, 1, "队列排空后恢复正常");
    db.close();
}

#[tokio::test]
async fn write_payload_cap_rejects_before_queueing() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    // 先让写线程睡住（自身载荷 0），再连续提交 1.5 MiB 写命令；
    // 队列中最多容纳 2 个（3 MiB ≤ 4 MiB < 4.5 MiB）。
    let holding = db.submit_write(0, sleeping_reply_long).unwrap();
    let mut outcomes = Vec::new();
    for _ in 0..4 {
        outcomes.push(db.submit_write(1_572_864, empty_reply));
    }
    let accepted = outcomes.iter().filter(|o| o.is_ok()).count();
    let rejected = outcomes.iter().filter(|o| o.is_err()).count();
    assert_eq!(accepted, 2, "恰好 2×1.5MiB 能进入 4MiB 载荷预算");
    assert_eq!(rejected, 2);
    for outcome in outcomes.into_iter().filter_map(|o| o.ok()) {
        let reply = tokio::time::timeout(Duration::from_secs(5), outcome)
            .await
            .expect("写命令应在写线程醒来后执行")
            .expect("通道存活")
            .expect("已接受的写命令必须成功");
        assert_eq!(reply.affected, 0);
    }
    drop(holding);
    assert_eq!(count(&db, "SELECT 1").await, 1);
    db.close();
}

#[tokio::test]
async fn newer_schema_version_refuses_to_open_with_guidance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("litebeat.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA user_version = 99;").unwrap();
    }
    let error = Db::open(&path, options()).unwrap_err();
    match error {
        DbError::Migration(message) => {
            assert!(message.contains("99"), "错误必须带版本号: {message}");
            assert!(
                message.contains("备份") || message.contains("升级"),
                "必须给恢复指导: {message}"
            );
        }
        other => panic!("预期 Migration 拒绝: {other:?}"),
    }
}

#[tokio::test]
async fn reopen_existing_database_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir);
    let root = seed_root(&db, "r", "/m").await;
    seed_track(&db, root, "a.mp3", "保持我").await;
    db.close();
    let db = Db::open(dir.path().join("litebeat.db"), options()).unwrap();
    assert_eq!(count(&db, "SELECT count(*) FROM tracks").await, 1);
    let integrity = db
        .query_rows("PRAGMA integrity_check", vec![])
        .await
        .unwrap();
    assert_eq!(integrity[0][0].as_text(), Some("ok"));
    db.close();
}
