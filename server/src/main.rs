use std::path::PathBuf;

use litebeat::db::{Db, DbOptions};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
        _ => Err(
            "usage: litebeat serve --config <file> | litebeat db migrate --config <file>".into(),
        ),
    }
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
