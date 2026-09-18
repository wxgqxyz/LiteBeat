use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use litebeat::auth::password::hash_password;
use litebeat::auth::routes::MAX_KEY_CHARS;
use litebeat::db::{Db, DbError, DbOptions, Value};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), BoxError> {
    tracing_subscriber::fmt()
        .with_env_filter("litebeat=info,tower_http=info")
        .with_target(false)
        .init();

    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref() {
        Some(command) if command == "serve" => {
            let config_path = config_flag(&mut args)?;
            litebeat::serve(config_path).await
        }
        Some(command) if command == "db" => {
            let sub = args.next();
            if sub.as_deref() != Some(std::ffi::OsStr::new("migrate")) {
                return Err("usage: litebeat db migrate --config <file>".into());
            }
            let config_path = config_flag(&mut args)?;
            let config = litebeat::config::Config::load(config_path)?;
            std::fs::create_dir_all(&config.storage.data_dir)?;
            let db_path = config.storage.data_dir.join("litebeat.db");
            let db = Db::open(&db_path, DbOptions::default())?;
            let rows = db
                .query_rows_on_write("PRAGMA user_version", vec![])
                .await?;
            let version = rows
                .first()
                .and_then(|r| r.first())
                .and_then(|v| v.as_i64())
                .unwrap_or(-1);
            println!(
                "数据库 {} schema 版本 {version}（当前程序支持到 {}）",
                db_path.display(),
                litebeat::db::SUPPORTED_VERSION
            );
            db.close();
            Ok(())
        }
        Some(command) if command == "admin" => {
            let sub = args.next();
            if sub.as_deref() != Some(std::ffi::OsStr::new("create")) {
                return Err("usage: litebeat admin create --config <file>".into());
            }
            let config_path = config_flag(&mut args)?;
            admin_create(&config_path).await
        }
        _ => Err(
            "usage: litebeat serve --config <file> | litebeat db migrate --config <file> | litebeat admin create --config <file>".into(),
        ),
    }
}

/// 建立管理员账号。口令只从终端读取且不回显，绝不作为命令行参数出现。
async fn admin_create(config_path: &Path) -> Result<(), BoxError> {
    let config = litebeat::config::Config::load(config_path)?;
    std::fs::create_dir_all(&config.storage.data_dir)?;
    let db_path = config.storage.data_dir.join("litebeat.db");
    // Db::open 内含迁移，因此新建 data 目录时无需先跑 db migrate。
    let db = Db::open(&db_path, DbOptions::default())?;
    let outcome = create_admin(&db).await;
    db.close();
    outcome
}

async fn create_admin(db: &Arc<Db>) -> Result<(), BoxError> {
    let username = prompt("用户名: ", read_line)?;
    if username.is_empty() {
        return Err("用户名不能为空".into());
    }
    if username.chars().count() > MAX_KEY_CHARS {
        return Err(format!("用户名长度不得超过 {MAX_KEY_CHARS} 字符").into());
    }
    if username.chars().any(char::is_control) {
        return Err("用户名不能包含控制字符".into());
    }
    let password = read_secret("口令（输入不显示）: ")?;
    if password.trim().is_empty() {
        return Err("口令不能为空".into());
    }
    let existing = db
        .query_rows(
            "SELECT id FROM users WHERE username = ?",
            vec![Value::Text(username.clone())],
        )
        .await?;
    if !existing.is_empty() {
        return Err(format!("用户名 {username} 已存在，未创建管理员").into());
    }
    // Argon2id 需要 32 MiB，放到阻塞线程里跑，避免占住运行时线程。
    let hash = tokio::task::spawn_blocking(move || hash_password(&password))
        .await?
        .map_err(|error| -> BoxError { format!("口令哈希失败: {error}").into() })?;
    match db
        .execute(
            "INSERT INTO users(username, password_hash) VALUES (?, ?)",
            vec![Value::Text(username.clone()), Value::Text(hash)],
        )
        .await
    {
        Ok(reply) => {
            println!("已创建管理员 {username}（id {}）", reply.last_rowid);
            Ok(())
        }
        // 用户名唯一约束兜住并发创建；此时不回显任何口令信息。
        Err(DbError::Sql(rusqlite::Error::SqliteFailure(code, _)))
            if code.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Err(format!("用户名 {username} 已存在，未创建管理员").into())
        }
        Err(error) => Err(error.into()),
    }
}

fn prompt(label: &str, read: fn() -> Result<String, BoxError>) -> Result<String, BoxError> {
    print!("{label}");
    io::stdout().flush()?;
    Ok(read()?.trim().to_owned())
}

fn read_line() -> Result<String, BoxError> {
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line)
}

/// 标准输入是终端时用 rpassword 免回显读取；管道或 CI 场景退回明文 stdin 读取并告警。
fn read_secret(label: &str) -> Result<String, BoxError> {
    if !io::stdin().is_terminal() {
        eprintln!("提示: 标准输入不是终端，改为明文输入。");
        return prompt(label, read_line);
    }
    Ok(rpassword::prompt_password(label)?
        .trim_end_matches(['\r', '\n'])
        .to_owned())
}

fn config_flag(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let flag = args.next();
    let path = args.next();
    if flag.as_deref() != Some(std::ffi::OsStr::new("--config")) {
        return Err("missing --config <file>".into());
    }
    Ok(path.map(PathBuf::from).ok_or("missing config file")?)
}
