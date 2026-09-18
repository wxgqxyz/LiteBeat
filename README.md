# LiteBeat

可以部署在个人电脑、小型服务器或 NAS 上的轻量自托管音乐 Web 平台。在浏览器中管理和播放你自己的音乐库，桌面与移动共用一个响应式前端。

## 状态

当前完成 **T01 工程基线**、**T02 数据库层**（SQLite 模式、迁移命令、2 读/1 写有界工作队列）、**T03 登录与会话**（Argon2id、CSRF、限流）、**T04 原文件播放**（完整 HTTP Range 语义、全局/会话流配额、30s 无进展超时）与 **T05 受限扫描**（惰性遍历、增量入库、有界 scan-worker 子进程、admin scans 端点）。曲库检索等按 [实施路线](./03-delivery-and-validation.md) 的 T06–T11 逐步交付。

| 阶段 | 内容 | 状态 |
|---|---|---|
| T01 | 工程基线：服务、健康检查、前端构建 | ✅ 已完成并通过本地验证 |
| T02 | SQLite、迁移与有界工作队列 | ✅ 已完成并通过 12 项契约测试 |
| T03 | 管理员登录与会话 | ✅ 已完成并通过 17 项集成测试 |
| T04 | 原文件播放与 Range 语义 | ✅ 已完成并通过 11 项契约测试（含浏览器播放冒烟） |
| T05 | 受限扫描与增量入库 | ✅ 已完成并通过 13 项集成测试（Linux cgroup 分支待真机复验） |
| T06–T11 | 曲库、播放器、歌单、性能、发布 | 计划中 |

## 规划文档

- [产品定位与前端体验](./01-product-and-experience.md)
- [技术架构与性能设计](./02-architecture-and-performance.md)
- [实施路线与验收计划](./03-delivery-and-validation.md)

## 技术栈

Rust + Axum + Tokio + SQLite（rusqlite, FTS5）｜Svelte 5 + TypeScript + Vite｜单进程部署，无 Redis / 独立搜索服务 / 实时转码。

## 快速开始

```powershell
# 后端（需要 Rust stable + MSVC 工具链）
cargo run -p litebeat -- serve --config config/litebeat.example.toml

# 前端构建
npm --prefix web ci
npm --prefix web run build
```

访问 `http://127.0.0.1:8090`。配置音乐目录：编辑 `config/litebeat.example.toml` 的 `[[library.roots]]`。

## 验证命令

```powershell
cargo fmt --all -- --check
cargo test -p litebeat --test health --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --release --locked
npm --prefix web run check
npm --prefix web run build
```

## 设计目标（尚未测量）

内存预算：空库 RSS ≤24 MiB，20 路播放混合负载峰值 ≤96 MiB；前端首屏 JS gzip ≤80 KiB；普通列表 API p95 ≤100ms。完整指标与测量口径见规划文档，所有数字在真实实现与对照测试前仅为验收目标。

## 许可

MIT
