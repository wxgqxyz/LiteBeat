use std::path::PathBuf;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter("litebeat=info,tower_http=info")
        .with_target(false)
        .init();

    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref() {
        Some(command) if command == "serve" => {
            let flag = args.next();
            let path = args.next();
            if flag.as_deref() != Some(std::ffi::OsStr::new("--config")) {
                return Err("usage: litebeat serve --config <file>".into());
            }
            let config_path = path.map(PathBuf::from).ok_or("missing config file")?;
            litebeat::serve(config_path).await
        }
        _ => Err("usage: litebeat serve --config <file>".into()),
    }
}
