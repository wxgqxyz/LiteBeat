pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod http;

use std::{net::SocketAddr, path::Path, sync::Arc, sync::atomic::AtomicBool};

use tokio::net::TcpListener;

pub async fn serve(
    config_path: impl AsRef<Path>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = config::Config::load(config_path)?;
    tokio::fs::create_dir_all(&config.storage.data_dir).await?;
    let address: SocketAddr = config.validate()?;
    // 迁移失败或数据库版本过新时在这里终止启动，服务不会进入 ready。
    let db_path = config.storage.data_dir.join("litebeat.db");
    let db = tokio::task::spawn_blocking(move || db::Db::open(db_path, db::DbOptions::default()))
        .await??;
    let listener = TcpListener::bind(address).await?;
    tracing::info!(%address, "LiteBeat listening");
    axum::serve(
        listener,
        http::router(http::AppState {
            web_dir: config.server.web_dir,
            ready: Arc::new(AtomicBool::new(true)),
            db: Some(Arc::clone(&db)),
        }),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    db.close();
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}
