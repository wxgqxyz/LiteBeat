//! 扫描协调器与 `/api/v1/admin/scans*` 端点（架构文档 §5.1、§7.2）。
//!
//! 运行语义：
//! - 同一时间只有一个扫描任务；第二个创建请求 409 并给出已有任务 ID。
//! - 以 `(root_id, relative_path, size_bytes, mtime_ns)` 判定增量；未变文件只做
//!   本轮标记（stamp），不重新解析；`force` 强制重读标签。
//! - 解析结果按 ≤100 条或 ≤100ms 组批，单事务提交（albums + tracks，FTS 由触发器同步）。
//! - **仅当整个根目录遍历成功**才把未见条目置 unavailable；取消或目录掉线保留原状态。
//! - 进度增量写回 scan_jobs；错误明细最多 1000 行，另计 failed 总数。
//! - 服务启动把 queued/running 任务标为 interrupted（进程重启必丢任务）。

// 内部助手用 axum `Response` 作错误类型是刻意写法：一次转换、直接返回，
// 比 Box 包装或自定义错误枚举更贴近路由层的既有模式。
#![allow(clippy::result_large_err)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{DefaultBodyLimit, Json, State};
use axum::http::{HeaderMap, HeaderName, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::auth::{authenticate, csrf};
use crate::db::{Db, DbError, Reply, Value};
use crate::error::json_error;
use crate::http::AppState;
use crate::scanner::protocol::{FileMetadata, MAX_ERROR_DETAIL_ROWS, ScanResponse};
use crate::scanner::walk::{DirectoryWalker, WalkItem};
use crate::scanner::worker::{ISOLATION_MODE, ParseWorker, WorkerConfig};

const REQUEST_ID: &str = "scan";
const BATCH_MAX_ITEMS: usize = 100;
const BATCH_MAX_AGE: Duration = Duration::from_millis(100);
const DEFAULT_PARSE_TIMEOUT: Duration = Duration::from_secs(5);
const WALK_QUEUE_ITEMS: usize = 64;
const IDEMPOTENCY_HEADER: HeaderName = HeaderName::from_static("idempotency-key");
const MAX_IDEMPOTENCY_KEY_CHARS: usize = 64;
const DEDUP_ROW_LIMIT: i64 = 10_000;

/// 协调器的可调参数；生产走默认值，测试用注入点覆盖。
#[derive(Debug, Clone)]
pub struct ScanConfig {
    pub worker: WorkerConfig,
    pub parse_timeout: Duration,
    /// 测试注入：遍历出第 N 个文件后模拟目录掉线（注入一次遍历错误并停止）。
    pub inject_dir_error_after_files: Option<u64>,
    /// 测试注入：每个文件处理前额外等待，制造可取消的慢扫描。
    pub per_file_delay: Option<Duration>,
}

impl ScanConfig {
    pub fn production() -> Self {
        Self {
            worker: WorkerConfig::for_current_exe(),
            parse_timeout: DEFAULT_PARSE_TIMEOUT,
            inject_dir_error_after_files: None,
            per_file_delay: None,
        }
    }
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self::production()
    }
}

#[derive(Debug)]
struct Running {
    job_id: i64,
    cancel: Arc<AtomicBool>,
}

#[derive(Debug)]
pub struct ScanEnv {
    config: std::sync::Mutex<ScanConfig>,
    running: std::sync::Mutex<Option<Running>>,
    isolation_active: std::sync::Mutex<Option<bool>>,
}

impl ScanEnv {
    pub fn new(config: ScanConfig) -> Arc<Self> {
        Arc::new(Self {
            config: std::sync::Mutex::new(config),
            running: std::sync::Mutex::new(None),
            isolation_active: std::sync::Mutex::new(None),
        })
    }

    /// 测试钩子：扫描启动前调整注入参数；运行中的任务沿用其启动时的快照。
    pub fn set_config(&self, adjust: impl FnOnce(&mut ScanConfig)) {
        adjust(&mut self.config.lock().unwrap_or_else(|e| e.into_inner()));
    }

