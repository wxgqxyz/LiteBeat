# T01 工程基线

## 启动

```powershell
cargo run -p litebeat -- serve --config config/litebeat.example.toml
```

配置中的 `library.roots` 必须指向已经存在的音乐目录。服务默认监听 `127.0.0.1:8090`，生产静态资源来自 `web/dist`。

## 验证

```powershell
cargo fmt --all -- --check
cargo test -p litebeat --test health
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --release --locked
npm --prefix web ci
npm --prefix web run check
npm --prefix web run build
```

Rust 验证已在 Windows MSVC 工具链上完成：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo test -p litebeat --test health --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings` 和 `cargo build --release --locked` 均通过。前端验证已完成：`npm run check` 和 `npm run build` 均通过。
