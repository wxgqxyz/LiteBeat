//! SQLite 访问层：2 个只读线程 + 1 个写线程，各自独占一条连接；
//! 命令经有界队列提交，队满立即返回可重试的忙碌错误。

pub mod migrate;
mod worker;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use rusqlite::types::Value as SqliteValue;
use tokio::sync::oneshot;

pub use migrate::{MIGRATIONS, SUPPORTED_VERSION};

/// 与 HTTP 层交换的数据库值；ID 与文件大小在 JSON 侧转成十进制字符串。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    fn from_sqlite(value: SqliteValue) -> Self {
        match value {
            SqliteValue::Null => Value::Null,
            SqliteValue::Integer(i) => Value::Integer(i),
            SqliteValue::Real(f) => Value::Real(f),
            SqliteValue::Text(s) => Value::Text(s),
            SqliteValue::Blob(b) => Value::Blob(b),
        }
    }

    fn into_sqlite(self) -> SqliteValue {
        match self {
            Value::Null => SqliteValue::Null,
            Value::Integer(i) => SqliteValue::Integer(i),
            Value::Real(f) => SqliteValue::Real(f),
            Value::Text(s) => SqliteValue::Text(s),
            Value::Blob(b) => SqliteValue::Blob(b),
        }
    }

    fn payload_bytes(&self) -> usize {
        match self {
            Value::Null => 0,
            Value::Integer(_) | Value::Real(_) => 8,
            Value::Text(s) => s.len(),
            Value::Blob(b) => b.len(),
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Integer(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Reply {
    pub rows: Option<Vec<Vec<Value>>>,
    pub affected: u64,
    pub last_rowid: i64,
}

type Exec = Box<dyn FnOnce(&Connection) -> Result<Reply, rusqlite::Error> + Send>;

pub(crate) struct Command {
    payload_bytes: usize,
    exec: Exec,
    reply: oneshot::Sender<Result<Reply, DbError>>,
}

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("数据库队列已满，请稍后重试")]
    QueueFull,
    #[error("写命令超出排队载荷上限，拒绝入队")]
    PayloadTooLarge,
    #[error("查询超过执行预算被中断")]
    QueryTimeout,
    #[error("数据库已关闭")]
    Closed,
    #[error("SQLite 错误: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("迁移或启动检查失败: {0}")]
    Migration(String),
}

#[derive(Debug, Clone)]
pub struct DbOptions {
    pub read_threads: usize,
    pub read_queue_capacity: usize,
    pub write_queue_capacity: usize,
    pub write_payload_limit: usize,
    pub read_budget: Duration,
}

impl Default for DbOptions {
    fn default() -> Self {
        Self {
            read_threads: 2,
            read_queue_capacity: 32,
            write_queue_capacity: 64,
            // 架构约定：写队列合计排队载荷不超过 4 MiB。
            write_payload_limit: 4 * 1024 * 1024,
            read_budget: Duration::from_millis(200),
        }
    }
}

pub struct Db {
    read_txs: Mutex<Vec<Option<SyncSender<Command>>>>,
    next_read: AtomicUsize,
    write_tx: Mutex<Option<SyncSender<Command>>>,
    /// 当前写队列累计排队载荷；写线程出队时扣减。
    write_payload: Arc<AtomicUsize>,
    write_payload_limit: usize,
    handles: Mutex<Vec<std::thread::JoinHandle<()>>>,
    path: PathBuf,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("path", &self.path)
            .field("write_payload", &self.write_payload.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

fn stmt_payload(sql: &str, params: &[Value]) -> usize {
    sql.len() + params.iter().map(Value::payload_bytes).sum::<usize>()
}

impl Db {
    /// 打开（必要时创建）数据库，启动工作线程并在写线程内完成迁移。
    /// 任一连接 PRAGMA 核对失败或迁移失败都会返回错误，调用方不得进入 ready。
    pub fn open(path: impl AsRef<Path>, options: DbOptions) -> Result<Arc<Db>, DbError> {
        let path: Arc<Path> = Arc::from(path.as_ref());
        let mut queues = Vec::new();
        let mut read_setups = Vec::new();
        let mut handles = Vec::new();
        for _ in 0..options.read_threads {
            let (tx, rx) = mpsc::sync_channel(options.read_queue_capacity);
            let (setup_tx, setup_rx) = mpsc::channel();
            let thread_path = Arc::clone(&path);
            let budget = options.read_budget;
            let handle = std::thread::Builder::new()
                .name("db-read".into())
                .spawn(move || worker::read_loop(thread_path, rx, setup_tx, budget))
                .map_err(|e| DbError::Migration(format!("启动读线程: {e}")))?;
            queues.push(tx);
            read_setups.push(setup_rx);
            handles.push(handle);
        }
        for setup_rx in read_setups {
            setup_rx
                .recv()
                .map_err(|_| DbError::Closed)?
                .map_err(DbError::Migration)?;
        }

        let (write_tx, write_rx) = mpsc::sync_channel(options.write_queue_capacity);
        let (setup_tx, setup_rx) = mpsc::channel();
        let write_payload = Arc::new(AtomicUsize::new(0));
        let thread_path = Arc::clone(&path);
        let payload_for_thread = Arc::clone(&write_payload);
        handles.push(
            std::thread::Builder::new()
                .name("db-write".into())
                .spawn(move || {
                    worker::write_loop(thread_path, write_rx, setup_tx, payload_for_thread)
                })
                .map_err(|e| DbError::Migration(format!("启动写线程: {e}")))?,
        );
        // 写线程在线程内完成 PRAGMA 核对与迁移；失败时关闭全部线程再返回错误。
        let setup_result = setup_rx.recv().map_err(|_| DbError::Closed);
        let db = Arc::new(Db {
            read_txs: Mutex::new(queues.into_iter().map(Some).collect()),
            next_read: AtomicUsize::new(0),
            write_tx: Mutex::new(Some(write_tx)),
            write_payload,
            write_payload_limit: options.write_payload_limit,
            handles: Mutex::new(handles),
            path: path.to_path_buf(),
        });
        match setup_result {
            Err(error) => {
                db.close_senders();
                Self::join_handles(&db);
                Err(error)
            }
            Ok(Err(message)) => {
                db.close_senders();
                Self::join_handles(&db);
                Err(DbError::Migration(message))
            }
            Ok(Ok(())) => Ok(db),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn send_read(&self, exec: Exec) -> Result<oneshot::Receiver<Result<Reply, DbError>>, DbError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let mut pending = Some(Command {
            payload_bytes: 0,
            exec,
            reply: reply_tx,
        });
        let guard = self.read_txs.lock().unwrap();
        let count = guard.len();
        if count == 0 {
            return Err(DbError::Closed);
        }
        let start = self.next_read.fetch_add(1, Ordering::Relaxed) % count;
        for offset in 0..count {
            let command = pending.take().expect("loop holds one command");
            let index = (start + offset) % count;
            match guard[index].as_ref() {
                Some(tx) => match tx.try_send(command) {
                    Ok(()) => return Ok(reply_rx),
                    Err(mpsc::TrySendError::Full(c)) => pending = Some(c),
                    Err(mpsc::TrySendError::Disconnected(c)) => pending = Some(c),
                },
                None => pending = Some(command),
            }
        }
        Err(DbError::QueueFull)
    }

    fn send_write(
        &self,
        payload_bytes: usize,
        exec: Exec,
    ) -> Result<oneshot::Receiver<Result<Reply, DbError>>, DbError> {
        if payload_bytes > self.write_payload_limit {
            return Err(DbError::PayloadTooLarge);
        }
        loop {
            let current = self.write_payload.load(Ordering::Acquire);
            if current + payload_bytes > self.write_payload_limit {
                return Err(DbError::PayloadTooLarge);
            }
            if self
                .write_payload
                .compare_exchange_weak(
                    current,
                    current + payload_bytes,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                break;
            }
        }
        match self.try_send_write(payload_bytes, exec) {
            Ok(rx) => Ok(rx),
            Err(error) => {
                self.write_payload
                    .fetch_sub(payload_bytes, Ordering::AcqRel);
                Err(error)
            }
        }
    }

    fn try_send_write(
        &self,
        payload_bytes: usize,
        exec: Exec,
    ) -> Result<oneshot::Receiver<Result<Reply, DbError>>, DbError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let command = Command {
            payload_bytes,
            exec,
            reply: reply_tx,
        };
        let mut guard = self.write_tx.lock().unwrap();
        match guard.as_mut() {
            Some(tx) => match tx.try_send(command) {
                Ok(()) => Ok(reply_rx),
                Err(mpsc::TrySendError::Full(_)) => Err(DbError::QueueFull),
                Err(mpsc::TrySendError::Disconnected(_)) => Err(DbError::Closed),
            },
            None => Err(DbError::Closed),
        }
    }

    /// 参数化只读查询；超过 `read_budget` 由 progress handler 中断为
    /// `QueryTimeout`，HTTP 层据此返回错误而不是留下失控 SQL。
    pub async fn query_rows(
        &self,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<Vec<Vec<Value>>, DbError> {
        let sql = sql.to_string();
        let sqlite_params: Vec<SqliteValue> = params.into_iter().map(Value::into_sqlite).collect();
        let reply = self
            .send_read(Box::new(move |conn| {
                worker::rows_on(conn, &sql, &sqlite_params)
            }))
            .unwrap_or_else(quick_reply)
            .await
            .map_err(|_| DbError::Closed)??;
        Ok(reply.rows.unwrap_or_default())
    }

    /// 在写连接上执行查询（用于 PRAGMA 核对等必须落在写线程的操作）。
    pub async fn query_rows_on_write(
        &self,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<Vec<Vec<Value>>, DbError> {
        let payload = stmt_payload(sql, &params);
        let sql = sql.to_string();
        let sqlite_params: Vec<SqliteValue> = params.into_iter().map(Value::into_sqlite).collect();
        let reply = self
            .send_write(
                payload,
                Box::new(move |conn| worker::rows_on(conn, &sql, &sqlite_params)),
            )
            .unwrap_or_else(quick_reply)
            .await
            .map_err(|_| DbError::Closed)??;
        Ok(reply.rows.unwrap_or_default())
    }

    pub async fn execute(&self, sql: &str, params: Vec<Value>) -> Result<Reply, DbError> {
        let payload = stmt_payload(sql, &params);
        let sql = sql.to_string();
        let sqlite_params: Vec<SqliteValue> = params.into_iter().map(Value::into_sqlite).collect();
        self.send_write(
            payload,
            Box::new(move |conn| worker::execute_on(conn, &sql, &sqlite_params)),
        )
        .unwrap_or_else(quick_reply)
        .await
        .map_err(|_| DbError::Closed)?
    }

    /// 单事务批量写入：要么全部提交，要么全部回滚（含 FTS 触发器效果）。
    pub async fn transaction(
        &self,
        statements: Vec<(String, Vec<Value>)>,
    ) -> Result<Reply, DbError> {
        let payload = statements
            .iter()
            .map(|(sql, params)| stmt_payload(sql, params))
            .sum();
        self.send_write(
            payload,
            Box::new(move |conn| {
                let sqlite_statements: Vec<(String, Vec<SqliteValue>)> = statements
                    .into_iter()
                    .map(|(sql, params)| {
                        (
                            sql,
                            params
                                .into_iter()
                                .map(Value::into_sqlite)
                                .collect::<Vec<_>>(),
                        )
                    })
                    .collect();
                worker::transaction_on(conn, &sqlite_statements)
            }),
        )
        .unwrap_or_else(quick_reply)
        .await
        .map_err(|_| DbError::Closed)?
    }

    /// 供测试与协作取消使用：直接提交自定义读闭包。
    pub fn submit_read<F>(&self, f: F) -> Result<oneshot::Receiver<Result<Reply, DbError>>, DbError>
    where
        F: FnOnce(&Connection) -> Result<Reply, rusqlite::Error> + Send + 'static,
    {
        self.send_read(Box::new(f))
    }

    /// 供测试与特殊写任务使用：直接提交自定义写闭包并声明其排队载荷。
    pub fn submit_write<F>(
        &self,
        payload_bytes: usize,
        f: F,
    ) -> Result<oneshot::Receiver<Result<Reply, DbError>>, DbError>
    where
        F: FnOnce(&Connection) -> Result<Reply, rusqlite::Error> + Send + 'static,
    {
        self.send_write(payload_bytes, Box::new(f))
    }

    /// 主动关闭：丢弃队列句柄让工作线程退出。不再接受新命令。
    pub fn shutdown(&self) {
        self.close_senders();
    }

    /// 关闭并等待全部工作线程退出。
    pub fn close(self: &Arc<Self>) {
        self.close_senders();
        Self::join_handles(self);
    }

    fn close_senders(&self) {
        let mut read_guard = self.read_txs.lock().unwrap();
        for slot in read_guard.iter_mut() {
            *slot = None;
        }
        drop(read_guard);
        *self.write_tx.lock().unwrap() = None;
    }

    fn join_handles(&self) {
        let mut handles = self.handles.lock().unwrap();
        for handle in handles.drain(..) {
            let _ = handle.join();
        }
    }
}

/// 入队失败的同步回执：让调用方统一 `await` 一个 Receiver。
fn quick_reply(error: DbError) -> oneshot::Receiver<Result<Reply, DbError>> {
    let (tx, rx) = oneshot::channel();
    let _ = tx.send(Err(error));
    rx
}

impl Drop for Db {
    fn drop(&mut self) {
        self.close_senders();
    }
}