    fn config_snapshot(&self) -> ScanConfig {
        self.config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn isolation_summary(&self) -> String {
        let active = *self
            .isolation_active
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match active {
            Some(true) => ISOLATION_MODE.to_owned(),
            Some(false) => format!("{ISOLATION_MODE}:unavailable"),
            None => format!("{ISOLATION_MODE}:idle"),
        }
    }

    pub fn running_job(&self) -> Option<i64> {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|r| r.job_id > 0)
            .map(|r| r.job_id)
    }

    /// 取消请求：命中在途任务返回 true。幂等由调用方结合 DB 状态处理。
    pub fn cancel(&self, job_id: i64) -> bool {
        let guard = self.running.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(running) if running.job_id == job_id => {
                running.cancel.store(true, Ordering::Relaxed);
                true
            }
            _ => false,
        }
    }

    /// 原子占位，防止并发创建出现双任务；job_id 先置 -1，创建成功后回填。
    fn try_reserve(self: &Arc<Self>) -> Option<Arc<AtomicBool>> {
        let mut guard = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return None;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *guard = Some(Running {
            job_id: -1,
            cancel: Arc::clone(&cancel),
        });
        Some(cancel)
    }

    fn confirm_job(&self, job_id: i64) {
        if let Some(running) = self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            running.job_id = job_id;
        }
    }

    fn release(&self) {
        *self.running.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn record_isolation(&self, active: bool) {
        let mut guard = self
            .isolation_active
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // 首个子进程的结果代表本机能力，后续不再翻转。
        if guard.is_none() {
            *guard = Some(active);
        }
    }

    /// 占位成功后调用：把扫描任务挂到当前运行时。
    pub fn start(
        self: &Arc<Self>,
        db: Arc<Db>,
        job_id: i64,
        root_id: i64,
        root_path: PathBuf,
        force: bool,
        cancel: Arc<AtomicBool>,
    ) {
        self.confirm_job(job_id);
        let env = Arc::clone(self);
        let config = self.config_snapshot();
        tokio::spawn(async move {
            let result = run_scan(
                env.clone(),
                config,
                db,
                job_id,
                root_id,
                root_path,
                force,
                cancel,
            )
            .await;
            if let Err(error) = result {
                tracing::warn!(job_id, error = %error, "扫描任务以失败结束");
            }
            env.release();
        });
    }
}

impl Default for ScanEnv {
    fn default() -> Self {
        Self {
            config: std::sync::Mutex::new(ScanConfig::default()),
            running: std::sync::Mutex::new(None),
            isolation_active: std::sync::Mutex::new(None),
        }
    }
}

