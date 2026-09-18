//! 数据库工作线程：每个线程独占一条 SQLite 连接，命令通过有界队列进入。

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::types::Value as SqliteValue;
use rusqlite::{Connection, ErrorCode};

use super::Value;
use super::migrate;
use super::{Command, DbError, Reply};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn apply_pragma(conn: &Connection, name: &str, value: &str) -> Result<String, DbError> {
    // 仅用于固定内部常量，不拼接用户输入。
    conn.execute_batch(&format!("PRAGMA {name} = {value};"))
        .map_err(|e| DbError::Migration(format!("PRAGMA {name}: {e}")))?;
    if name.eq_ignore_ascii_case("journal_mode") {
        return conn
            .query_row(&format!("PRAGMA {name}"), [], |row| row.get::<_, String>(0))
            .map_err(|e| DbError::Migration(format!("读取 PRAGMA {name}: {e}")));
    }
    let actual: i64 = conn
        .query_row(&format!("PRAGMA {name}"), [], |row| row.get::<_, i64>(0))
        .map_err(|e| DbError::Migration(format!("读取 PRAGMA {name}: {e}")))?;
    Ok(actual.to_string())
}

fn check_pragma(conn: &Connection, name: &str, value: &str, expect: &str) -> Result<(), DbError> {
    let actual = apply_pragma(conn, name, value)?;
    if !actual.eq_ignore_ascii_case(expect) {
        return Err(DbError::Migration(format!(
            "PRAGMA {name} 期望 {expect}，实际 {actual}"
        )));
    }
    Ok(())
}

/// 每个连接都要应用并读回核对的 PRAGMA；`cache_size=-2048` 即约 2 MiB 页缓存。
fn configure_common(conn: &Connection) -> Result<(), DbError> {
    check_pragma(conn, "foreign_keys", "ON", "1")?;
    check_pragma(conn, "cache_size", "-2048", "-2048")?;
    check_pragma(conn, "mmap_size", "0", "0")?;
    check_pragma(conn, "temp_store", "FILE", "1")?;
    check_pragma(conn, "busy_timeout", "1000", "1000")?;
    Ok(())
}

fn configure_write(conn: &Connection) -> Result<(), DbError> {
    check_pragma(conn, "journal_mode", "WAL", "wal")?;
    configure_common(conn)?;
    check_pragma(conn, "synchronous", "FULL", "2")?;
    check_pragma(conn, "wal_autocheckpoint", "1000", "1000")?;
    Ok(())
}

/// progress handler 触发的中断映射为可识别的超时，而不是笼统的 SQL 错误。
fn map_sqlite_error(err: rusqlite::Error) -> DbError {
    if let rusqlite::Error::SqliteFailure(code, _) = &err
        && code.code == ErrorCode::OperationInterrupted
    {
        return DbError::QueryTimeout;
    }
    DbError::Sql(err)
}

/// 只读线程：progress handler 实现每次查询的执行预算。
pub(crate) fn read_loop(
    path: Arc<Path>,
    rx: mpsc::Receiver<Command>,
    setup: mpsc::Sender<Result<(), String>>,
    budget: Duration,
) {
    let conn = match Connection::open(&*path) {
        Ok(conn) => conn,
        Err(error) => {
            let _ = setup.send(Err(format!("打开数据库: {error}")));
            return;
        }
    };
    if let Err(error) = configure_common(&conn) {
        let _ = setup.send(Err(error.to_string()));
        return;
    }
    let deadline = Arc::new(AtomicI64::new(0));
    let handler_deadline = Arc::clone(&deadline);
    conn.progress_handler(
        256,
        Some(move || {
            let target = handler_deadline.load(Ordering::Relaxed);
            target != 0 && now_ms() >= target
        }),
    );
    let _ = setup.send(Ok(()));
    let budget_ms = budget.as_millis() as i64;
    while let Ok(cmd) = rx.recv() {
        if cmd.reply.is_closed() {
            // 客户端已取消且尚未开始执行的只读任务直接丢弃。
            continue;
        }
        deadline.store(now_ms() + budget_ms, Ordering::Relaxed);
        let result = (cmd.exec)(&conn).map_err(map_sqlite_error);
        deadline.store(0, Ordering::Relaxed);
        let _ = cmd.reply.send(result);
    }
}

/// 写线程：先完成 PRAGMA 核对与迁移，再进入命令循环；
/// 写事务不使用进度中断， begun 的事务必须完整提交或回滚。
pub(crate) fn write_loop(
    path: Arc<Path>,
    rx: mpsc::Receiver<Command>,
    setup: mpsc::Sender<Result<(), String>>,
    queued_payload: Arc<AtomicUsize>,
) {
    let conn = match Connection::open(&*path) {
        Ok(conn) => conn,
        Err(error) => {
            let _ = setup.send(Err(format!("打开数据库: {error}")));
            return;
        }
    };
    if let Err(error) = configure_write(&conn).and_then(|()| migrate::migrate(&conn)) {
        let _ = setup.send(Err(error.to_string()));
        return;
    }
    let _ = setup.send(Ok(()));
    while let Ok(cmd) = rx.recv() {
        queued_payload.fetch_sub(cmd.payload_bytes, Ordering::AcqRel);
        if cmd.reply.is_closed() {
            continue;
        }
        let result = (cmd.exec)(&conn).map_err(map_sqlite_error);
        let _ = cmd.reply.send(result);
    }
}

/// 在给定连接上执行参数化查询并返回行。
pub(crate) fn rows_on(
    conn: &Connection,
    sql: &str,
    params: &[SqliteValue],
) -> Result<Reply, rusqlite::Error> {
    let mut stmt = conn.prepare(sql)?;
    let columns = stmt.column_count();
    let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut values = Vec::with_capacity(columns);
        for index in 0..columns {
            let value: SqliteValue = row.get(index)?;
            values.push(Value::from_sqlite(value));
        }
        out.push(values);
    }
    Ok(Reply {
        rows: Some(out),
        affected: 0,
        last_rowid: 0,
    })
}

pub(crate) fn execute_on(
    conn: &Connection,
    sql: &str,
    params: &[SqliteValue],
) -> Result<Reply, rusqlite::Error> {
    let affected = conn.execute(sql, rusqlite::params_from_iter(params.iter()))?;
    Ok(Reply {
        rows: None,
        affected: affected as u64,
        last_rowid: conn.last_insert_rowid(),
    })
}

pub(crate) fn transaction_on(
    conn: &Connection,
    statements: &[(String, Vec<SqliteValue>)],
) -> Result<Reply, rusqlite::Error> {
    let tx = conn.unchecked_transaction()?;
    let mut affected_total = 0u64;
    for (sql, params) in statements {
        affected_total += tx.execute(sql, rusqlite::params_from_iter(params.iter()))? as u64;
    }
    let last_rowid = tx.last_insert_rowid();
    tx.commit()?;
    Ok(Reply {
        rows: None,
        affected: affected_total,
        last_rowid,
    })
}
