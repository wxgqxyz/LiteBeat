//! SQLite 迁移：版本保存在 `user_version` 中，每个迁移在自己的事务里执行，
//! 中途失败只回滚该迁移，不会留下半套 schema。

use std::fmt;

use rusqlite::Connection;

use super::DbError;

/// 当前程序支持的最低与最高 schema 版本。
pub const SUPPORTED_VERSION: i32 = 2;

#[derive(Debug)]
pub struct Migration {
    pub version: i32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// 已应用的迁移会保持顺序；新增迁移只能追加，不能修改历史 SQL。
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "001_core",
        sql: include_str!("../../migrations/001_core.sql"),
    },
    Migration {
        version: 2,
        name: "002_library_indexes",
        sql: include_str!("../../migrations/002_library_indexes.sql"),
    },
];

pub fn current_version(conn: &Connection) -> Result<i32, DbError> {
    let version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(version)
}

/// 把数据库升级到 `SUPPORTED_VERSION`。版本高于程序支持范围时拒绝继续，
/// 让调用方（服务启动或 `db migrate`）给出恢复指导，而不是静默降级。
pub fn migrate(conn: &Connection) -> Result<(), DbError> {
    let start = current_version(conn)?;
    if start > SUPPORTED_VERSION {
        return Err(DbError::Migration(format!(
            "数据库 schema 版本为 {start}，高于当前程序支持的 {SUPPORTED_VERSION}；\
             请升级 LiteBeat 或用备份恢复到与程序匹配的版本，不要直接覆盖旧可执行文件"
        )));
    }
    if start == SUPPORTED_VERSION {
        return Ok(());
    }
    // 既可以是全新库（0），也可以是历史已应用版本（例如 v1 库补跑到 v2）；
    // 只有 user_version 不属于任何迁移收尾版本时才拒绝，避免误伤升级路径。
    if start != 0 && !MIGRATIONS.iter().any(|m| m.version == start) {
        return Err(DbError::Migration(format!(
            "无法识别的数据库 schema 版本 {start}（程序支持 0 或 {SUPPORTED_VERSION}）"
        )));
    }
    for migration in MIGRATIONS {
        if migration.version <= start {
            continue;
        }
        let tx = conn.unchecked_transaction().map_err(DbError::Sql)?;
        tx.execute_batch(migration.sql)
            .map_err(|error| DbError::Migration(format!("{}: {error}", migration.name)))?;
        // user_version 是事务性 PRAGMA，随本批迁移一起提交或回滚。
        tx.execute_batch(&format!("PRAGMA user_version = {};", migration.version))
            .map_err(DbError::Sql)?;
        tx.commit().map_err(DbError::Sql)?;
    }
    Ok(())
}

impl fmt::Display for Migration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{} {}", self.version, self.name)
    }
}