/// 服务启动时调用：上次进程遗留的 queued/running 一律标为 interrupted。
pub async fn mark_interrupted(db: &Db) -> Result<(), DbError> {
    db.execute(
        "UPDATE scan_jobs SET state = 'interrupted', finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE state IN ('queued', 'running')",
        vec![],
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 扫描主循环
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum Pending {
    /// 文件仍在且未变化：仅刷新 last_seen 与 available。
    Stamp { relative_path: String },
    /// 新增或变化：用解析出的元数据整行覆盖。
    Upsert {
        relative_path: String,
        size_bytes: i64,
        mtime_ns: i64,
        meta: FileMetadata,
    },
}

// 一次性的内部编排函数：参数即该次扫描的全部上下文，拆开重组反而晦涩。
#[allow(clippy::too_many_arguments)]
async fn run_scan(
    env: Arc<ScanEnv>,
    config: ScanConfig,
    db: Arc<Db>,
    job_id: i64,
    root_id: i64,
    root_path: PathBuf,
    force: bool,
    cancel: Arc<AtomicBool>,
) -> Result<(), DbError> {
    db.execute(
        "UPDATE scan_jobs SET state = 'running', started_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
        vec![Value::Integer(job_id)],
    )
    .await?;

    let (tx, mut rx) = mpsc::channel::<WalkItem>(WALK_QUEUE_ITEMS);
    let producer_cancel = Arc::clone(&cancel);
    let producer_root = root_path.clone();
    let fault_after = config.inject_dir_error_after_files;
    let producer = tokio::task::spawn_blocking(move || {
        let mut files_seen = 0u64;
        let mut done = true;
        for item in DirectoryWalker::new(&producer_root) {
            if producer_cancel.load(Ordering::Relaxed) {
                done = false;
                break;
            }
            let is_file = matches!(item, WalkItem::File(_));
            if tokio::runtime::Handle::current()
                .block_on(tx.send(item))
                .is_err()
            {
                done = false;
                break;
            }
            if is_file {
                files_seen += 1;
                if fault_after == Some(files_seen) {
                    // 模拟挂载盘中途掉线：遍历不再可信，禁止收尾标记。
                    let _ = tokio::runtime::Handle::current().block_on(tx.send(WalkItem::Error {
                        relative_path: String::new(),
                        code: "INJECTED_ROOT_LOST".to_owned(),
                    }));
                    done = false;
                    break;
                }
            }
        }
        drop(tx);
        done
    });

    let mut worker: Option<ParseWorker> = None;
    let mut batch: Vec<Pending> = Vec::new();
    let mut last_flush = Instant::now();
    let (mut scanned, mut updated, mut failed) = (0i64, 0i64, 0i64);
    let mut error_rows = 0usize;
    let mut walk_ok = true;
    let mut cancelled = false;

    while let Some(item) = rx.recv().await {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        if let Some(delay) = config.per_file_delay {
            tokio::time::sleep(delay).await;
        }
        match item {
            WalkItem::SkippedLink { relative_path } => {
                record_error(&db, job_id, &mut error_rows, &relative_path, "LINK_SKIPPED").await;
            }
            WalkItem::Error {
                relative_path,
                code,
            } => {
                walk_ok = false;
                failed += 1;
                record_error(&db, job_id, &mut error_rows, &relative_path, &code).await;
            }
            WalkItem::File(entry) => {
                scanned += 1;
                let existing = db
                    .query_rows(
                        "SELECT id, size_bytes, mtime_ns FROM tracks WHERE root_id = ? AND relative_path = ?",
                        vec![Value::Integer(root_id), Value::Text(entry.relative_path.clone())],
                    )
                    .await?;
                let existing = existing.into_iter().next();
                let unchanged = !force
                    && existing.as_ref().is_some_and(|row| {
                        row.get(1).and_then(Value::as_i64) == Some(entry.size_bytes)
                            && row.get(2).and_then(Value::as_i64) == Some(entry.mtime_ns)
                    });
                let resolution = if unchanged {
                    Pending::Stamp {
                        relative_path: entry.relative_path,
                    }
                } else {
                    match parse_one(&env, &config, &mut worker, &entry).await {
                        ParseOutcome::Meta(meta) => Pending::Upsert {
                            relative_path: entry.relative_path,
                            size_bytes: entry.size_bytes,
                            mtime_ns: entry.mtime_ns,
                            meta,
                        },
                        ParseOutcome::Failed(code) => {
                            failed += 1;
                            record_error(&db, job_id, &mut error_rows, &entry.relative_path, &code)
                                .await;
                            // 旧行仍在原位：保留元数据，只确认“文件还在”。
                            if existing.is_some() {
                                Pending::Stamp {
                                    relative_path: entry.relative_path,
                                }
                            } else {
                                continue;
                            }
                        }
                    }
                };
                batch.push(resolution);
            }
        }
        if batch.len() >= BATCH_MAX_ITEMS || last_flush.elapsed() >= BATCH_MAX_AGE {
            updated += flush(&db, job_id, root_id, &mut batch).await?;
            last_flush = Instant::now();
            db.execute(
                "UPDATE scan_jobs SET scanned = ?, updated = ?, failed = ? WHERE id = ?",
                vec![
                    Value::Integer(scanned),
                    Value::Integer(updated),
                    Value::Integer(failed),
                    Value::Integer(job_id),
                ],
            )
            .await?;
        }
    }
    updated += flush(&db, job_id, root_id, &mut batch).await?;
    let _ = producer.await;
    if let Some(live_worker) = worker {
        live_worker.shutdown().await;
    }

    let state = if cancelled || cancel.load(Ordering::Relaxed) {
        "cancelled"
    } else if walk_ok {
        // 仅完整遍历才允许把未见条目判为不可用。
        db.execute(
            "UPDATE tracks SET available = 0
             WHERE root_id = ? AND available = 1
               AND COALESCE(last_seen_scan_id, -1) <> ?",
            vec![Value::Integer(root_id), Value::Integer(job_id)],
        )
        .await?;
        "completed"
    } else {
        "failed"
    };
    db.execute(
        "UPDATE scan_jobs SET state = ?, finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
         scanned = ?, updated = ?, failed = ? WHERE id = ?",
        vec![
            Value::Text(state.to_owned()),
            Value::Integer(scanned),
            Value::Integer(updated),
            Value::Integer(failed),
            Value::Integer(job_id),
        ],
    )
    .await?;
    tracing::info!(job_id, state, scanned, updated, failed, "扫描任务结束");
    Ok(())
}

enum ParseOutcome {
    Meta(FileMetadata),
    Failed(String),
}

async fn parse_one(
    env: &Arc<ScanEnv>,
    config: &ScanConfig,
    worker: &mut Option<ParseWorker>,
    entry: &crate::scanner::walk::WalkEntry,
) -> ParseOutcome {
    if worker.is_none() {
        match ParseWorker::spawn(&config.worker).await {
            Ok(spawned) => {
                env.record_isolation(spawned.isolation_active());
                *worker = Some(spawned);
            }
            Err(error) => {
                tracing::error!(error = %error, "无法拉起扫描子进程");
                env.record_isolation(false);
                return ParseOutcome::Failed("WORKER_UNAVAILABLE".to_owned());
            }
        }
    }
    let live = worker.as_mut().expect("worker 刚被确认存在");
    let response = live.parse(&entry.absolute, config.parse_timeout).await;
    match response {
        Ok(ScanResponse {
            ok: true,
            meta: Some(meta),
            ..
        }) => ParseOutcome::Meta(meta),
        Ok(ScanResponse {
            code: Some(code), ..
        }) => {
            // 子进程上报的业务失败（如 PARSE_FAILED）不需要重建进程。
            ParseOutcome::Failed(code)
        }
        Ok(_) => ParseOutcome::Failed("WORKER_PROTOCOL_VIOLATION".to_owned()),
        Err(failure) => {
            // parse() 内部已杀死进程；置 None 让下一个文件重新拉起。
            *worker = None;
            ParseOutcome::Failed(failure.error_code().to_owned())
        }
    }
}

async fn record_error(db: &Db, job_id: i64, written: &mut usize, path: &str, code: &str) {
    if *written >= MAX_ERROR_DETAIL_ROWS {
        return;
    }
    *written += 1;
    if let Err(error) = db
        .execute(
            "INSERT OR IGNORE INTO scan_errors(job_id, relative_path, code) VALUES (?, ?, ?)",
            vec![
                Value::Integer(job_id),
                Value::Text(path.to_owned()),
                Value::Text(code.to_owned()),
            ],
        )
        .await
    {
        tracing::warn!(%error, "扫描错误明细写入失败");
    }
}

/// 单事务提交一批：albums 去重 + tracks upsert；返回本批写入（新增或整行更新）数。
async fn flush(
    db: &Db,
    job_id: i64,
    root_id: i64,
    batch: &mut Vec<Pending>,
) -> Result<i64, DbError> {
    if batch.is_empty() {
        return Ok(0);
    }
    let statements = std::mem::take(batch);
    let payload = statements.len() * 2048;
    let reply = db
        .submit_write(payload, move |conn| {
            use rusqlite::params;
            let tx = conn.unchecked_transaction()?;
            let mut upserted = 0i64;
            for item in &statements {
                match item {
                    Pending::Stamp { relative_path } => {
                        tx.execute(
                            "UPDATE tracks SET last_seen_scan_id = ?, available = 1
                             WHERE root_id = ? AND relative_path = ?",
                            params![job_id, root_id, relative_path],
                        )?;
                    }
                    Pending::Upsert {
                        relative_path,
                        size_bytes,
                        mtime_ns,
                        meta,
                    } => {
                        let directory_key = match relative_path.rfind('/') {
                            Some(index) => relative_path[..index].to_owned(),
                            None => String::new(),
                        };
                        let album_id: Option<i64> = if meta.album_title.is_empty() {
                            None
                        } else {
                            tx.execute(
                                "INSERT INTO albums(root_id, directory_key, title, album_artist, sort_title)
                                 VALUES (?, ?, ?, ?, ?)
                                 ON CONFLICT(root_id, directory_key, title, album_artist) DO NOTHING",
                                params![
                                    root_id,
                                    directory_key,
                                    meta.album_title,
                                    meta.album_artist,
                                    sort_key(&meta.album_title)
                                ],
                            )?;
                            let id: i64 = tx.query_row(
                                "SELECT id FROM albums
                                 WHERE root_id = ? AND directory_key = ? AND title = ? AND album_artist = ?",
                                params![root_id, directory_key, meta.album_title, meta.album_artist],
                                |row| row.get(0),
                            )?;
                            Some(id)
                        };
                        upserted += tx.execute(
                            "INSERT INTO tracks(root_id, relative_path, title, artist, album_id,
                                album_title, duration_ms, codec, mime, size_bytes, mtime_ns,
                                sort_title, sort_artist, sort_album, disc_no, track_no,
                                available, last_seen_scan_id)
                             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?)
                             ON CONFLICT(root_id, relative_path) DO UPDATE SET
                                title = excluded.title,
                                artist = excluded.artist,
                                album_id = excluded.album_id,
                                album_title = excluded.album_title,
                                duration_ms = excluded.duration_ms,
                                codec = excluded.codec,
                                mime = excluded.mime,
                                size_bytes = excluded.size_bytes,
                                mtime_ns = excluded.mtime_ns,
                                sort_title = excluded.sort_title,
                                sort_artist = excluded.sort_artist,
                                sort_album = excluded.sort_album,
                                disc_no = excluded.disc_no,
                                track_no = excluded.track_no,
                                available = 1,
                                last_seen_scan_id = excluded.last_seen_scan_id",
                            params![
                                root_id,
                                relative_path,
                                meta.title,
                                meta.artist,
                                album_id,
                                meta.album_title,
                                meta.duration_ms,
                                meta.codec,
                                meta.mime,
                                size_bytes,
                                mtime_ns,
                                sort_key(&meta.title),
                                sort_key(&meta.artist),
                                sort_key(&meta.album_title),
                                meta.disc_no,
                                meta.track_no,
                                job_id
                            ],
                        )? as i64;
                    }
                }
            }
            let affected = tx.changes();
            tx.commit()?;
            Ok(Reply {
                rows: None,
                affected,
                last_rowid: upserted,
            })
        })
        .unwrap_or_else(quick_failed_write);
    let updated = match reply.await {
        Ok(inner) => inner?.last_rowid,
        Err(_) => return Err(DbError::Closed),
    };
    Ok(updated)
}

/// submit_write 同步失败时伪装成已关闭的通道，统一走 await 分支。
fn quick_failed_write(error: DbError) -> tokio::sync::oneshot::Receiver<Result<Reply, DbError>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = tx.send(Err(error));
    rx
}

/// 排序键规范化与 T06 查询共用同一函数（NFKC、压空白、小写），
/// 保证「规范化字段与查询使用同一函数」不在两处漂移。
pub fn sort_key(value: &str) -> String {
    crate::library::normalize::normalize(value)
}

// ---------------------------------------------------------------------------
// admin 端点
// ---------------------------------------------------------------------------

pub fn scanner_router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/admin/scans", post(create_scan))
        .route("/api/v1/admin/scans/{id}", get(scan_status))
        .route("/api/v1/admin/scans/{id}/cancel", post(cancel_scan))
        .layer(DefaultBodyLimit::max(8 * 1024))
}

#[derive(Debug, Deserialize)]
struct CreateScanRequest {
    root_id: String,
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct ScanCreated {
    job_id: String,
}

async fn create_scan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateScanRequest>,
) -> Response {
    let Some(db) = state.db.clone() else {
        return not_ready();
    };
    let session = match authenticated(&db, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = guard_write(&session, &headers) {
        return response;
    }
    let Ok(root_id) = body.root_id.trim().parse::<i64>() else {
        return bad_request("root_id 必须是十进制字符串 ID");
    };
    let key = match idempotency_key(&headers) {
        Some(key) => key,
        None => return bad_request("缺少 Idempotency-Key 请求头"),
    };
    let rows = match db
        .query_rows(
            "SELECT canonical_path FROM library_roots WHERE id = ? AND enabled = 1",
            vec![Value::Integer(root_id)],
        )
        .await
    {
        Ok(rows) => rows,
        Err(error) => return db_error(error),
    };
    let Some(root_path) = rows
        .first()
        .and_then(|row| row.first())
        .and_then(Value::as_text)
        .map(str::to_owned)
    else {
        return json_error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "曲库根目录不存在或未启用",
            REQUEST_ID,
        );
    };
    let root_path = PathBuf::from(root_path);
    if !root_path.is_dir() {
        return json_error(
            StatusCode::CONFLICT,
            "ROOT_UNAVAILABLE",
            "曲库根目录当前不可访问",
            REQUEST_ID,
        );
    }
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(format!("POST /api/v1/admin/scans {root_id} {}", body.force));
    let request_hash = hasher.finalize().to_vec();
    // 幂等重放必须先于“已有任务”检查，否则成功响应丢失后的重试会被误判 409。
    let existing = match lookup_dedup(&db, session.user_id, &key, &request_hash).await {
        Ok(existing) => existing,
        Err(response) => return response,
    };
    if let Some(response_json) = existing {
        return match serde_json::from_str::<ScanCreated>(&response_json) {
            Ok(payload) => (
                StatusCode::ACCEPTED,
                [(header::CACHE_CONTROL, "no-store")],
                Json(payload),
            )
                .into_response(),
            Err(_) => json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "幂等记录损坏",
                REQUEST_ID,
            ),
        };
    }
    let Some(cancel) = state.scans.try_reserve() else {
        let running = state
            .scans
            .running_job()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "未知".into());
        return json_error(
            StatusCode::CONFLICT,
            "SCAN_RUNNING",
            &format!("已有扫描任务（#{running}）进行中，请等待其结束"),
            REQUEST_ID,
        );
    };
    let force = body.force;
    let user_id = session.user_id;
    let created = db
        .submit_write(
            1024 + key.len(),
            move |conn| {
                use rusqlite::params;
                let tx = conn.unchecked_transaction()?;
                tx.execute(
                    "DELETE FROM request_dedup WHERE expires_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
                    [],
                )?;
                let live: i64 =
                    tx.query_row("SELECT COUNT(*) FROM request_dedup", [], |row| row.get(0))?;
                if live >= DEDUP_ROW_LIMIT {
                    return Err(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(rusqlite::ErrorCode::DiskFull as i32),
                        Some("幂等表已满".into()),
                    ));
                }
                tx.execute(
                    "INSERT INTO scan_jobs(root_id, state, force) VALUES (?, 'queued', ?)",
                    params![root_id, force as i64],
                )?;
                let job_id = tx.last_insert_rowid();
                let response_json = format!("{{\"job_id\":\"{job_id}\"}}");
                tx.execute(
                    "INSERT INTO request_dedup(user_id, key, method, path, request_hash, response_json, expires_at)
                     VALUES (?, ?, 'POST', '/api/v1/admin/scans', ?, ?,
                             strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+24 hours'))",
                    params![user_id, key, request_hash, response_json],
                )?;
                tx.commit()?;
                Ok(Reply {
                    rows: Some(vec![vec![Value::Integer(job_id)]]),
                    affected: 0,
                    last_rowid: job_id,
                })
            },
        )
        .map_err(db_error);
    let job_id = match created {
        Err(response) => return response,
        Ok(rx) => match rx.await {
            Ok(Ok(reply)) => reply
                .rows
                .and_then(|rows| rows.into_iter().next())
                .and_then(|mut row| row.pop())
                .and_then(|value| value.as_i64())
                .unwrap_or(0),
            Ok(Err(DbError::Sql(rusqlite::Error::SqliteFailure(code, _))))
                if code.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                state.scans.release();
                return json_error(
                    StatusCode::CONFLICT,
                    "IDEMPOTENCY_CONFLICT",
                    "相同幂等键的并发请求已创建记录",
                    REQUEST_ID,
                );
            }
            Ok(Err(DbError::Sql(rusqlite::Error::SqliteFailure(code, _))))
                if code.code == rusqlite::ErrorCode::DiskFull =>
            {
                state.scans.release();
                return json_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "DEDUP_FULL",
                    "幂等记录已满，请先清理",
                    REQUEST_ID,
                );
            }
            Ok(Err(error)) => {
                state.scans.release();
                return db_error(error);
            }
            Err(_) => {
                state.scans.release();
                return not_ready();
            }
        },
    };
    state
        .scans
        .clone()
        .start(db, job_id, root_id, root_path, force, cancel);
    (
        StatusCode::ACCEPTED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(ScanCreated {
            job_id: job_id.to_string(),
        }),
    )
        .into_response()
}

async fn authenticated(db: &Db, headers: &HeaderMap) -> Result<crate::auth::Session, Response> {
    authenticate(db, headers)
        .await
        .map_err(|error| error.to_response(REQUEST_ID))
}

fn guard_write(session: &crate::auth::Session, headers: &HeaderMap) -> Result<(), Response> {
    csrf::verify_origin(headers).map_err(|error| error.to_response(REQUEST_ID))?;
    csrf::verify_token(session, headers).map_err(|error| error.to_response(REQUEST_ID))?;
    Ok(())
}

fn idempotency_key(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get(IDEMPOTENCY_HEADER)
        .and_then(|value| value.to_str().ok())?
        .trim();
    if raw.is_empty() || raw.chars().count() > MAX_IDEMPOTENCY_KEY_CHARS {
        return None;
    }
    Some(raw.to_owned())
}

async fn lookup_dedup(
    db: &Db,
    user_id: i64,
    key: &str,
    request_hash: &[u8],
) -> Result<Option<String>, Response> {
    let rows = db
        .query_rows(
            "SELECT request_hash, response_json,
                    (expires_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) AS expired
             FROM request_dedup WHERE user_id = ? AND key = ?",
            vec![Value::Integer(user_id), Value::Text(key.to_owned())],
        )
        .await
        .map_err(db_error)?;
    let Some(mut row) = rows.into_iter().next() else {
        return Ok(None);
    };
    let mut cells = row.drain(..);
    let (Some(Value::Blob(stored)), Some(Value::Text(response_json)), expired) =
        (cells.next(), cells.next(), cells.next())
    else {
        return Ok(None);
    };
    if expired.as_ref().and_then(|value| value.as_i64()) == Some(1) {
        return Ok(None);
    }
    if stored != request_hash {
        return Err(json_error(
            StatusCode::CONFLICT,
            "IDEMPOTENCY_CONFLICT",
            "相同幂等键对应了不同的请求载荷",
            REQUEST_ID,
        ));
    }
    Ok(Some(response_json))
}

async fn scan_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let Some(db) = state.db.clone() else {
        return not_ready();
    };
    if let Err(response) = authenticated(&db, &headers).await {
        return response;
    }
    let Ok(job_id) = id.trim().parse::<i64>() else {
        return json_error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "扫描任务不存在",
            REQUEST_ID,
        );
    };
    let rows = match db
        .query_rows(
            "SELECT root_id, state, force, scanned, updated, failed, started_at, finished_at
             FROM scan_jobs WHERE id = ?",
            vec![Value::Integer(job_id)],
        )
        .await
    {
        Ok(rows) => rows,
        Err(error) => return db_error(error),
    };
    let Some(row) = rows.into_iter().next() else {
        return json_error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "扫描任务不存在",
            REQUEST_ID,
        );
    };
    let field_text = |cell: Option<Value>| match cell {
        Some(Value::Text(text)) => Some(text),
        Some(Value::Integer(i)) => Some(i.to_string()),
        _ => None,
    };
    let errors = match db
        .query_rows(
            "SELECT relative_path, code FROM scan_errors WHERE job_id = ? ORDER BY relative_path, code LIMIT ?",
            vec![Value::Integer(job_id), Value::Integer(101)],
        )
        .await
    {
        Ok(rows) => rows,
        Err(error) => return db_error(error),
    };
    let errors_truncated = errors.len() > 100;
    let errors: Vec<ErrorEntry> = errors
        .into_iter()
        .take(100)
        .map(|mut row| {
            let mut cells = row.drain(..);
            ErrorEntry {
                relative_path: field_text(cells.next()).unwrap_or_default(),
                code: field_text(cells.next()).unwrap_or_default(),
            }
        })
        .collect();
    let mut cells = row.into_iter();
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "id": job_id.to_string(),
            "root_id": field_text(cells.next()),
            "state": field_text(cells.next()),
            "force": matches!(cells.next(), Some(Value::Integer(1))),
            "scanned": cells.next().and_then(|v| v.as_i64()),
            "updated": cells.next().and_then(|v| v.as_i64()),
            "failed": cells.next().and_then(|v| v.as_i64()),
            "started_at": field_text(cells.next()),
            "finished_at": field_text(cells.next()),
            "isolation": state.scans.isolation_summary(),
            "errors": errors,
            "errors_truncated": errors_truncated,
        })),
    )
        .into_response()
}

#[derive(Debug, Serialize)]
struct ErrorEntry {
    relative_path: String,
    code: String,
}

async fn cancel_scan(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let Some(db) = state.db.clone() else {
        return not_ready();
    };
    let session = match authenticated(&db, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = guard_write(&session, &headers) {
        return response;
    }
    let Ok(job_id) = id.trim().parse::<i64>() else {
        return json_error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "扫描任务不存在",
            REQUEST_ID,
        );
    };
    let in_flight = state.scans.cancel(job_id);
    let rows = match db
        .query_rows(
            "SELECT state FROM scan_jobs WHERE id = ?",
            vec![Value::Integer(job_id)],
        )
        .await
    {
        Ok(rows) => rows,
        Err(error) => return db_error(error),
    };
    let Some(state_text) = rows
        .first()
        .and_then(|row| row.first())
        .and_then(Value::as_text)
        .map(str::to_owned)
    else {
        return json_error(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "扫描任务不存在",
            REQUEST_ID,
        );
    };
    // 幂等：已结束的任务再取消不报错，只回当前状态。
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "id": job_id.to_string(),
            "state": state_text,
            "cancelling": in_flight,
        })),
    )
        .into_response()
}

fn bad_request(message: &str) -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        "INVALID_REQUEST",
        message,
        REQUEST_ID,
    )
}

fn not_ready() -> Response {
    json_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "NOT_READY",
        "服务尚未就绪",
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
